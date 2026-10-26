// SPDX-License-Identifier: MPL-2.0

//! **Koru's Settings tab: the shared surface, and the readings only this
//! process can take.**
//!
//! The rows moved out of `welcome/` into `koru-settings-ui`, which Koru, Koru
//! Terminal and Koru AI all host. What could not move is everything a row
//! *reads about this machine*: the GPU backends `koru-window` offers, the
//! verified voice-model cache, the microphones the worker last saw, the
//! dictation preferences that live in `koru-workspace-host`'s own globals. A
//! surface that took those readings itself would have to link the crates that
//! own them, and then the Terminal could not host it.
//!
//! So this file is the seam, and it is deliberately thin: take the readings,
//! hand them down, drain what the surface asks for, perform it here.
//!
//! **Nothing about how a preference persists changed.** The rows that wrote a
//! process global still write it, and the shell's `patch_settings` — which
//! already runs straight out of the press that changed a setting — still
//! carries it to the authority as a per-field patch. That is what makes the
//! swap provable rather than merely plausible: the same suite that watched
//! `WelcomeView` move the theme watches this move it.

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, scene, theme};
use koru_settings::{
    CloseBehaviorChoice, DictationGesture, DictationInput, DictationLanguage, DictationTier,
};
use koru_settings_ui::{
    AiSlot, RendererRow, Request, Sections, SettingsLinks, SettingsSurface, VoiceFacts,
};
use koru_workspace::Workspace;
use koru_workspace_host::voice_prefs::{self, InputChoice, ModelTier, Requested, VoiceLanguage};
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

mod reminders;
#[cfg(test)]
mod tests;

/// Why there is nothing to configure.
///
/// Two variants and not three: *no model installed* is a state of the **Model**
/// row rather than of the section, because the language and quality pickers are
/// exactly what decides which model a download would fetch.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum VoiceOff {
    /// `KORU_VOICE_DISABLED`. A kill switch rather than a diagnostic.
    Disabled,
    /// This build has no voice runtime for its platform yet (Linux). Nothing
    /// a person sets turns it on, so the line names the platform and not the
    /// switch.
    Platform,
    /// A dictation was refused because the worker would not start. Reported by
    /// the container out of `View::dictation_refused`, because nothing may
    /// *ask* a worker to start in order to draw a settings page.
    WorkerUnavailable,
}

impl VoiceOff {
    fn line(self) -> &'static str {
        match self {
            Self::Disabled => {
                "Voice input is switched off on this installation (KORU_VOICE_DISABLED)."
            }
            Self::WorkerUnavailable => {
                "Voice input is unavailable: the voice worker would not start."
            }
            Self::Platform => {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::VoiceNotOnLinuxYet)
            }
        }
    }
}

pub struct SettingsView {
    surface: SettingsSurface,
    /// The shared settings crate owns its typography engine; this second one
    /// belongs only to Koru's host section. It keeps reminder input and layout
    /// state out of the reusable surface without duplicating scroll authority.
    reminder_engine: TextEngine,
    reminders: reminders::ReminderSurface,
    off: Option<VoiceOff>,
    /// Set when this page is built and drained by the container. **A view may
    /// not ask a worker anything**: enumerating microphones is a blocking
    /// socket round trip, so the shell does it on a thread.
    wants_voice_devices: bool,
    /// The dictionary download's version when this page last read it, so a
    /// progress step or a finish repaints the row without polling the disk.
    dictionary_version: u64,
    /// `spelling::overlay_version` when the dictionary count was last read.
    overlay_version: u64,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsView {
    pub fn new() -> Self {
        // **Koru's arrangement: the shell keeps the Global link.** It has held
        // one since SP-6b and patches every preference this page moves; a
        // second writer of the same fields would be two windows' worth of race
        // inside one process. The Terminal section has no other reader here, so
        // the surface holds that link itself.
        //
        // Under test it holds neither: a dependency is never compiled with
        // `cfg(test)`, so a guard inside `koru-settings-ui` could not stop this
        // binary's suite from dialling whatever Gateway the machine has running.
        let links = if cfg!(test) {
            SettingsLinks::inert(false, true)
        } else {
            SettingsLinks::terminal_only(
                koru_channel::segment(),
                koru_workspace_host::cli::root_from_args(),
            )
        };
        let mut surface = SettingsSurface::with_links(sections(), links);
        surface.set_lang(crate::format::lang());
        surface.set_renderer(renderer_row());
        surface.set_ai_slot(ai_slot());
        surface.set_telemetry_consent(koru_workspace_host::telemetry_consent::current().toggle());
        // Read the record again as the page opens: another window may have
        // answered since this process last looked, or the Gateway may not have
        // been answering then. Off the event loop; the maintenance pass pushes
        // what comes back.
        koru_workspace_host::telemetry_consent::refresh();
        let mut reminder_engine = TextEngine::new();
        let reminders = reminders::ReminderSurface::new(&mut reminder_engine);
        let mut view = Self {
            surface,
            reminder_engine,
            reminders,
            off: None,
            wants_voice_devices: false,
            dictionary_version: 0,
            overlay_version: 0,
        };
        // **The world, asked once.** The kill switch and a walk of the model
        // cache — neither may happen on a frame, and neither may happen under
        // `cargo test`, where the cache it would walk is the developer's own.
        if !cfg!(test) {
            view.refresh_voice();
            view.wants_voice_devices = true;
            view.reminders.set_system_week_start(
                koru_workspace_host::environment::preferred_first_day_of_week(),
            );
        }
        view.push_voice_preferences();
        view.push_spelling_preferences();
        view
    }

    /// Whether this page still wants to know which microphones there are.
    pub fn take_wants_voice_devices(&mut self) -> bool {
        std::mem::take(&mut self.wants_voice_devices)
    }

    /// What only the host can do. Drained by the container.
    pub fn take_requests(&mut self) -> Vec<Request> {
        self.surface.take_requests()
    }

    /// Opened from the first-minutes guide's mic link.
    pub fn scroll_to_voice(&mut self) -> bool {
        self.surface.scroll_to_voice()
    }

    /// Copy controller-owned reminder state into Koru's host section. This is
    /// presentation only; Store and Agent effects stay in `MainView`.
    pub(crate) fn set_reminder_preferences(
        &mut self,
        model: &crate::reminder_settings::ReminderPreferences,
    ) -> bool {
        self.reminders.set_model(&mut self.reminder_engine, model)
    }

    /// The next typed reminder action for MainView's controller to apply.
    pub(crate) fn take_reminder_intent(&mut self) -> Option<crate::reminder_settings::Intent> {
        self.reminders.take_intent()
    }

    /// Reveal and focus the profile's End of day control after a reminder
    /// expression discovers that the setting is absent.
    pub(crate) fn reveal_reminder_end_of_day(&mut self) -> bool {
        self.surface.show_page(koru_settings_ui::PageId::Reminders);
        self.layout_reminders();
        self.surface.relayout();
        let Some(rect) = self.reminders.focus_end_of_day(&mut self.reminder_engine) else {
            return false;
        };
        self.surface.reveal_content_rect(rect);
        true
    }

    /// Whether the reminder rows are the ones on screen. They are the
    /// Reminders page's, so on any other page — or under an open dropdown
    /// list, which is drawn above them — their last layout is not what a
    /// person sees, and nothing may reach it.
    fn reminders_live(&self) -> bool {
        self.surface.page() == koru_settings_ui::PageId::Reminders && !self.surface.menu_open()
    }

    /// Measure Koru's host-owned section in the shared surface's content
    /// coordinates, then reserve exactly that height in the one scroll model.
    fn layout_reminders(&mut self) {
        self.surface.relayout();
        let Some(slot) = self.surface.host_section_rect() else {
            self.surface.set_host_section_height(0.0);
            return;
        };
        let (vw, vh) = self.surface.viewport();
        self.reminders.set_view(vw, self.surface.content_y(vh));
        let bottom = self.reminders.layout(
            &mut self.reminder_engine,
            slot,
            slot.y,
            crate::format::lang(),
        );
        self.surface
            .set_host_section_height((bottom - slot.y).max(0.0));
    }

    /// The host performed a spelling write, or the tab just opened; the
    /// surface's mirror follows `koru_workspace_host::spelling`'s globals.
    /// The chips are rebuilt with it — Automatic first, then each dictionary
    /// language by endonym — and the D6 sentence appears exactly when
    /// automatic resolves to no dictionary.
    /// This device's telemetry answer, as the shell last read it; `true` when
    /// the switch moved.
    pub fn set_telemetry_consent(&mut self, granted: Option<bool>) -> bool {
        self.surface.set_telemetry_consent(granted)
    }

    pub fn push_spelling_preferences(&mut self) {
        use koru_workspace_host::spelling;
        let ui =
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
        let mut languages = vec![(
            koru_settings::LanguageChoice::Automatic,
            koru_i18n::t(ui, koru_i18n::Key::SpellingAutomatic).to_owned(),
        )];
        languages.extend(spelling::SpellLang::ALL.iter().map(|language| {
            (
                koru_settings::LanguageChoice::Tag(language.tag().to_owned()),
                language.endonym().to_owned(),
            )
        }));
        let enabled = spelling::enabled();
        let language = spelling::preferred()
            .map_or(koru_settings::LanguageChoice::Automatic, |l| {
                koru_settings::LanguageChoice::Tag(l.tag().to_owned())
            });
        let effective = spelling::effective_language();
        let note = if enabled && effective.is_none() {
            Some(koru_i18n::t(ui, koru_i18n::Key::SpellingNotAvailable).to_owned())
        } else {
            None
        };
        let missing = effective.filter(|lang| !spelling::dictionary_files_available(*lang));
        self.surface
            .set_spelling(enabled, language, languages, note);
        self.surface
            .set_dictionary(missing.map(|lang| dictionary_row(ui, lang)));
        self.surface
            .set_personal_dictionary_words(spelling::personal_word_count());
        self.dictionary_version = spelling::download::version();
        self.overlay_version = spelling::overlay_version();
    }

    /// The host performed one of the voice writes; the surface's mirror follows
    /// the globals rather than guessing.
    pub fn push_voice_preferences(&mut self) {
        self.surface.set_voice_preferences(
            DictationLanguage::parse(voice_prefs::language_token(voice_prefs::language())),
            DictationInput::parse(voice_prefs::input_token(&voice_prefs::input())),
            DictationTier::parse(voice_prefs::tier().as_str()),
            if voice_prefs::toggle() {
                DictationGesture::Toggle
            } else {
                DictationGesture::PushToTalk
            },
        );
    }

    /// **Take another reading of the model cache.** Called when the shell says a
    /// model was installed or deleted. The kill switch is re-read with it,
    /// deliberately: asking one predicate rather than caching half the answer is
    /// what keeps the four voice surfaces agreeing (Q-V44).
    pub fn refresh_voice(&mut self) {
        if let Some(off) = koru_dictation::off() {
            let off = match off {
                koru_dictation::Off::Switch => VoiceOff::Disabled,
                koru_dictation::Off::Platform => VoiceOff::Platform,
            };
            self.off = Some(off);
            self.surface.set_voice_facts(VoiceFacts {
                blocked: Some(off.line().to_owned()),
                ..VoiceFacts::default()
            });
            return;
        }
        if matches!(self.off, Some(VoiceOff::Disabled | VoiceOff::Platform)) {
            self.off = None;
        }
        let models = koru_workspace_host::voice_install::installed_models();
        self.surface.set_voice_facts(VoiceFacts {
            blocked: self.off.map(|off| off.line().to_owned()),
            languages: language_options(),
            inputs: input_options(),
            input_note: input_note(),
            tiers: ModelTier::ALL
                .iter()
                .map(|tier| (tier_of(*tier), quality_label(*tier).to_owned()))
                .collect(),
            tier_notes: ModelTier::ALL
                .iter()
                .map(|tier| (tier_of(*tier), quality_note(*tier)))
                .collect(),
            model_line: model_line(&models),
            model_action: if models.complete() {
                "Remove".to_owned()
            } else {
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::SettingsDownload).to_owned()
            },
            model_removes: models.complete(),
            // **Stopped short, and recorded rather than faked.** The revoke row
            // is built and tested in `koru-settings-ui`; drawing it here needs
            // `koru_workspace_host::voice_install::read_consent` (today
            // `pub(crate)`) and a `revoke_consent` beside it, and SP-7 may not
            // change that crate. See the packet report.
            consent_granted: false,
            consent_line: String::new(),
            consent_action: String::new(),
            chord: koru_keys::show_hint(koru_keys::ShortcutId::Dictate, koru_keys::Section::Menu),
        });
    }

    /// **The worker would not start.** Recorded rather than asked, because
    /// asking means starting one. Only two codes mean *there is no worker*;
    /// every other refusal is about one dictation, and clears the flag so a
    /// machine that recovered does not keep an obsolete sentence.
    pub fn note_refusal(&mut self, code: koru_dictation::ErrorCode) -> bool {
        let broken = matches!(
            code,
            koru_dictation::ErrorCode::WorkerUnavailable
                | koru_dictation::ErrorCode::ProtocolMismatch
        );
        let was = self.off;
        self.off = match (self.off, broken) {
            // The kill switch outranks everything: with it set there is no
            // worker to have failed.
            (Some(VoiceOff::Disabled), _) => Some(VoiceOff::Disabled),
            // Nor with no runtime for this platform.
            (Some(VoiceOff::Platform), _) => Some(VoiceOff::Platform),
            (_, true) => Some(VoiceOff::WorkerUnavailable),
            (_, false) => None,
        };
        if was == self.off {
            return false;
        }
        if !cfg!(test) {
            self.refresh_voice();
        }
        true
    }

    /// The host cycled the canvas colour and hands back what it landed on.
    pub fn set_canvas_background(&mut self, background: Option<[u8; 4]>) {
        self.surface.set_canvas_background(background);
    }

    /// **The four values the container owns a copy of, read back.**
    ///
    /// Nothing in the running app asks: `capture` writes them into `Settings`
    /// and the surface is what draws them. They exist for the container's test
    /// that a Settings tab opened an hour into a session starts from what is
    /// stored rather than from the defaults — the failure that made a late tab
    /// write the defaults back over the real values.
    #[cfg(test)]
    pub fn canvas_background(&self) -> Option<[u8; 4]> {
        self.surface.canvas_background()
    }

    #[cfg(test)]
    pub fn view_mode(&self) -> koru_draw::theme::ViewMode {
        self.surface.view_mode()
    }

    #[cfg(test)]
    pub fn pinned_show_title(&self) -> bool {
        self.surface.pinned_show_title()
    }

    #[cfg(test)]
    pub fn close_behavior(&self) -> koru_window::CloseBehavior {
        close_behavior_of(self.surface.close_behavior())
    }

    #[cfg(test)]
    pub(crate) fn reminder_end_of_day_focused(&self) -> bool {
        self.reminders.end_of_day_focused()
    }

    /// Whether the End of day field is on the Reminders page and inside the
    /// visible part of it — what "revealed" means, whatever the scroll it took.
    #[cfg(test)]
    pub(crate) fn end_of_day_on_screen(&self) -> bool {
        let Some(rect) = self.reminders.end_of_day_rect() else {
            return false;
        };
        let top = self.surface.content_y(0.0);
        let (_, h) = self.surface.viewport();
        self.surface.page() == koru_settings_ui::PageId::Reminders
            && rect.y >= top
            && rect.y + rect.h <= top + h
    }

    // **No wake proxy here, deliberately.** `SettingsSurface::on_update` exists
    // for a host that has one; a pane inside Koru does not — the event-loop
    // proxy belongs to the shell. Convergence reaches this tab through
    // `maintenance_deadline` / `maintain` instead, which is a `try_recv` and a
    // clock comparison rather than a wake.

    #[cfg(test)]
    pub(crate) fn surface_mut(&mut self) -> &mut SettingsSurface {
        &mut self.surface
    }
}

impl View for SettingsView {
    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        (self.reminders_live()
            && self
                .reminders
                .key(&mut self.reminder_engine, key, text, mods))
            || self.surface.key(key, text, mods)
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        let content_y = self.surface.content_y(y);
        (self.reminders_live()
            && self
                .reminders
                .press(&mut self.reminder_engine, x, content_y))
            || self.surface.mouse_down(x, y)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let content_y = self.surface.content_y(y);
        let reminder_moved = self.reminders_live()
            && self
                .reminders
                .move_pointer(&mut self.reminder_engine, x, content_y);
        self.surface.mouse_move(x, y) || reminder_moved
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        self.reminders.release() || self.surface.mouse_up()
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let content_y = self.surface.content_y(y);
        self.reminders_live()
            && self
                .reminders
                .double_click(&mut self.reminder_engine, x, content_y)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        self.surface.scroll_by(delta);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.surface.set_viewport(w, h);
    }

    fn cursor_icon(&self) -> CursorIcon {
        if self.reminders_live() && self.reminders.hot() {
            koru_ui::cursor::PRESS
        } else {
            self.surface.cursor_icon()
        }
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn reseat(&mut self, _ws: &WorkspaceDoc) {
        // Settings names no document and no workspace, so a workspace switch
        // invalidates nothing here — which is exactly what the charter says.
    }

    fn invalidate(&mut self) {
        self.surface.invalidate();
        self.reminder_engine.clear_pill_cache();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.layout_reminders();
        self.surface.draw(scene, root);
        let reminders = &mut self.reminders;
        let engine = &mut self.reminder_engine;
        self.surface
            .draw_host_section(scene, root, |scene, root, slot| {
                reminders.draw(scene, root, engine, slot, crate::format::lang());
            });
    }

    fn restore(&mut self, _ws: &Workspace, settings: &Settings) {
        self.adopt_preferences(settings);
        theme::set_caret_color(settings.caret_color);
        theme::set_caret_width(settings.caret_width);
        theme::set_caret_height(settings.caret_height);
    }

    /// **The four values this surface keeps a field of.** Everything else it
    /// draws reads a process global and repaints itself. Without this the next
    /// `capture` would write the stale copy straight back over an authoritative
    /// change — in two windows, forever, each undoing the other.
    fn adopt_preferences(&mut self, settings: &Settings) {
        self.surface.adopt_preferences(
            settings.view_mode,
            settings.canvas_bg,
            settings.pinned_show_title,
            close_behavior_to(settings.close_behavior),
        );
        self.push_voice_preferences();
    }

    fn capture(&self, settings: &mut Settings) {
        settings.view_mode = self.surface.view_mode();
        settings.pinned_show_title = self.surface.pinned_show_title();
        settings.canvas_bg = self.surface.canvas_background();
        settings.caret_color = theme::caret_color();
        settings.caret_width = theme::caret_width();
        settings.caret_height = theme::caret_height();
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        self.surface.maintenance_deadline()
    }

    fn maintain(&mut self, _ws: &mut WorkspaceDoc, _now: std::time::Instant) -> bool {
        // A dictionary download moved (progress, finished, failed): re-read
        // the spelling row. An atomic compare when nothing moved.
        // …or the personal dictionary changed (a word taught, or cleared), so
        // its count does.
        let downloaded = koru_workspace_host::spelling::download::version()
            != self.dictionary_version
            || koru_workspace_host::spelling::overlay_version() != self.overlay_version;
        if downloaded {
            self.push_spelling_preferences();
        }
        // The Terminal section's link, and the liveness the read-only state
        // reads off it. Never blocks: a `try_recv` and a clock comparison.
        let polled = self.surface.poll();
        // What this page shows is what open terminals should draw — a press
        // here reaches them now, not after the authority's round trip.
        crate::terminal_prefs::adopt(self.surface.terminal());
        polled || downloaded
    }

    fn dictation_refused(&mut self, refusal: koru_dictation::Refusal) -> bool {
        self.note_refusal(refusal.code)
    }

    fn voice_models_changed(&mut self) -> bool {
        if cfg!(test) {
            return false;
        }
        self.refresh_voice();
        true
    }
}

// ------------------------------------------------------------ the readings

/// **The renderer passthrough, in this process's own tokens.**
///
/// `renderer.conf` is exempt from the settings authority (ADR-266) and
/// `koru-window` is its sole writer, so the surface is handed opaque strings
/// and hands one back. macOS has one backend — Metal — so the row is absent
/// there rather than being a control with nothing to control.
fn renderer_row() -> RendererRow {
    if cfg!(target_os = "macos") {
        return RendererRow::none();
    }
    RendererRow {
        label: "Graphics renderer".to_owned(),
        note: "Only change if you see drawing glitches. Applies next time Koru starts.".to_owned(),
        options: koru_window::RendererBackend::ALL
            .iter()
            .map(|backend| (backend.key().to_owned(), backend_label(*backend).to_owned()))
            .collect(),
        current: koru_window::RendererBackend::load().key().to_owned(),
    }
}

/// The names people know these by, not the tokens they are stored as.
fn backend_label(backend: koru_window::RendererBackend) -> &'static str {
    match backend {
        koru_window::RendererBackend::Auto => "Auto",
        koru_window::RendererBackend::Dx12 => "DirectX 12",
        koru_window::RendererBackend::Vulkan => "Vulkan",
        koru_window::RendererBackend::Gl => "OpenGL",
    }
}

/// **Write it, and say so both ways.** This is the one setting whose effect
/// arrives a whole launch later: when the next start draws on a different
/// backend — or crashes on one — the log has to say that somebody asked for it,
/// and a write that failed has to be distinguishable from a choice that never
/// took.
pub(crate) fn set_renderer_backend(token: &str) {
    let wanted = koru_window::RendererBackend::ALL
        .iter()
        .copied()
        .find(|backend| backend.key() == token);
    let Some(backend) = wanted else {
        return;
    };
    let previous = koru_window::RendererBackend::load();
    if backend == previous {
        return;
    }
    match backend.store() {
        Ok(()) => tracing::info!(
            target: "koru::settings",
            from = previous.key(),
            to = backend.key(),
            "renderer backend changed; it applies on the next launch"
        ),
        Err(error) => tracing::error!(
            target: "koru::settings",
            to = backend.key(),
            %error,
            "the renderer backend could not be recorded; the next launch keeps the old one"
        ),
    }
}

/// The AI section's slot. The provider rows live in `koru-ai-ui`, where they
/// are private to `ChatSurface` and drawn at fixed offsets into its own
/// viewport; moving them would change where an AI provider is configured, and
/// this packet changes no behavior beyond the re-homing. So Settings names the
/// section and offers the way in.
fn ai_slot() -> AiSlot {
    AiSlot {
        note: "Managed in Koru AI. Koru works fine without it.".to_owned(),
        action: Some("Open Koru AI…".to_owned()),
    }
}

/// Every language a picker offers: *detect it* first, then the eight in
/// `koru_i18n::Lang::ALL`'s order, each named in itself. A person who has
/// expressed no preference is not saying English.
fn language_options() -> Vec<(DictationLanguage, String)> {
    let mut out = vec![(
        DictationLanguage::parse(DictationLanguage::AUTO),
        "Detect automatically".to_owned(),
    )];
    out.extend(VoiceLanguage::ALL.into_iter().map(|language| {
        (
            DictationLanguage::parse(voice_prefs::language_token(Requested::Fixed(language))),
            language.endonym().to_owned(),
        )
    }));
    out
}

/// *System default* first, then what the worker last said it could see, then —
/// if it is not among them — the device somebody remembered. The remembered one
/// is kept precisely *because* it is missing: dropping it would move the
/// selection with nothing to put it back with.
fn input_options() -> Vec<(DictationInput, String)> {
    let mut out = vec![(
        DictationInput::parse(DictationInput::SYSTEM_DEFAULT),
        "System default".to_owned(),
    )];
    let known = koru_workspace_host::voice_devices::known();
    out.extend(known.iter().map(|device| {
        (
            DictationInput::parse(voice_prefs::input_token(&InputChoice::named(&device.name))),
            device.name.clone(),
        )
    }));
    if let Some(wanted) = voice_prefs::input().name()
        && !known.iter().any(|device| device.name == wanted)
    {
        out.push((
            DictationInput::parse(voice_prefs::input_token(&InputChoice::named(wanted))),
            wanted.to_owned(),
        ));
    }
    out
}

/// Exactly one situation produces a sentence: a remembered device the worker
/// has looked for and not found. Not an error, and not a correction — the
/// choice is left where it was, so a headset that wakes up is used again.
fn input_note() -> Option<String> {
    let missing = koru_workspace_host::voice_devices::resolution()
        .missing()?
        .to_owned();
    Some(format!(
        "{missing} is not connected. Using the system default until it is."
    ))
}

fn quality_label(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Compact => "Compact",
        ModelTier::HighAccuracy => "High accuracy",
    }
}

/// **The size is the descriptor's, never a literal**, and the live-versus-final
/// difference is the substance: Whisper transcribes a finished utterance, so
/// words appear when you stop, while Nemotron revises as you speak. Nothing
/// here claims either is more accurate.
fn quality_note(tier: ModelTier) -> String {
    let behaviour = match tier {
        ModelTier::Compact => "Words appear when you finish speaking.",
        ModelTier::HighAccuracy => "Words appear as you speak.",
    };
    match koru_workspace_host::voice_install::tier_megabytes(tier) {
        Some(megabytes) => format!("{} — {} MB. {behaviour}", quality_label(tier), megabytes),
        // No size rather than a guessed one: nothing has read a descriptor yet.
        None => format!("{} — {behaviour}", quality_label(tier)),
    }
}

/// The missing-dictionary row: what is missing and its size, how far a
/// download has got, or why the last one stopped.
fn dictionary_row(
    ui: koru_i18n::Lang,
    lang: koru_workspace_host::spelling::SpellLang,
) -> koru_settings_ui::DictionaryRow {
    use koru_workspace_host::spelling::download::{self, State};
    let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.0);
    let size = koru_workspace_host::spelling::sources::download_bytes(lang);
    match download::state() {
        State::Running {
            lang: running,
            received,
            total,
        } if running == lang => koru_settings_ui::DictionaryRow {
            line: koru_i18n::fill(
                koru_i18n::t(ui, koru_i18n::Key::SettingsDictionaryProgress),
                &[lang.endonym(), &megabytes(received), &megabytes(total)],
            ),
            action: None,
        },
        State::Failed { lang: failed, .. } if failed == lang => koru_settings_ui::DictionaryRow {
            line: koru_i18n::fill(
                koru_i18n::t(ui, koru_i18n::Key::SettingsDictionaryDownloadFailed),
                &[lang.endonym()],
            ),
            action: Some(koru_i18n::t(ui, koru_i18n::Key::TryAgain).to_owned()),
        },
        _ => koru_settings_ui::DictionaryRow {
            line: format!(
                "{} \u{2014} {} ({} MB).",
                koru_i18n::t(ui, koru_i18n::Key::SpellingDictionaryFilesMissing),
                lang.endonym(),
                megabytes(size)
            ),
            action: Some(koru_i18n::t(ui, koru_i18n::Key::SettingsDictionaryDownload).to_owned()),
        },
    }
}

/// Derived, every time it is drawn, from the reading — never from a stored flag.
fn model_line(models: &koru_workspace_host::voice_install::VoiceModels) -> String {
    if !models.installed() {
        return "Not downloaded yet — download it to use voice input.".to_owned();
    }
    if !models.complete() {
        return format!(
            "Partly installed · {} MB; one model still needed",
            koru_voice_consent::megabytes(models.bytes)
        );
    }
    format!(
        "Installed · {} MB",
        koru_voice_consent::megabytes(models.bytes)
    )
}

fn tier_of(tier: ModelTier) -> DictationTier {
    DictationTier::parse(tier.as_str())
}

pub(crate) fn tier_from(tier: DictationTier) -> ModelTier {
    voice_prefs::parse_tier(tier.token())
}

pub(crate) fn language_from(language: &DictationLanguage) -> Requested {
    voice_prefs::parse_language(language.token())
}

pub(crate) fn input_from(input: &DictationInput) -> InputChoice {
    voice_prefs::parse_input(input.token())
}

pub(crate) fn close_behavior_of(choice: CloseBehaviorChoice) -> koru_window::CloseBehavior {
    match choice {
        CloseBehaviorChoice::Ask => koru_window::CloseBehavior::Ask,
        CloseBehaviorChoice::Background => koru_window::CloseBehavior::Background,
        CloseBehaviorChoice::Quit => koru_window::CloseBehavior::Quit,
    }
}

fn close_behavior_to(behavior: koru_window::CloseBehavior) -> CloseBehaviorChoice {
    match behavior {
        koru_window::CloseBehavior::Ask => CloseBehaviorChoice::Ask,
        koru_window::CloseBehavior::Background => CloseBehaviorChoice::Background,
        koru_window::CloseBehavior::Quit => CloseBehaviorChoice::Quit,
    }
}

/// The Settings sections this build shows: Koru's, minus the Koru AI
/// providers card without Koru AI and the Reminders page without reminders
/// (`koru_features`, guide/contributing/FEATURE-FLAGS.md). The host decides; the
/// settings crate only draws what it is given.
fn sections() -> Sections {
    Sections {
        ai: koru_features::ai(),
        reminders: koru_features::reminders(),
        ..Sections::koru()
    }
}
