// SPDX-License-Identifier: MPL-2.0

//! The notification area, and the one way back to Koru from it.
//!
//! ## The rule this module exists to keep
//!
//! **An agent you cannot see is an agent you cannot stop.** It has no window
//! most of the time and no console under `windows_subsystem`, so without an
//! icon it is a process holding a global hotkey that only Task Manager can end.
//! So the menu always offers **Quit**, and the tooltip names the version and the
//! channel (or an explicit root) plus an internal build's branch — two agents
//! can run at once and two identical icons would be worse than one.
//!
//! For the same reason the menu carries **About Koru** and **Check for
//! updates**: with several channels installed side by side, *which build is
//! this* and *is there a newer one* have to be answerable from the notification
//! area, because for an agent with no window open there is nowhere else to ask.
//!
//! ## It is created on the winit thread, and it must **wake** the loop
//!
//! `tray-icon` registers the icon against a hidden window and delivers clicks
//! as window messages, so something has to pump for it. `apps/koru-store-server` runs
//! its own `GetMessageW` loop on a dedicated thread precisely because a daemon
//! has none; the agent has winit's, so the icon is created here, on that
//! thread, which is what the crate's documentation asks for.
//!
//! **But polling is not available to us, and that is the part that is easy to
//! get wrong.** This loop is `ControlFlow::Wait` with no clock at all — a
//! tray-resident agent must sit at zero CPU — so a menu click left in a polled
//! channel would never be seen. The handlers therefore **raise an event through
//! the [`Waker`]**, which is the whole reason the loop has a wake path.
//!
//! `set_event_handler`'s closure must be `Send`, which is what stopped the
//! store using it: that one wanted to read a `CheckMenuItem`'s state inside the
//! handler, and those hold `Rc` internals. Here the handler only carries an id
//! and wakes; the loop, which owns the [`Tray`] on the winit thread, is what
//! reads anything.
//!
//! ## Failure is never fatal
//!
//! No icon resource, no shell, another process holding the notification area —
//! the agent carries on without one. An agent that refused to run because it
//! could not draw a picture would be a worse outcome than the one it was
//! avoiding.
//!
//! ## Windows and macOS share `tray-icon`; Linux speaks StatusNotifierItem
//!
//! This section used to say *Windows only*, because `tray-icon` "reaches for
//! gtk and libxdo elsewhere". [ADR-118](../../../docs/decisions/README.md) checked:
//! that weight is Linux's — on macOS the crate wraps `NSStatusItem` and pulls
//! neither — so one implementation, `imp`, serves both platforms and the
//! `cfg`s are what change.
//!
//! Linux has its own, `linux`: the StatusNotifierItem D-Bus protocol through
//! `ksni`, which needs no gtk. Its menu is a pure function of the tray's
//! state, [`linux_menu`], built from the same text helpers `imp` uses, so the
//! platforms cannot tell different stories. With no notification-area host on
//! the session bus — GNOME without the AppIndicator extension — `show`
//! answers `None` and the agent runs headless, as it does wherever there is
//! no tray. Linux leaves out the same two rows macOS does, for reasons of its
//! own (see [`linux_menu`]), and keeps the Windows left click.
//!
//! Two rows are Windows-only and are **left out** of the macOS menu rather
//! than shipped dead ([ADR-118](../../../docs/decisions/ADR-101-125.md#adr-118--the-tray-is-windows-and-macos-the-gtk-objection-was-linuxs)):
//! *Backup Store…* (its folder picker is `rfd`, and NSOpenPanel wants the main
//! thread the agent's dialog worker does not have) and the *Links* submenu
//! (`koru://` registration is bundle-level work on macOS, deferred with
//! packaging). And one click differs: on macOS the **menu** opens on left
//! click — the platform's convention — so the left-click gesture (bring back
//! the workspace window in use, or Search when there is none) stays
//! Windows-only.

#[cfg(any(windows, target_os = "macos", target_os = "linux", test))]
use crate::event::TrayAsk;

#[cfg(any(windows, target_os = "macos"))]
pub use imp::Tray;

#[cfg(target_os = "linux")]
pub use linux::Tray;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub use stub::Tray;

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use super::{
        Shortcut, TrayAsk, VoiceAsk, VoiceLine, VoiceMenu, VoiceStatus, branch, input_title,
        tooltip, voice_line, voice_menu_localized,
    };
    use crate::event::{AgentEvent, Waker};
    use crate::registry::{ConnectionId, OpenWorkspace};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    // `Code` and `Modifiers` are deliberately not imported: an accelerator is
    // *parsed* from the label the registration reported, never assembled here
    // from parts. Building one by hand is what let the menu name a chord this
    // agent had not registered.
    use tray_icon::menu::{
        CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
        accelerator::Accelerator,
    };
    use tray_icon::{Icon, TrayIconBuilder};
    #[cfg(windows)]
    use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};

    /// Whether a notification-area event is the icon's left button.
    ///
    /// **Exactly one gesture: releasing the left button.** Two things make that
    /// narrower than it looks, and both have been wrong here before.
    ///
    /// **The button has to be named.** `Click { .. }` matches every button, so
    /// a right click opened the overlay *as well as* the menu — and the overlay
    /// takes the focus, which dismisses the menu before anyone can reach Quit.
    /// The agent then could not be stopped from the notification area at all,
    /// which is the one place it is reachable from. The same pairing in the
    /// other direction is why the builder opts out of
    /// `menu_on_left_click` on Windows: the crate's default is `true`, so a
    /// left click showed the context menu *as well as* answering the click.
    ///
    /// **And the button state has to be named too.** `Click` carries a
    /// `button_state` and is emitted for the press *and* the release, so
    /// matching on the button alone asks twice per click — once
    /// under the pointer going down and again coming up. Opening on the
    /// release is also the convention every other menu on the platform follows.
    ///
    /// Pure, and tested, because this is the third time the predicate has been
    /// the bug and a comment has twice not been enough.
    #[cfg(windows)]
    pub(super) fn is_icon_click(event: &TrayIconEvent) -> bool {
        matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        )
    }

    /// The status and action items the *Voice input* submenu is built from, borrowed
    /// together.
    ///
    /// A struct rather than a positional argument list because [`fill_voice`] is called
    /// from two places — once while the tray is being built and once on every
    /// state change — and a positional list of seven `&MenuItem`s is a swap
    /// waiting to happen that the type system would not catch.
    struct VoiceItems<'a> {
        live: &'a MenuItem,
        summary: &'a MenuItem,
        chord: &'a MenuItem,
        error: &'a MenuItem,
        act: &'a MenuItem,
        settings: &'a MenuItem,
        release: &'a MenuItem,
        stop: &'a MenuItem,
        input: &'a InputMenu,
    }

    /// **The microphone picker's native side.**
    ///
    /// Its rows are minted on every rebuild, unlike every other item in this
    /// menu: there are as many as the machine has microphones, so their ids
    /// cannot be closed over when the menu is built. That is exactly the
    /// open-workspace rows' arrangement and it is answered the same way --
    /// a shared map from id to choice, consulted by the handler after every
    /// fixed id has failed to match.
    ///
    /// `items` is a `RefCell` because `fill_voice` runs on `&` and a native
    /// item dropped while it is still in a menu is a row that stops raising
    /// events -- so the minted rows have to be kept somewhere that outlives
    /// the call. It is never borrowed across another borrow of itself; the one
    /// function that touches it is below.
    struct InputMenu {
        menu: Submenu,
        items: std::cell::RefCell<Vec<CheckMenuItem>>,
        routes: Arc<Mutex<HashMap<String, koru_voice::InputChoice>>>,
    }

    impl InputMenu {
        fn new() -> InputMenu {
            InputMenu {
                menu: Submenu::new("Input", true),
                items: std::cell::RefCell::new(Vec::new()),
                routes: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        /// Rebuild the rows from the state, and register what each one asks
        /// for.
        fn fill(&self, state: &VoiceMenu) {
            self.menu.set_text(input_title(state));
            while self.menu.remove_at(0).is_some() {}
            let mut items = self.items.borrow_mut();
            items.clear();
            let Ok(mut routes) = self.routes.lock() else {
                return;
            };
            routes.clear();
            for row in super::input_rows(state) {
                // `&` is a native Windows mnemonic, and a device name is
                // somebody's own text that may carry one. Doubling preserves
                // it -- the open-workspace rows' rule.
                let item = CheckMenuItem::new(row.label.replace('&', "&&"), true, row.chosen, None);
                routes.insert(item.id().0.clone(), row.choice);
                let _ = self.menu.append(&item);
                items.push(item);
            }
        }
    }

    /// **Put the submenu's lines in, from the state.**
    ///
    /// Called at construction *and* from [`Tray::set_voice`], and it has to be
    /// both: `set_voice` returns early when the state has not moved, so a
    /// submenu that was only filled on a change would be empty until one
    /// happened — which on a machine where nothing about voice ever changes is
    /// for the life of the process.
    ///
    /// **Rebuilt rather than relabelled**, unlike every other row in this menu:
    /// the live line and the error line are absent most of the time, so what
    /// changes is *which lines exist*. The items are the ones minted in
    /// [`Tray::show`] and are re-appended by identity, so the three ids the menu
    /// handler closed over are still the three ids that arrive — the same
    /// arrangement `set_workspaces` uses, without its routing table, because
    /// these three rows are known at build time.
    fn fill_voice(
        menu: &Submenu,
        items: &VoiceItems<'_>,
        state: &VoiceMenu,
        shortcut: &Shortcut,
        lang: koru_i18n::Lang,
    ) {
        menu.set_text(voice_line(&state.row));
        while menu.remove_at(0).is_some() {}
        let chord = match shortcut {
            Shortcut::Chord(label) => Some(label.as_str()),
            Shortcut::Unknown | Shortcut::Absent => None,
        };
        for line in voice_menu_localized(state, chord, lang) {
            match line {
                VoiceLine::Separator => {
                    let _ = menu.append(&PredefinedMenuItem::separator());
                }
                VoiceLine::Status(slot, text) => {
                    let item = match slot {
                        VoiceStatus::Live => items.live,
                        VoiceStatus::Summary => items.summary,
                        VoiceStatus::Chord => items.chord,
                        VoiceStatus::Error => items.error,
                    };
                    item.set_text(text);
                    let _ = menu.append(item);
                }
                VoiceLine::Input(_) => {
                    // The rows themselves are `input_rows(state)`, taken
                    // inside `fill` -- the line carries them so the pure
                    // function is testable, and the native side re-derives
                    // rather than moving a `Vec` through two layers.
                    items.input.fill(state);
                    let _ = menu.append(&items.input.menu);
                }
                VoiceLine::Action {
                    ask,
                    label,
                    enabled,
                } => {
                    let item = match ask {
                        VoiceAsk::Act => items.act,
                        VoiceAsk::Settings => items.settings,
                        VoiceAsk::Release => items.release,
                        VoiceAsk::Stop => items.stop,
                    };
                    item.set_text(label);
                    item.set_enabled(enabled);
                    let _ = menu.append(item);
                }
            }
        }
    }

    /// A live icon. **Dropping it removes the icon**, which is why the agent
    /// holds one rather than building it and forgetting.
    pub struct Tray {
        _icon: tray_icon::TrayIcon,
        /// Held rather than forgotten, because the chord beside it is not known
        /// when the menu is built. See [`Tray::set_shortcut`].
        search: MenuItem,
        ai_chat: MenuItem,
        ai_shortcut: Option<bool>,
        /// What that row currently claims, so a repeated report is not a
        /// repeated native call.
        shortcut: Shortcut,
        store_status: MenuItem,
        store_running: Option<bool>,
        /// The dictation submenu, held for the same reason the Search row is:
        /// what it says is a state that moves after the menu exists — a model
        /// arrives, a worker starts, a dictation ends — and `tray-icon` offers
        /// no "menu is about to open" hook to read it lazily from.
        ///
        /// Its **composition** changes, not only its text: the live line and
        /// the error line are absent most of the time, so a rebuild is what
        /// [`Tray::set_voice`] does. The items below are minted once and
        /// re-appended, so their ids — which the menu handler closed over
        /// before any of this existed — survive every rebuild.
        voice: Submenu,
        voice_live: MenuItem,
        voice_summary: MenuItem,
        voice_chord: MenuItem,
        voice_error: MenuItem,
        voice_act: MenuItem,
        voice_settings: MenuItem,
        voice_release: MenuItem,
        voice_stop: MenuItem,
        voice_input: InputMenu,
        voice_state: VoiceMenu,
        dictation_shortcut: Shortcut,
        _gateway: Submenu,
        /// The **Agents** submenu (Q-E8 P9): New pinned at the top, then the
        /// live sessions grouped by project, straight from the session host's
        /// catalog rather than from any window's projection.
        agents: Submenu,
        agents_new: MenuItem,
        /// Every minted row — session rows *and* the disabled project
        /// headings — held because a native item dropped while it is still in
        /// a menu is a row that stops raising events.
        agents_items: Vec<MenuItem>,
        agents_routes: Arc<Mutex<HashMap<String, koru_session_protocol::SessionId>>>,
        agents_signature: Vec<super::AgentsLine>,
        _store: Submenu,
        _about: Submenu,
        workspaces: Submenu,
        open_workspace: MenuItem,
        _data_locations: Submenu,
        _links: Submenu,
        /// Held for the same reason the Search row is: what it says is decided
        /// after the menu exists, and changes when an action lands.
        scheme_status: MenuItem,
        scheme_action: MenuItem,
        /// The relay switch (iroh-relay decision №6). The label never moves;
        /// what moves is the tick, and [`Tray::set_relay`] pushes it because
        /// there is no "menu is about to open" hook to read it lazily from.
        relay: CheckMenuItem,
        /// What that row currently claims, so a repeated report is not a
        /// repeated native call — the Store status row's arrangement.
        relay_state: Option<bool>,
        /// The informational row beneath the switch: `<host> — <state>`,
        /// e.g. `use1-1.relay.koru.do — connected`. Drawn disabled — it is
        /// a reading, not an action — and pushed per wake like the switch,
        /// because there is no "menu is about to open" hook.
        relay_status: MenuItem,
        /// What that row currently says, the `relay_state` arrangement.
        relay_status_state: koru_network_iroh::RelayConnectionStatus,
        /// The menu's language, decided once at build; rows relabelled
        /// later ([`Tray::set_relay_status`]) must speak the same one.
        lang: koru_i18n::Lang,
        workspace_items: Vec<MenuItem>,
        workspace_routes: Arc<Mutex<HashMap<String, ConnectionId>>>,
        workspace_signature: Vec<(ConnectionId, String)>,
    }

    impl Tray {
        /// Put an icon in the notification area for `segment`.
        ///
        /// **Must be called on the winit thread.** `None` means there is none
        /// and the agent carries on without it.
        pub fn show(segment: &str, root: Option<&std::path::Path>, waker: Waker) -> Option<Self> {
            // The exe's own icon, resource 1 — the id `koru.rc` gives it. Read
            // from the binary rather than from `assets/`, so a bundle with a
            // missing asset folder still shows the right picture and there is
            // no path to resolve.
            #[cfg(windows)]
            let icon = Icon::from_resource(1, None).ok()?;
            // macOS has no such resource, so the picture is compiled in —
            // `include_bytes!`, deliberately, for the same reason as above: an
            // icon that depends on a path is an icon that goes missing.
            #[cfg(target_os = "macos")]
            let icon = {
                let rgba = image::load_from_memory(super::ICON_PNG).ok()?.to_rgba8();
                let (width, height) = rgba.dimensions();
                Icon::from_rgba(rgba.into_raw(), width, height).ok()?
            };
            let menu = Menu::new();
            let lang =
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
            // **No accelerator yet, and that is deliberate.** Which chord this
            // agent holds is not known here: `hotkey::start` registers on its
            // own thread, against whatever else on the machine wants the same
            // combination, and falls through a list of candidates. The row is
            // filled in by [`Tray::set_shortcut`] when the answer arrives.
            //
            // It used to be `Ctrl+Shift+K`, written here as a constant — which
            // was right for the first agent on a machine and a plain lie for
            // every other one, told by the only surface a person can consult.
            let search = MenuItem::new("Search", true, None);
            let ai_chat = MenuItem::new("Koru AI Chat", true, None);
            // **The one surface that is there when no window is**, so it is
            // where somebody who has never read a release note finds out that
            // dictation exists, where a model is installed from, and where
            // *does this work and what is it holding* is answered. The title is
            // stable so it stays easy to scan; the lines inside carry the
            // state. See [`voice_line`] and [`super::voice_menu`].
            // **The microphone row starts on *follow the OS* and is corrected
            // by the agent's first `set_voice`.** This closure builds the menu
            // before a window exists and holds no settings link; the agent's
            // own reading — which is the authority's — arrives with the next
            // refresh, and the row is re-labelled only if it actually moved.
            let voice_state = super::voice_state(root, koru_voice::InputChoice::SystemDefault);
            // **A stable submenu title, with the state inside.** It is enabled
            // in every state, including a switched-off installation: the line
            // inside saying *why* there is nothing here is unreachable behind
            // a submenu nobody can open.
            let voice = Submenu::new(voice_line(&voice_state.row), true);
            // Four status lines and three actions, minted once. Only the
            // actions are pressable; a status line is `enabled: false`, which
            // on both platforms means it raises no menu event at all.
            let voice_live = MenuItem::new("", false, None);
            let voice_summary = MenuItem::new("", false, None);
            let voice_chord = MenuItem::new("", false, None);
            let voice_error = MenuItem::new("", false, None);
            let voice_act = MenuItem::new("", true, None);
            let voice_settings = MenuItem::new(
                koru_i18n::t(lang, koru_i18n::Key::VoiceGlobalOpen),
                true,
                None,
            );
            let voice_release = MenuItem::new("", false, None);
            let voice_stop = MenuItem::new("", false, None);
            let voice_input = InputMenu::new();
            let input_routes_for_handler = Arc::clone(&voice_input.routes);
            // **Filled here, not left to the first change.** `set_voice` is a
            // no-op while the state has not moved, so a submenu that was only
            // populated on a change would be empty on a machine where nothing
            // about voice ever changes -- which is most of them.
            fill_voice(
                &voice,
                &VoiceItems {
                    live: &voice_live,
                    summary: &voice_summary,
                    chord: &voice_chord,
                    error: &voice_error,
                    act: &voice_act,
                    settings: &voice_settings,
                    release: &voice_release,
                    stop: &voice_stop,
                    input: &voice_input,
                },
                &voice_state,
                &Shortcut::Unknown,
                lang,
            );
            let open_workspace = MenuItem::new("Open Workspace", true, None);
            let workspaces = Submenu::new("Workspaces", true);
            workspaces.append(&open_workspace).ok()?;
            let lang =
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
            let reminders =
                MenuItem::new(koru_i18n::t(lang, koru_i18n::Key::ReminderAll), true, None);
            // **Agents, not "Koru Terminal"** (Q-E8 P9): sessions are Koru
            // Workspace tabs now, and the submenu says what its rows are —
            // the running agents — rather than which executable used to show
            // them.
            let agents = Submenu::new(koru_i18n::t(lang, koru_i18n::Key::TerminalAgents), true);
            let agents_new =
                MenuItem::new(koru_i18n::t(lang, koru_i18n::Key::TerminalNew), true, None);
            agents.append(&agents_new).ok()?;
            let location = MenuItem::new("Show root", true, None);
            let local_data = MenuItem::new("Show local data", true, None);
            let shared = MenuItem::new("Show remote data", true, None);
            let data_locations = Submenu::new("Data locations", true);
            data_locations.append(&location).ok()?;
            data_locations.append(&local_data).ok()?;
            data_locations.append(&shared).ok()?;
            let gateway = Submenu::new("API Gateway", true);
            let gateway_docs = MenuItem::new("Docs", true, None);
            gateway.append(&gateway_docs).ok()?;
            let store = Submenu::new("Store", true);
            let store_status = MenuItem::new("Store running: Checking…", true, None);
            let store_stats = MenuItem::new("Open store stats", true, None);
            let store_location = MenuItem::new("Open store location", true, None);
            let store_backup = MenuItem::new("Backup Store…", true, None);
            store.append(&store_status).ok()?;
            store.append(&PredefinedMenuItem::separator()).ok()?;
            store.append(&store_stats).ok()?;
            store.append(&store_location).ok()?;
            // Windows only: the folder picker is `rfd` (Windows-only in this
            // crate), and NSOpenPanel wants the main thread while the agent
            // runs its dialogs on a worker. The item exists on macOS but is
            // never appended — a detached row raises no event, so the handler
            // below needs no second shape.
            #[cfg(windows)]
            store.append(&store_backup).ok()?;
            // **Which build is this, and where are its notes.** The row names
            // the build and opens the page for *that* version, which
            // `koru-release` wrote once and never replaces — see
            // [`release_notes_url`].
            //
            // **A build with no page gets a row that cannot be clicked.**
            // `MenuItem::new`'s second argument is `enabled`, so an unpublished
            // build's About row is drawn greyed and raises no menu event at all:
            // the version is still readable, which is the whole point of the
            // row, and there is no URL to open that would 404. The id is
            // registered either way so there is one shape here rather than two.
            // **Links.** A status row that cannot be pressed, and one that can.
            // The state is read once here and refreshed by
            // [`Tray::set_url_scheme`] whenever an action changes it; there is
            // no "menu about to open" hook to read it lazily from, which is the
            // same constraint the Search row's accelerator lives under.
            let scheme_state = super::url_scheme_state();
            let scheme_status = MenuItem::new(super::url_scheme_line(&scheme_state), false, None);
            let scheme_action = MenuItem::new(
                super::url_scheme_action(&scheme_state),
                !matches!(
                    scheme_state,
                    koru_url_scheme::Registration::Unsupported
                        | koru_url_scheme::Registration::Foreign { .. }
                ),
                None,
            );
            let links = Submenu::new("Links", true);
            links.append(&scheme_status).ok()?;
            links.append(&scheme_action).ok()?;
            let scheme_action_id = scheme_action.id().0.clone();

            // **The relay switch** (iroh-relay decision №6): one checkable
            // row, device-local, on/off only — the relay list is never
            // edited from a menu, and direct/LAN paths are not this row's
            // business. The tick is the state: checked means relays are in
            // use. An agent whose device network never started has nothing
            // to switch, so the row is drawn greyed rather than left out —
            // the About row's rule — and a greyed row raises no menu event.
            let relay_state = super::relay_state();
            let relay_drawn = super::relay_row(relay_state);
            let relay = CheckMenuItem::new(
                koru_i18n::t(lang, koru_i18n::Key::RelayUse),
                relay_drawn.enabled,
                relay_drawn.checked,
                None,
            );
            let relay_id = relay.id().0.clone();
            // The reading beneath the switch — which relay, and whether it
            // is reached. Disabled like the scheme status row: a status
            // that cannot be pressed, so it raises no menu event.
            let relay_status_state = super::relay_connection();
            let relay_status = MenuItem::new(
                super::relay_status_line(lang, &relay_status_state),
                false,
                None,
            );

            let notes = super::release_notes_url();
            let info = koru_update::build_info();
            let about_build = MenuItem::new(
                super::about_line(info.version, info.build_number, info.build_label()),
                notes.is_some(),
                None,
            );
            let about = Submenu::new("About Koru", true);
            about.append(&about_build).ok()?;
            // Enabled unconditionally, unlike the row above it: a mailbox exists
            // whether or not this build was ever published.
            let feedback = MenuItem::new("Send feedback", true, None);
            about.append(&feedback).ok()?;
            // Left enabled on every build, including one with no feed: what a
            // check *means* on an internal build is the agent's answer to give,
            // not the menu's — `update.rs` already refuses one and can say so.
            let check_updates = MenuItem::new("Check for updates", true, None);
            let quit = MenuItem::new("Quit Koru Agent", true, None);
            let about_id = about_build.id().0.clone();
            let feedback_id = feedback.id().0.clone();
            let check_updates_id = check_updates.id().0.clone();
            let (ai_chat_id, search_id, open_id, location_id, local_data_id, shared_id, quit_id) = (
                ai_chat.id().0.clone(),
                search.id().0.clone(),
                open_workspace.id().0.clone(),
                location.id().0.clone(),
                local_data.id().0.clone(),
                shared.id().0.clone(),
                quit.id().0.clone(),
            );
            let store_status_id = store_status.id().0.clone();
            let reminders_id = reminders.id().0.clone();
            let voice_id = voice_act.id().0.clone();
            let voice_settings_id = voice_settings.id().0.clone();
            let voice_release_id = voice_release.id().0.clone();
            let voice_stop_id = voice_stop.id().0.clone();
            let agents_new_id = agents_new.id().0.clone();
            let gateway_docs_id = gateway_docs.id().0.clone();
            let store_stats_id = store_stats.id().0.clone();
            let store_location_id = store_location.id().0.clone();
            let store_backup_id = store_backup.id().0.clone();
            // **A surface this build hides is never appended** (koru-features):
            // a detached item raises no menu event, so the handler below keeps
            // one shape — the same rule the macOS-only backup row follows.
            if koru_features::ai() {
                menu.append(&ai_chat).ok()?;
            }
            menu.append(&search).ok()?;
            if koru_features::reminders() {
                menu.append(&reminders).ok()?;
            }
            // Beside Search and Koru AI Chat: the three things a person does
            // from here, above the diagnostics.
            menu.append(&voice).ok()?;
            menu.append(&workspaces).ok()?;
            if koru_features::agents() {
                menu.append(&agents).ok()?;
            }
            menu.append(&gateway).ok()?;
            menu.append(&store).ok()?;
            menu.append(&PredefinedMenuItem::separator()).ok()?;
            menu.append(&data_locations).ok()?;
            // Windows only, like the backup row: registering `koru://` on
            // macOS is bundle-level work (CFBundleURLTypes), deferred with
            // packaging — a submenu whose only action answers `Unsupported`
            // would be furniture. ADR-118's rule: prefer omission.
            #[cfg(windows)]
            menu.append(&links).ok()?;
            // Beside the other device-level rows, on both platforms: the
            // device network runs wherever the tray does.
            menu.append(&relay).ok()?;
            menu.append(&relay_status).ok()?;
            menu.append(&PredefinedMenuItem::separator()).ok()?;
            menu.append(&about).ok()?;
            menu.append(&check_updates).ok()?;
            menu.append(&PredefinedMenuItem::separator()).ok()?;
            menu.append(&quit).ok()?;

            let workspace_routes = Arc::new(Mutex::new(HashMap::new()));
            let agents_routes = Arc::new(Mutex::new(HashMap::new()));

            let built = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                // **Identity is in the tooltip**: the version, then the channel
                // or an explicit root, and the branch for an internal build.
                // Two agents can run at once and two identical icons would be
                // worse than one — and now that several channels install side
                // by side, hovering is the cheapest answer there is to "which
                // build is this".
                .with_tooltip(tooltip(info.version, segment, root, branch()))
                // **On Windows the menu is the right button's.** The crate's
                // default here is `true` — the macOS convention — which showed
                // the context menu *as well as* answering a left click, so the
                // overlay opened underneath a menu it immediately dismissed.
                .with_menu_on_left_click(cfg!(target_os = "macos"))
                .with_icon(icon)
                .build()
                .ok()?;

            // Wake, never poll — see the module note. The closures carry only
            // ids and a `Waker`, which is what keeps them `Send`.
            {
                let waker = waker.clone();
                let workspace_routes = Arc::clone(&workspace_routes);
                let input_routes = input_routes_for_handler;
                let agents_routes = Arc::clone(&agents_routes);
                let (
                    ai_chat_id,
                    search_id,
                    reminders_id,
                    open_id,
                    location_id,
                    local_data_id,
                    shared_id,
                    agents_new_id,
                    gateway_docs_id,
                    store_status_id,
                    store_stats_id,
                    store_location_id,
                    store_backup_id,
                    about_id,
                    check_updates_id,
                    quit_id,
                    scheme_action_id,
                    relay_id,
                    voice_id,
                    voice_settings_id,
                    voice_release_id,
                    voice_stop_id,
                ) = (
                    ai_chat_id.clone(),
                    search_id.clone(),
                    reminders_id.clone(),
                    open_id.clone(),
                    location_id.clone(),
                    local_data_id.clone(),
                    shared_id.clone(),
                    agents_new_id.clone(),
                    gateway_docs_id.clone(),
                    store_status_id.clone(),
                    store_stats_id.clone(),
                    store_location_id.clone(),
                    store_backup_id.clone(),
                    about_id.clone(),
                    check_updates_id.clone(),
                    quit_id.clone(),
                    scheme_action_id.clone(),
                    relay_id.clone(),
                    voice_id.clone(),
                    voice_settings_id.clone(),
                    voice_release_id.clone(),
                    voice_stop_id.clone(),
                );
                MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                    let id = &event.id().0;
                    let ask = if *id == quit_id {
                        TrayAsk::Quit
                    } else if *id == ai_chat_id {
                        TrayAsk::AiChat
                    } else if *id == search_id {
                        TrayAsk::Search
                    } else if *id == reminders_id {
                        TrayAsk::Reminders
                    } else if *id == open_id {
                        TrayAsk::Restore
                    } else if *id == location_id {
                        TrayAsk::OpenLocation
                    } else if *id == local_data_id {
                        TrayAsk::OpenLocalDataLocation
                    } else if *id == shared_id {
                        TrayAsk::OpenSharedLocation
                    } else if *id == agents_new_id {
                        TrayAsk::NewAgent
                    } else if let Some(session) = agents_routes
                        .lock()
                        .ok()
                        .and_then(|routes| routes.get(id).copied())
                    {
                        TrayAsk::ShowAgentSession(session)
                    } else if *id == gateway_docs_id {
                        TrayAsk::OpenGatewayDocs
                    } else if *id == store_status_id {
                        TrayAsk::RefreshStore
                    } else if *id == store_stats_id {
                        TrayAsk::OpenStoreStats
                    } else if *id == store_location_id {
                        TrayAsk::OpenStoreLocation
                    } else if *id == store_backup_id {
                        TrayAsk::BackupStore
                    } else if *id == about_id {
                        TrayAsk::About
                    } else if *id == voice_id {
                        // **Only a wake.** Which of the four things this means
                        // depends on the model cache and on the kill switch,
                        // and reading either here would put file I/O on the
                        // winit thread inside a menu callback. The loop owns
                        // the Tray and answers.
                        TrayAsk::Voice
                    } else if *id == voice_settings_id {
                        TrayAsk::VoiceSettings
                    } else if *id == voice_release_id {
                        // A wake too, and a *different* one: releasing the
                        // model and stopping the worker are two things with
                        // different costs, so they are two asks rather than one
                        // the loop would have to disambiguate. Both dial a
                        // socket, which is not something a menu callback on the
                        // winit thread may do.
                        TrayAsk::VoiceRelease
                    } else if *id == voice_stop_id {
                        TrayAsk::VoiceStop
                    } else if *id == check_updates_id {
                        TrayAsk::CheckForUpdates
                    } else if *id == feedback_id {
                        TrayAsk::SendFeedback
                    } else if *id == scheme_action_id {
                        // One row, two meanings, decided by what is registered
                        // *now* rather than by what the label said when the
                        // menu was built.
                        match super::url_scheme_state() {
                            koru_url_scheme::Registration::ThisBuild => {
                                TrayAsk::UnregisterUrlScheme
                            }
                            _ => TrayAsk::RegisterUrlScheme,
                        }
                    } else if *id == relay_id {
                        // **Only a wake, carrying no boolean.** The loop
                        // reads `AgentNetwork::relay_enabled` fresh and
                        // flips it — the scheme row's rule: act on what is
                        // true now, not on what the tick said when the menu
                        // was drawn. Applying it here would also put a
                        // thread spawn inside a menu callback on the winit
                        // thread, which is not this handler's to do.
                        TrayAsk::ToggleRelay
                    } else if let Some(choice) = input_routes
                        .lock()
                        .ok()
                        .and_then(|routes| routes.get(id).cloned())
                    {
                        // **Only the choice travels.** Writing it is the
                        // shell's -- `preferences.conf` has one writer -- and
                        // reading that file here would put disk I/O on the
                        // winit thread inside a menu callback.
                        TrayAsk::ChooseVoiceInput(choice)
                    } else if let Some(connection) = workspace_routes
                        .lock()
                        .ok()
                        .and_then(|routes| routes.get(id).copied())
                    {
                        TrayAsk::OpenWorkspace(connection)
                    } else {
                        return;
                    };
                    waker.wake(AgentEvent::Tray(ask));
                }));
            }
            // On macOS the *menu* opens on left click — the platform's
            // convention for a status item, kept by `menu_on_left_click` above —
            // so the left-click gesture is Windows-only and no icon-event
            // handler is installed at all.
            #[cfg(windows)]
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if is_icon_click(&event) {
                    waker.wake(AgentEvent::Tray(TrayAsk::IconClick));
                }
            }));
            // The menu handler took its own clone; this one has no second
            // consumer on macOS.
            #[cfg(target_os = "macos")]
            drop(waker);

            Some(Self {
                _icon: built,
                search,
                ai_chat,
                ai_shortcut: None,
                shortcut: Shortcut::Unknown,
                store_status,
                store_running: None,
                voice,
                voice_live,
                voice_summary,
                voice_chord,
                voice_error,
                voice_act,
                voice_settings,
                voice_release,
                voice_stop,
                voice_input,
                voice_state,
                dictation_shortcut: Shortcut::Unknown,
                _gateway: gateway,
                agents,
                agents_new,
                agents_items: Vec::new(),
                agents_routes,
                agents_signature: Vec::new(),
                _store: store,
                _about: about,
                workspaces,
                open_workspace,
                _data_locations: data_locations,
                _links: links,
                scheme_status,
                scheme_action,
                relay,
                relay_state,
                relay_status,
                relay_status_state,
                lang,
                workspace_items: Vec::new(),
                workspace_routes,
                workspace_signature: Vec::new(),
            })
        }

        /// Replace the submenu only when the open-workspace projection changes.
        /// Agent events can be frequent; a native menu rebuild is not maintenance
        /// work that belongs on every one of them.
        pub fn set_workspaces(&mut self, rows: &[OpenWorkspace]) {
            let signature: Vec<_> = rows
                .iter()
                .map(|row| (row.connection, row.label.clone()))
                .collect();
            if signature == self.workspace_signature {
                return;
            }
            while self.workspaces.remove_at(0).is_some() {}
            self.workspace_items.clear();
            let _ = self.workspaces.append(&self.open_workspace);
            if let Ok(mut routes) = self.workspace_routes.lock() {
                routes.clear();
                if !rows.is_empty() {
                    let _ = self.workspaces.append(&PredefinedMenuItem::separator());
                    for row in rows {
                        // `&` denotes a native Windows menu mnemonic. Doubling
                        // preserves authored workspace and profile names.
                        let item = MenuItem::new(row.label.replace('&', "&&"), true, None);
                        routes.insert(item.id().0.clone(), row.connection);
                        let _ = self.workspaces.append(&item);
                        self.workspace_items.push(item);
                    }
                }
            }
            self.workspace_signature = signature;
        }

        /// Replace the native Agents submenu only when its projection changes.
        /// New remains pinned at the top; the grouped sessions follow it.
        ///
        /// The grouping is [`super::agents_menu_lines`]'s — pure, and a test
        /// can hold it — and this only turns the lines into native rows: a
        /// heading is a disabled item (the About row's rule: greyed raises no
        /// menu event), a separator is a separator, and a session row is
        /// registered in the routing table the click handler consults.
        pub fn set_agent_sessions(&mut self, catalog: &crate::agents_menu::AgentsCatalog) {
            let lines = super::agents_menu_lines(catalog, self.lang);
            if lines == self.agents_signature {
                return;
            }
            while self.agents.remove_at(0).is_some() {}
            self.agents_items.clear();
            let _ = self.agents.append(&self.agents_new);
            if let Ok(mut routes) = self.agents_routes.lock() {
                routes.clear();
                for line in &lines {
                    match line {
                        super::AgentsLine::Separator => {
                            let _ = self.agents.append(&PredefinedMenuItem::separator());
                        }
                        super::AgentsLine::Header(name) => {
                            // `&` is a native Windows mnemonic and a project
                            // name is somebody's own text — the workspace
                            // rows' rule.
                            let item = MenuItem::new(name.replace('&', "&&"), false, None);
                            let _ = self.agents.append(&item);
                            self.agents_items.push(item);
                        }
                        super::AgentsLine::Session { session, label } => {
                            let item = MenuItem::new(label.replace('&', "&&"), true, None);
                            routes.insert(item.id().0.clone(), *session);
                            let _ = self.agents.append(&item);
                            self.agents_items.push(item);
                        }
                    }
                }
            }
            self.agents_signature = lines;
        }

        /// Update the native status row when supervision reports a result.
        pub fn set_store_running(&mut self, running: bool) {
            if self.store_running == Some(running) {
                return;
            }
            self.store_status.set_text(if running {
                "Store running: Yes"
            } else {
                "Store running: No"
            });
            self.store_running = Some(running);
        }

        /// Say what the dictation submenu says now.
        ///
        /// Pushed rather than pulled, like the Store status row and the Search
        /// row's accelerator, and for the same reason: there is no hook for
        /// "the menu is about to open". A repeated state is not a repeated
        /// native rebuild, which is what the equality check at the top is for —
        /// agent events are frequent and a native menu rebuild is not
        /// maintenance work that belongs on every one of them.
        ///
        /// **Rebuilt rather than relabelled**, unlike every other row here: the
        /// live line and the error line are absent most of the time, so what
        /// changes is which lines exist. The items are the ones minted in
        /// [`Tray::show`] and are re-appended by identity, so the three ids the
        /// menu handler closed over are still the three ids that arrive — the
        /// same arrangement `set_workspaces` uses, without its routing table,
        /// because these three rows are known at build time.
        pub fn set_voice(&mut self, state: &VoiceMenu) {
            if &self.voice_state == state {
                return;
            }
            fill_voice(
                &self.voice,
                &VoiceItems {
                    live: &self.voice_live,
                    summary: &self.voice_summary,
                    chord: &self.voice_chord,
                    error: &self.voice_error,
                    act: &self.voice_act,
                    settings: &self.voice_settings,
                    release: &self.voice_release,
                    stop: &self.voice_stop,
                    input: &self.voice_input,
                },
                state,
                &self.dictation_shortcut,
                self.lang,
            );
            self.voice_state = state.clone();
        }

        /// Say which chord the Agent actually registered for global Dictation.
        pub fn set_dictation_shortcut(&mut self, shortcut: &Shortcut) {
            if &self.dictation_shortcut == shortcut {
                return;
            }
            self.dictation_shortcut = shortcut.clone();
            fill_voice(
                &self.voice,
                &VoiceItems {
                    live: &self.voice_live,
                    summary: &self.voice_summary,
                    chord: &self.voice_chord,
                    error: &self.voice_error,
                    act: &self.voice_act,
                    settings: &self.voice_settings,
                    release: &self.voice_release,
                    stop: &self.voice_stop,
                    input: &self.voice_input,
                },
                &self.voice_state,
                &self.dictation_shortcut,
                self.lang,
            );
        }

        /// Say which chord actually opens Search, once the hotkey thread knows.
        ///
        /// **`None` draws no accelerator and says so in the row's text**, rather
        /// than leaving *Search* looking like a plain menu item. There are two
        /// ways to get here and a person cannot tell them apart from the menu,
        /// so the wording covers both: every candidate was already taken by
        /// another application, or this agent has an explicit `--root` and took
        /// nothing machine-wide.
        /// Re-read who owns `koru://` and say so.
        ///
        /// Called after either action lands. It cannot be lazy: `tray-icon`
        /// offers no "menu is about to open" hook, which is the same reason the
        /// Search row's accelerator is pushed rather than pulled.
        ///
        /// **Two channels' agents will disagree.** Registering from one does
        /// not refresh the other's menu, and a registry poll for a menu nobody
        /// has open would be worse than a stale row somebody has to reopen.
        pub fn set_url_scheme(&mut self, state: &koru_url_scheme::Registration) {
            self.scheme_status.set_text(super::url_scheme_line(state));
            self.scheme_action.set_text(super::url_scheme_action(state));
            self.scheme_action.set_enabled(!matches!(
                state,
                koru_url_scheme::Registration::Unsupported
                    | koru_url_scheme::Registration::Foreign { .. }
            ));
        }

        /// Say what the relay switch is now — the network's cached answer,
        /// never a disk read.
        ///
        /// Pushed on every wake, like the scheme rows and for the same
        /// reason: there is no "menu is about to open" hook. Reading it
        /// back rather than tracking the click also means the tick cannot
        /// disagree with what the runtime was actually told — the native
        /// item flips its own checkmark on a press, and this is what makes
        /// that cosmetic rather than authoritative.
        pub fn set_relay(&mut self, state: Option<bool>) {
            if self.relay_state == state {
                return;
            }
            let drawn = super::relay_row(state);
            self.relay.set_checked(drawn.checked);
            self.relay.set_enabled(drawn.enabled);
            self.relay_state = state;
        }

        /// Say what the relay connection is now — the runtime's status
        /// snapshot, pushed per wake with the switch above it and guarded
        /// the same way: a repeated reading is not a repeated native call.
        /// The runtime tracks the state event-driven from its endpoint's
        /// own watcher; this only relabels a row.
        pub fn set_relay_status(&mut self, state: koru_network_iroh::RelayConnectionStatus) {
            if self.relay_status_state == state {
                return;
            }
            self.relay_status
                .set_text(super::relay_status_line(self.lang, &state));
            self.relay_status_state = state;
        }

        pub fn set_shortcut(&mut self, shortcut: &Shortcut) {
            if &self.shortcut == shortcut {
                return;
            }
            match shortcut {
                // No answer yet: leave the row exactly as it was built. Saying
                // anything here is guessing, and guessing is what this replaced.
                Shortcut::Unknown => return,
                Shortcut::Chord(label) => {
                    self.search.set_text("Search");
                    // Parsed from the label the registration reported, so the
                    // menu cannot disagree with what was registered — the two
                    // used to be written down separately, in different files,
                    // and only one of them was ever right.
                    match label.parse::<Accelerator>() {
                        Ok(accelerator) => {
                            let _ = self.search.set_accelerator(Some(accelerator));
                        }
                        Err(error) => {
                            // A chord this menu cannot render is still a working
                            // shortcut; name it in the text rather than dropping
                            // it and implying there is none.
                            koru_ipc::ipc_debug!(
                                "tray: {label} is not a menu accelerator: {error}"
                            );
                            let _ = self.search.set_accelerator(None);
                            self.search.set_text(format!("Search ({label})"));
                        }
                    }
                }
                Shortcut::Absent => {
                    let _ = self.search.set_accelerator(None);
                    self.search.set_text("Search (no global shortcut)");
                }
            }
            self.shortcut = shortcut.clone();
        }

        pub fn set_ai_shortcut(&mut self, available: bool) {
            if self.ai_shortcut == Some(available) {
                return;
            }
            let _ = self.ai_chat.set_accelerator(if available {
                "Ctrl+Shift+I".parse::<Accelerator>().ok()
            } else {
                None
            });
            self.ai_chat.set_text(if available {
                "Koru AI Chat"
            } else {
                "Koru AI Chat (no global shortcut)"
            });
            self.ai_shortcut = Some(available);
        }
    }
}

/// The tray picture on macOS and Linux, compiled in — tracked in git, not a
/// fetched asset, so a fresh clone builds. See the icon note in
/// `imp::Tray::show`.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../../../assets/logo.png");

/// **Linux: a StatusNotifierItem**, served over D-Bus by `ksni`.
///
/// The same menu `imp` builds, derived from [`super::MenuState`] by
/// [`super::linux_menu`] — see there for the two rows Linux leaves out and
/// why. The setters keep that state and push it to the service only when it
/// moved; the host asks for the layout when it opens the menu.
///
/// `None` when no notification-area host is listening — vanilla GNOME without
/// the AppIndicator extension. `ksni` asks the StatusNotifierWatcher at spawn
/// and fails when there is none, so the Agent then runs as it did before, with
/// the console line telling the person how to stop it.
///
/// ## Three things that differ from `tray-icon`
///
/// **Clicks arrive on `ksni`'s own thread.** Its service runs on a thread it
/// spawns, and an item's `activate` closure runs there with the service's lock
/// held — so, as in `imp`, a click only carries its ask through the [`Waker`]
/// and the loop does the work. Nothing here may block in a closure: the menu
/// would freeze until it returned.
///
/// **An underscore is a mnemonic.** DBusMenu reads `_x` as *x is the access
/// key* and drops the underscore, the way Windows reads `&`; a workspace named
/// `my_notes` would show as `mynotes`. Every label is escaped on its way out —
/// see [`escape`] — which is `imp`'s `&` → `&&` rule in this protocol's
/// spelling.
///
/// **Left click is `activate`**, and it raises [`TrayAsk::IconClick`] — the
/// Windows gesture: bring back the workspace window in use, or Search. Hosts
/// decide whether a left click activates or opens the menu; GNOME's
/// AppIndicator extension may open the menu, which is a host's convention the
/// Agent does not fight.
#[cfg(target_os = "linux")]
mod linux {
    use super::{MenuRow, MenuState, Shortcut, Surfaces, VoiceMenu};
    use crate::event::{AgentEvent, TrayAsk, Waker};
    use crate::registry::OpenWorkspace;
    use ksni::blocking::TrayMethods;

    /// What the service thread owns: the state the menu is derived from, and
    /// what a click needs to raise its ask.
    struct Item {
        state: MenuState,
        tooltip: String,
        icon: Option<ksni::Icon>,
        waker: Waker,
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "koru-agent".into()
        }
        fn title(&self) -> String {
            "Koru Agent".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.iter().cloned().collect()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: self.tooltip.clone(),
                ..Default::default()
            }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            self.waker.wake(AgentEvent::Tray(TrayAsk::IconClick));
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            super::linux_menu(&self.state)
                .into_iter()
                .map(native)
                .collect()
        }
    }

    /// One [`MenuRow`] as a `ksni` item. A pressable row gets a closure that
    /// raises its ask and nothing else; a reading or a greyed row gets one
    /// that does nothing — see [`raise`].
    fn native(row: MenuRow) -> ksni::MenuItem<Item> {
        match row {
            MenuRow::Separator => ksni::MenuItem::Separator,
            MenuRow::Item {
                label,
                enabled,
                ask,
                shortcut,
            } => ksni::menu::StandardItem {
                label: escape(&label),
                enabled,
                shortcut: shortcut.map(|press| vec![press]).unwrap_or_default(),
                activate: raise(ask.filter(|_| enabled)),
                ..Default::default()
            }
            .into(),
            // **The tick is not flipped here.** `ksni` leaves `checked` to the
            // tray, and the loop answers the ask by reading the real state
            // fresh — `imp`'s read-back rule — so the tick changes when the
            // next setter reports it, never on the strength of the click.
            MenuRow::Check {
                label,
                enabled,
                checked,
                ask,
            } => ksni::menu::CheckmarkItem {
                label: escape(&label),
                enabled,
                checked,
                activate: raise(Some(ask).filter(|_| enabled)),
                ..Default::default()
            }
            .into(),
            MenuRow::Submenu { label, rows } => ksni::menu::SubMenu {
                label: escape(&label),
                submenu: rows.into_iter().map(native).collect(),
                ..Default::default()
            }
            .into(),
        }
    }

    /// The closure a row carries: wake the loop with its ask, or do nothing.
    ///
    /// **A greyed row is given nothing to raise**, rather than trusted not to
    /// be clicked: `ksni` passes a `clicked` event to the closure whatever the
    /// item's `enabled` says, so the guarantee `tray-icon` gives `imp` — a
    /// disabled row raises no event — is made here, not left to the host.
    fn raise(ask: Option<TrayAsk>) -> Box<dyn Fn(&mut Item) + Send> {
        match ask {
            Some(ask) => {
                Box::new(move |item: &mut Item| item.waker.wake(AgentEvent::Tray(ask.clone())))
            }
            None => Box::new(|_: &mut Item| {}),
        }
    }

    /// A label as DBusMenu should draw it: every `_` doubled, so none is read
    /// as an access-key marker. See the module note.
    pub(super) fn escape(label: &str) -> String {
        label.replace('_', "__")
    }

    /// The same compiled-in picture macOS uses, as the ARGB32 big-endian
    /// pixmap the protocol carries.
    fn icon() -> Option<ksni::Icon> {
        let rgba = image::load_from_memory(super::ICON_PNG).ok()?.to_rgba8();
        let (width, height) = rgba.dimensions();
        let mut data = rgba.into_raw();
        for pixel in data.chunks_exact_mut(4) {
            pixel.rotate_right(1);
        }
        Some(ksni::Icon {
            width: i32::try_from(width).ok()?,
            height: i32::try_from(height).ok()?,
            data,
        })
    }

    /// A live icon. **Dropping it removes the icon**, as `imp`'s does.
    pub struct Tray {
        handle: ksni::blocking::Handle<Item>,
        /// What the service was last given — the setters' equality guard.
        ///
        /// **A guard, not a nicety.** The loop calls every setter on every
        /// wake, and each `Handle::update` is a blocking round trip to the
        /// service thread that re-derives the menu and diffs it; nothing here
        /// should cost a D-Bus conversation when nothing changed — `imp`'s
        /// rule for native calls.
        state: MenuState,
    }

    impl Tray {
        pub fn show(segment: &str, root: Option<&std::path::Path>, waker: Waker) -> Option<Self> {
            let info = koru_update::build_info();
            let lang =
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
            // Read once, as `imp` reads each of these when it builds its menu;
            // the setters correct whatever moves afterwards.
            let state = MenuState {
                lang,
                surfaces: Surfaces {
                    ai: koru_features::ai(),
                    reminders: koru_features::reminders(),
                    agents: koru_features::agents(),
                },
                shortcut: Shortcut::Unknown,
                ai_shortcut: None,
                store_running: None,
                voice: super::voice_state(root, koru_voice::InputChoice::SystemDefault),
                dictation_shortcut: Shortcut::Unknown,
                workspaces: Vec::new(),
                agents: Vec::new(),
                relay: super::relay_state(),
                relay_status: super::relay_connection(),
                about: super::about_line(info.version, info.build_number, info.build_label()),
                about_enabled: super::release_notes_url().is_some(),
            };
            let item = Item {
                state: state.clone(),
                tooltip: super::tooltip(info.version, segment, root, super::branch()),
                icon: icon(),
                waker,
            };
            match item.spawn() {
                Ok(handle) => Some(Self { handle, state }),
                Err(error) => {
                    tracing::info!(%error, "no notification-area host; running without an icon");
                    None
                }
            }
        }

        /// Hand the service the current state. Called only after a setter has
        /// seen its field move.
        fn push(&self) {
            let state = self.state.clone();
            // `None` is a service that has shut down. There is nothing to
            // update and nothing the loop could do about it.
            let _ = self.handle.update(move |item| item.state = state);
        }

        pub fn set_workspaces(&mut self, rows: &[OpenWorkspace]) {
            if self.state.workspaces == rows {
                return;
            }
            self.state.workspaces = rows.to_vec();
            self.push();
        }

        pub fn set_agent_sessions(&mut self, catalog: &crate::agents_menu::AgentsCatalog) {
            let lines = super::agents_menu_lines(catalog, self.state.lang);
            if self.state.agents == lines {
                return;
            }
            self.state.agents = lines;
            self.push();
        }

        pub fn set_store_running(&mut self, running: bool) {
            if self.state.store_running == Some(running) {
                return;
            }
            self.state.store_running = Some(running);
            self.push();
        }

        pub fn set_voice(&mut self, state: &VoiceMenu) {
            if &self.state.voice == state {
                return;
            }
            self.state.voice = state.clone();
            self.push();
        }

        pub fn set_relay(&mut self, state: Option<bool>) {
            if self.state.relay == state {
                return;
            }
            self.state.relay = state;
            self.push();
        }

        pub fn set_relay_status(&mut self, state: koru_network_iroh::RelayConnectionStatus) {
            if self.state.relay_status == state {
                return;
            }
            self.state.relay_status = state;
            self.push();
        }

        /// `Unknown` leaves the row as it was — `imp`'s rule: saying anything
        /// before the hotkey thread answers is guessing.
        pub fn set_shortcut(&mut self, shortcut: &Shortcut) {
            if &self.state.shortcut == shortcut || *shortcut == Shortcut::Unknown {
                return;
            }
            self.state.shortcut = shortcut.clone();
            self.push();
        }

        pub fn set_dictation_shortcut(&mut self, shortcut: &Shortcut) {
            if &self.state.dictation_shortcut == shortcut {
                return;
            }
            self.state.dictation_shortcut = shortcut.clone();
            self.push();
        }

        pub fn set_ai_shortcut(&mut self, available: bool) {
            if self.state.ai_shortcut == Some(available) {
                return;
            }
            self.state.ai_shortcut = Some(available);
            self.push();
        }

        /// A no-op: Linux has no Links submenu (see [`super::linux_menu`]).
        /// Kept so the loop calls every platform's tray the same way.
        pub fn set_url_scheme(&mut self, _state: &koru_url_scheme::Registration) {}
    }

    impl Drop for Tray {
        /// Unregister from the host, so the icon goes when the Agent does
        /// rather than when its D-Bus connection happens to close. Not waited
        /// for: a quitting Agent has nothing to gain from blocking on it.
        fn drop(&mut self) {
            let _ = self.handle.shutdown();
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod stub {
    use crate::event::Waker;
    use crate::registry::OpenWorkspace;

    /// No notification area here. `None` is not a degraded mode: it is a
    /// platform where an agent simply runs headless.
    pub struct Tray;

    impl Tray {
        pub fn show(
            _segment: &str,
            _root: Option<&std::path::Path>,
            _waker: Waker,
        ) -> Option<Self> {
            None
        }

        pub fn set_workspaces(&mut self, _rows: &[OpenWorkspace]) {}
        pub fn set_agent_sessions(&mut self, _catalog: &crate::agents_menu::AgentsCatalog) {}
        pub fn set_store_running(&mut self, _running: bool) {}
        pub fn set_voice(&mut self, _state: &super::VoiceMenu) {}
        pub fn set_relay(&mut self, _state: Option<bool>) {}
        pub fn set_relay_status(&mut self, _state: koru_network_iroh::RelayConnectionStatus) {}
        pub fn set_shortcut(&mut self, _shortcut: &super::Shortcut) {}
        pub fn set_dictation_shortcut(&mut self, _shortcut: &super::Shortcut) {}
        pub fn set_ai_shortcut(&mut self, _available: bool) {}
        pub fn set_url_scheme(&mut self, _state: &koru_url_scheme::Registration) {}
    }
}

/// What is known about the global shortcut, which is three things and not two.
///
/// **`Absent` and `Unknown` must not collapse into one.** Registration happens
/// on its own thread and the menu is built before it answers, so for a moment
/// there genuinely is no answer — and drawing *Search (no global shortcut)*
/// then, only to correct it a heartbeat later, is a flicker that reads as a
/// bug. An `Option` would have made those two states the same value, which is
/// the mistake the previous hardcoded accelerator made in the other direction:
/// it was never unsure, and it was often wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Shortcut {
    /// The hotkey thread has not reported yet. Draw nothing, say nothing.
    #[default]
    Unknown,
    /// It reported that there is none — every candidate was taken, or this
    /// agent has an explicit `--root` and never asked for one.
    Absent,
    /// This chord opens Search, and it is what was actually registered.
    Chord(String),
}

/// One line of the **Agents** submenu — the grouped projection of the session
/// host's catalog, as a value the native menu is built from and a test can
/// hold. See [`agents_menu_lines`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AgentsLine {
    Separator,
    /// A group's name — a project, or the Unassigned bucket. Drawn disabled:
    /// it is a heading, not an action, and a greyed native row raises no menu
    /// event at all (the About row's rule).
    Header(String),
    /// A running session. Pressing it asks the loop to show it in Koru
    /// Workspace.
    Session {
        session: koru_session_protocol::SessionId,
        label: String,
    },
}

/// **The submenu's grouped rows, as a pure function of the catalog.**
///
/// The grouping mirrors the Agents navigator's (`apps/koru/src/panes/
/// explore.rs::rebuild_agent_rows`), so the tray and the pane cannot tell one
/// story two ways: projects ordered by case-folded name then id, sessions
/// within a group by creation time then id, and a session whose project id
/// names nothing the registry has — or that nobody placed — files under
/// **Unassigned**, last. Two deliberate differences, both because this is a
/// flat menu rather than a managed tree:
///
/// - **every project is shown**, whatever workspace it is scoped to — the
///   notification area is device-level and has no current workspace to filter
///   by, and hiding a scoped project here would hide its running sessions
///   entirely;
/// - **a project with no sessions draws no heading.** The navigator shows
///   empty projects because they are management targets (rename, delete, New
///   session ▸); a menu heading with nothing under it offers nothing.
///
/// Each session row is labelled by its profile's display name — the same
/// `koru_session_profiles` projection the tab strip renders — so the two
/// surfaces cannot disagree about what a profile is called.
pub(crate) fn agents_menu_lines(
    catalog: &crate::agents_menu::AgentsCatalog,
    lang: koru_i18n::Lang,
) -> Vec<AgentsLine> {
    use std::collections::{HashMap, HashSet};

    let known: HashSet<koru_session_protocol::ProjectId> =
        catalog.projects.iter().map(|project| project.id).collect();
    let mut grouped: HashMap<
        Option<koru_session_protocol::ProjectId>,
        Vec<&koru_session_protocol::SessionSummary>,
    > = HashMap::new();
    for session in &catalog.sessions {
        let group = match session.project_id {
            Some(id) if known.contains(&id) => Some(id),
            _ => None,
        };
        grouped.entry(group).or_default().push(session);
    }
    for sessions in grouped.values_mut() {
        sessions.sort_by_key(|session| (session.created_at_ms, session.id));
    }

    let mut projects: Vec<_> = catalog.projects.iter().collect();
    projects.sort_by_key(|project| (project.name.to_lowercase(), project.id));

    let mut lines = Vec::new();
    let mut append = |name: &str, sessions: Vec<&koru_session_protocol::SessionSummary>| {
        if sessions.is_empty() {
            return;
        }
        lines.push(AgentsLine::Separator);
        lines.push(AgentsLine::Header(name.to_owned()));
        for session in sessions {
            lines.push(AgentsLine::Session {
                session: session.id,
                label: session_row_label(session),
            });
        }
    };
    for project in projects {
        append(
            &project.name,
            grouped.remove(&Some(project.id)).unwrap_or_default(),
        );
    }
    // The bucket sits last — the navigator's rule: it is where things have
    // not been put yet, and first would read as the default place.
    append(
        koru_i18n::t(lang, koru_i18n::Key::TerminalUnassigned),
        grouped.remove(&None).unwrap_or_default(),
    );
    lines
}

/// What a session row says: the profile's display name where the profile
/// still resolves, and the stable key's own spelling where it does not (a
/// built-in this build no longer ships). A custom profile has no name this
/// process can know, so it gets the honest generic — the same rule
/// `apps/koru`'s `session_profile_label` states for the tab strip.
fn session_row_label(session: &koru_session_protocol::SessionSummary) -> String {
    match koru_session_profiles::resolve(&session.profile) {
        Ok(profile) => profile.fallback_display_name().to_owned(),
        Err(_) => match &session.profile {
            koru_session_protocol::SessionProfileKey::BuiltIn(token) => token.as_str().to_owned(),
            koru_session_protocol::SessionProfileKey::Custom(_) => "Session".to_owned(),
        },
    }
}

/// What the icon can ask for, as a list — so a reader can see that Quit is
/// always offered, which is the rule this module exists to keep.
#[cfg(test)]
pub(crate) const OFFERED: &[TrayAsk] = &[
    TrayAsk::AiChat,
    TrayAsk::Search,
    TrayAsk::Reminders,
    // Offered on every platform and in every state. On an installation with
    // dictation switched off the submenu carries one line saying so and no
    // action at all, which is a different thing from the menu not carrying it:
    // a person has to be able to find out that the feature exists and that it
    // is off here.
    TrayAsk::Voice,
    TrayAsk::VoiceSettings,
    // **Two, not one.** Freeing the weights and ending the process have
    // different costs, so they are separate rows and separate asks; see
    // `voice_menu`.
    TrayAsk::VoiceRelease,
    TrayAsk::VoiceStop,
    TrayAsk::NewAgent,
    // The two Links rows and the backup row are appended on Windows only —
    // registration is bundle-level work on macOS and the folder picker is a
    // threading redesign there; Linux's reasons are at `linux_menu`.
    #[cfg(windows)]
    TrayAsk::RegisterUrlScheme,
    #[cfg(windows)]
    TrayAsk::UnregisterUrlScheme,
    TrayAsk::Restore,
    TrayAsk::OpenLocation,
    TrayAsk::OpenLocalDataLocation,
    TrayAsk::OpenSharedLocation,
    TrayAsk::OpenGatewayDocs,
    TrayAsk::RefreshStore,
    TrayAsk::OpenStoreLocation,
    TrayAsk::OpenStoreStats,
    #[cfg(windows)]
    TrayAsk::BackupStore,
    // The relay switch (iroh-relay decision №6): on both tray platforms,
    // because the device network runs wherever the tray does. Greyed, not
    // absent, on an agent whose network never started.
    TrayAsk::ToggleRelay,
    TrayAsk::About,
    TrayAsk::CheckForUpdates,
    TrayAsk::SendFeedback,
    TrayAsk::Quit,
];

/// Who opens `koru://` links, as this executable sees it.
///
/// The agent is what a registration names, because the agent is what routes a
/// target to a window -- see `route.rs`. A shell launched with the target on
/// its command line would bypass window reuse entirely.
pub(crate) fn url_scheme_state() -> koru_url_scheme::Registration {
    match std::env::current_exe() {
        Ok(exe) => koru_url_scheme::state(&exe),
        // Nothing can be said about a registration without knowing which
        // executable is asking, and guessing would name the wrong build.
        Err(_) => koru_url_scheme::Registration::Unsupported,
    }
}

/// What the Links submenu's status row says.
///
/// A switch that claims nothing is a switch nobody can trust: every channel
/// installs side by side and only one of them opens `koru://`, so the state has
/// to be readable at the moment somebody is deciding whether to press the row
/// beneath it.
///
/// Windows and macOS only, with its sibling below: Linux draws no Links
/// submenu (see [`linux_menu`]).
#[cfg(any(windows, target_os = "macos", test))]
fn url_scheme_line(state: &koru_url_scheme::Registration) -> String {
    use koru_url_scheme::Registration;
    match state {
        Registration::Absent => "koru:// links: not handled".to_owned(),
        Registration::ThisBuild => "koru:// links: this build".to_owned(),
        // Named, not "another build": which one it is decides whether pressing
        // the row below is what somebody wants.
        Registration::Other { exe } => {
            let path = std::path::Path::new(exe);
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_else(|| exe.as_str().into());
            let parent = path
                .parent()
                .and_then(std::path::Path::file_name)
                .map(|name| name.to_string_lossy());
            match parent {
                Some(parent) => format!("koru:// links: {parent}/{file}"),
                None => format!("koru:// links: {file}"),
            }
        }
        Registration::Foreign { .. } => "koru:// links: another program".to_owned(),
        Registration::Unsupported => "koru:// links: not on this platform".to_owned(),
    }
}

/// What the action row beneath it offers.
#[cfg(any(windows, target_os = "macos", test))]
fn url_scheme_action(state: &koru_url_scheme::Registration) -> &'static str {
    match state {
        koru_url_scheme::Registration::ThisBuild => "Stop handling koru:// links",
        _ => "Handle koru:// links",
    }
}

/// How the *Use relay* row is drawn for a state.
///
/// `Some(enabled)` is the network's cached preference; `None` is an agent
/// whose device network never started — a device with no sync identity yet.
/// There is nothing to switch then, so the row is greyed rather than left
/// out (the About row's rule), and a greyed native row raises no menu event,
/// which is what makes the unpressable state safe.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct RelayRow {
    pub(crate) checked: bool,
    pub(crate) enabled: bool,
}

/// The tick and the pressability, as a pure function a test can hold.
pub(crate) fn relay_row(state: Option<bool>) -> RelayRow {
    RelayRow {
        checked: state == Some(true),
        enabled: state.is_some(),
    }
}

/// The relay preference as this process is applying it **now** — the
/// network's cached answer ([`crate::network::AgentNetwork::relay_enabled`]),
/// never a disk read, so it is safe on every path that refreshes the menu.
pub(crate) fn relay_state() -> Option<bool> {
    crate::network::current().map(crate::network::AgentNetwork::relay_enabled)
}

/// The status row's text, as a pure function a test can hold: `<host> —
/// <state>`, the host never translated, the state word from the i18n table
/// and the separator living here. Off carries no host worth naming — the
/// unticked switch above already says which switch — so it is the localized
/// whole-row text alone.
pub(crate) fn relay_status_line(
    lang: koru_i18n::Lang,
    state: &koru_network_iroh::RelayConnectionStatus,
) -> String {
    use koru_network_iroh::RelayConnectionStatus as Status;
    let (host, word) = match state {
        Status::Off => return koru_i18n::t(lang, koru_i18n::Key::RelayStateOff).to_owned(),
        Status::Connecting { host } => (host, koru_i18n::Key::RelayStateConnecting),
        Status::Connected { host } => (host, koru_i18n::Key::RelayStateConnected),
        Status::Disconnected { host } => (host, koru_i18n::Key::RelayStateDisconnected),
    };
    format!("{host} — {}", koru_i18n::t(lang, word))
}

/// The relay connection as the runtime reads it **now** — the network
/// status snapshot's relay field, tracked event-driven inside
/// `koru-network-iroh` from the endpoint's own home-relay watcher. The
/// snapshot clone is a cheap read taken once per wake; nothing here polls
/// or dials. An agent whose device network never started has no relay
/// connection to speak of, which is what `Off` (the default) says.
pub(crate) fn relay_connection() -> koru_network_iroh::RelayConnectionStatus {
    crate::network::current()
        .map(|network| network.runtime().status().relay)
        .unwrap_or_default()
}

/// **What the notification area can say about dictation.**
///
/// The tray is the one surface that is there when no window is, which is what
/// makes it the place a model gets installed from — and the row is both the way
/// in and an honest status display, so the state has to be a value rather than
/// a sentence assembled at the menu.
///
/// Four states and no fifth. `Unavailable` is `koru_voice::off()` — the
/// `KORU_VOICE_DISABLED` switch, or a platform with no voice yet (Linux) — the
/// one predicate the worker's supervisor, the welcome hint and the `/dictate`
/// row also ask, so the four surfaces cannot answer it four ways (Q-V44). It
/// carries which, because the two are different sentences.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VoiceRow {
    /// This installation offers no dictation. The submenu says so and offers
    /// nothing; nothing is dialled and no worker is started.
    Unavailable(koru_voice::Off),
    /// Nothing is installed yet.
    ///
    /// `megabytes` is what the download costs, when that is known — it comes
    /// from a catalog entry, and a build that has not read one says *Download
    /// model…* with no figure rather than inventing a plausible number.
    NotInstalled { megabytes: Option<u64> },
    /// An install is running. Percent, not bytes: a menu row is not a progress
    /// bar and `koru_voice_models::InstallProgress` is already coalesced to at
    /// most one report per whole percent.
    Downloading { percent: u8 },
    /// A model is on the disk and a dictation can begin.
    Installed,
}

/// **Everything the *Voice input* submenu draws**, as one value.
///
/// Three readings of different things, kept apart because they are taken at
/// different moments and by different means:
///
/// - [`VoiceMenu::row`] and [`VoiceMenu::installed`] come off the **disk**,
///   offline, and are what [`voice_state`] answers.
/// - [`VoiceMenu::worker`] comes off the **socket**, and only if something is
///   already listening on it. It is `None` when nothing is.
///
/// Keeping them in one struct is what makes every label below a pure function
/// of a value a test can build by hand.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VoiceMenu {
    pub row: VoiceRow,
    /// Which quality tiers have verified bytes on this machine, cheapest
    /// first. Derived from the cache every time it is read, for the reason
    /// `koru_voice_models::anything_installed` gives at length: a stored flag
    /// eventually answers *installed* for a directory another channel pruned.
    pub installed: Vec<koru_voice_models::ModelTier>,
    /// What a running worker last said about itself, or `None` when there is
    /// no worker. **`None` is not a failure**: nothing here starts one.
    pub worker: Option<VoiceWorker>,
    /// **Which microphones a running worker said it can see**, and empty when
    /// none has said. Read off the socket beside [`VoiceMenu::worker`] and
    /// never enumerated in this process: `cpal` is linked in exactly one
    /// binary in this workspace and this is not it.
    ///
    /// **Empty is *nothing has said*, not *there are no microphones*.** The
    /// submenu draws *System default* and whatever name is remembered, and no
    /// sentence about hardware it has not been told about.
    pub devices: Vec<koru_voice::InputDevice>,
    /// Which microphone the person chose, read from the roaming preferences
    /// the shell writes.
    ///
    /// **Read, never written.** `koru_window::user_preferences` is explicit
    /// that the shell is the only writer of `preferences.conf`, so pressing a
    /// row here routes `Push::VoiceInput` to a window rather than editing the
    /// file behind it.
    pub input: koru_voice::InputChoice,
}

/// The three things a running worker tells the menu, and nothing else.
///
/// **Not the status report.** `koru-voice-worker status` answers with ring overruns, a
/// sample rate, stage timings, sessions this run and the capture device's
/// category, and none of those belongs on a menu somebody opened to find out
/// whether dictation works: they are diagnostics, and
/// [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) says a line a reader could have
/// predicted without reading it does not earn its place. The device's *name* is
/// not even in the report — `docs/contributing/operations/LOGGING.md` keeps a person's
/// hardware out of everything — and nothing here would have made it visible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VoiceWorker {
    /// Which of the four states the session machine is in. `Idle` is a worker
    /// sitting there; anything else is a dictation somebody is in the middle
    /// of.
    pub state: koru_voice::State,
    /// Whether the weights are resident **now**. This is what *Release model
    /// from memory* acts on, and what greys it when there is nothing to
    /// release.
    pub model_loaded: bool,
    /// The last code this worker produced, or `None` when it has produced
    /// none. See [`voice_menu`]: silent when healthy.
    pub last_error: Option<koru_voice::ErrorCode>,
}

impl VoiceMenu {
    /// A fresh reading of the disk, keeping what the worker last said.
    ///
    /// The two are re-read on different events — the disk whenever something
    /// could have changed it, the worker only when the agent is already
    /// dialling it — so a disk read must not silently claim there is no
    /// worker. `Unavailable` is the exception and drops it: an installation
    /// with dictation switched off has nothing to say about a worker, and
    /// carrying a stale one would let the submenu offer *Stop* on a build that
    /// may not start anything.
    pub fn keeping_worker(mut self, worker: Option<VoiceWorker>) -> VoiceMenu {
        if !matches!(self.row, VoiceRow::Unavailable(_)) {
            self.worker = worker;
        }
        self
    }

    /// A fresh reading of the disk, keeping the device list too.
    ///
    /// Same reasoning as [`keeping_worker`](Self::keeping_worker) and the same
    /// exception: a disk read says nothing about what is plugged in, so
    /// blanking the list on one would empty the picker every time a model was
    /// installed.
    pub fn keeping_devices(mut self, devices: Vec<koru_voice::InputDevice>) -> VoiceMenu {
        if !matches!(self.row, VoiceRow::Unavailable(_)) {
            self.devices = devices;
        }
        self
    }
}

/// Which slot a status line fills, so the native menu reuses one item per slot
/// rather than minting a new one on every rebuild.
///
/// A line's *identity* is what it is about, not where it happens to sit: the
/// live line is absent most of the time and the error line nearly always is, so
/// counting positions would rename items every time one appeared.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum VoiceStatus {
    /// A dictation is happening right now.
    Live,
    /// The one-line answer to *does this work, and with what*.
    Summary,
    /// The chord, from `show_hint`.
    Chord,
    /// The last failure's sentence.
    Error,
}

/// What pressing a row asks the loop for.
///
/// **Three asks, not one.** [`VoiceAsk::Act`] is the existing "do whatever the
/// state means"; the other two are deliberately separate rows and deliberately
/// separate asks — see [`voice_menu`].
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum VoiceAsk {
    /// Start a dictation, or open the setup step — decided by the loop from a
    /// fresh reading, never by the label this row was given.
    Act,
    /// Open the Agent-hosted Voice surface without starting a session.
    Settings,
    /// Drop the model and keep working.
    Release,
    /// End the worker process.
    Stop,
}

/// One row of the *Input* submenu: a microphone, and whether it is the one
/// being listened with.
///
/// `choice` is what pressing it asks for; `label` is what it says. They are
/// separate because *System default* is a choice with no name, and because a
/// device that is remembered and not connected says more than its name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct InputRow {
    pub(crate) choice: koru_voice::InputChoice,
    pub(crate) label: String,
    /// Drawn with a tick. Exactly one row carries it — see [`input_rows`].
    pub(crate) chosen: bool,
}

/// One line of the submenu.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum VoiceLine {
    /// Says something and cannot be pressed.
    Status(VoiceStatus, String),
    Separator,
    /// Can be pressed, unless `enabled` is false — in which case it is drawn
    /// greyed and **raises no menu event at all**, which is the About row's
    /// rule and the mechanism that makes an unpressable row safe.
    Action {
        ask: VoiceAsk,
        label: String,
        enabled: bool,
    },
    /// The *Input* submenu, whole. One line rather than a run of them, because
    /// the rows are as many as the machine has microphones and the native side
    /// has to mint them per rebuild — the same arrangement the open-workspace
    /// rows already use.
    Input(Vec<InputRow>),
}

/// **Which microphones the *Input* submenu offers**, and which one is ticked.
///
/// A pure function of the state, which is the point: the tray is a surface
/// nobody can drive from a test, so everything it says has to be decided
/// somewhere that can be.
///
/// Three rules, and each of them is a decision:
///
/// **`System default` is always first and always offered.** It is a choice
/// somebody makes — *follow the OS when I change headsets* — and not the
/// absence of one. A picker where it were merely what you got by not choosing
/// would give somebody who had picked a device no way back.
///
/// **A remembered device that is not in the list is still offered, and still
/// ticked.** Dropping it would move the tick to *System default* and leave
/// nothing to put the choice back with, which is a silent correction of
/// something the person said. Its label says it is not connected, so the tick
/// is not a lie about what is being listened to.
///
/// **Exactly one row is ticked**, always — `the_input_submenu_ticks_exactly_one`
/// holds it, because a menu with two ticks or none is a menu that has stopped
/// saying what the state is.
pub(crate) fn input_rows(state: &VoiceMenu) -> Vec<InputRow> {
    let chosen = &state.input;
    let mut rows = vec![InputRow {
        choice: koru_voice::InputChoice::SystemDefault,
        label: "System default".to_owned(),
        chosen: chosen.name().is_none(),
    }];
    for device in &state.devices {
        let choice = koru_voice::InputChoice::named(&device.name);
        rows.push(InputRow {
            chosen: &choice == chosen,
            choice,
            label: device.name.clone(),
        });
    }
    if let Some(wanted) = chosen.name()
        && !state.devices.iter().any(|d| d.name == wanted)
    {
        rows.push(InputRow {
            choice: chosen.clone(),
            label: format!("{wanted} (not connected)"),
            chosen: true,
        });
    }
    rows
}

/// **The submenu's own title**, which carries what is being listened with.
///
/// The name rather than a count, because the question somebody opens this to
/// answer is *which microphone*, and a title saying *Input (3)* answers a
/// different one.
pub(crate) fn input_title(state: &VoiceMenu) -> String {
    match state.input.name() {
        None => "Input: System default".to_owned(),
        Some(name) if state.devices.iter().any(|d| d.name == name) => format!("Input: {name}"),
        // **The fall-back, said in the title.** Otherwise the only way to find
        // out that a headset is asleep is to open the submenu, and the person
        // affected is exactly the one who would not think to.
        Some(name) => format!("Input: {name} (not connected)"),
    }
}

/// **The submenu, as a list of lines** — a pure function of the state and the
/// chord.
///
/// Tested from fixtures, with no tray, no worker and no model cache: what it
/// says is the whole of the tray's contribution to discovery, and it is the
/// half that has to be right on a machine nobody can run a menu on.
///
/// ## What is in it
///
/// ```text
/// Listening…                          only while a dictation is live
/// Ready · High accuracy · in memory   what works, and with what
/// Ctrl+Shift+D                        only when show_hint gives one
/// <the last failure's sentence>       only when there has been one
/// ─────────────
/// Start dictation                     or: Set voice input up…
/// ─────────────
/// Release model from memory           greyed when nothing is resident
/// Stop voice worker                   greyed when nothing is running
/// ```
///
/// ## Two actions, deliberately not one toggle
///
/// *Release model from memory* frees several hundred megabytes and leaves voice
/// working — the next dictation pays the model load again, which is seconds
/// somebody will feel. *Stop voice worker* ends the process. Collapsing them
/// into one *Free memory* row would hide which of the two you were getting, and
/// one of them has a cost next time.
///
/// **Both are drawn greyed rather than left out** when there is nothing for
/// them to do, which is the About row's rule: the label is the status, and a
/// row that vanishes answers *why is there no way to stop this* with silence.
/// A greyed native row raises no menu event, so nothing can reach the loop for
/// one.
///
/// ## The error line is silent when healthy
///
/// *Koru is not allowed to use the microphone…* is the difference between
/// broken and fixable, and it is the one sentence worth a line of a menu. When
/// there has been no failure there is no line — a status that said *no errors*
/// is exactly the line a reader could have predicted without reading it.
///
/// ## What is deliberately not here
///
/// Ring overruns, the sample rate, stage timings, sessions this run and the
/// capture device: those are `koru-voice-worker status` and the log. See
/// [`VoiceWorker`].
#[cfg(test)]
pub(crate) fn voice_menu(state: &VoiceMenu, chord: Option<&str>) -> Vec<VoiceLine> {
    voice_menu_localized(state, chord, koru_i18n::Lang::En)
}

fn voice_menu_localized(
    state: &VoiceMenu,
    chord: Option<&str>,
    lang: koru_i18n::Lang,
) -> Vec<VoiceLine> {
    // **One line, and nothing that could dial or start anything.** A build with
    // `KORU_VOICE_DISABLED` set offers no dictation at all, so a submenu of
    // greyed rows would be four ways of saying the same thing.
    if let VoiceRow::Unavailable(off) = state.row {
        return vec![VoiceLine::Status(
            VoiceStatus::Summary,
            unavailable_line(off, lang),
        )];
    }

    let mut lines = Vec::new();
    if let Some(live) = state.worker.as_ref().and_then(|w| live_line(w.state)) {
        lines.push(VoiceLine::Status(VoiceStatus::Live, live.to_owned()));
    }
    lines.push(VoiceLine::Status(VoiceStatus::Summary, summary_line(state)));
    // Only where there is something for it to start. A chord printed beside
    // *no model yet* teaches a gesture that would open a download.
    if matches!(state.row, VoiceRow::Installed)
        && let Some(chord) = chord
    {
        lines.push(VoiceLine::Status(VoiceStatus::Chord, chord.to_owned()));
    }
    if let Some(code) = state.worker.as_ref().and_then(|w| w.last_error) {
        // **Plan 2.5's sentences, not new ones.** The mapping is the code's own
        // (`koru_voice::ErrorCode::sentence_key`), so the notification area and
        // the document's refusal toast cannot disagree about what a code means.
        // English, explicitly: every string in this menu is English until the
        // i18n pass the owner has taken as their own work (Q-V69, Q-V75).
        lines.push(VoiceLine::Status(
            VoiceStatus::Error,
            koru_i18n::t(koru_i18n::Lang::En, code.sentence_key()).to_owned(),
        ));
    }

    lines.push(VoiceLine::Separator);
    lines.push(VoiceLine::Action {
        ask: VoiceAsk::Act,
        label: act_label(&state.row).to_owned(),
        // Every state that gets this far has something to do: start a
        // dictation, or open the setup step. `Unavailable` returned above.
        enabled: true,
    });
    lines.push(VoiceLine::Action {
        ask: VoiceAsk::Settings,
        label: koru_i18n::t(lang, koru_i18n::Key::VoiceGlobalOpen).to_owned(),
        enabled: true,
    });

    // **Above the two memory actions and below the one that starts a
    // dictation**, because it is about how a dictation happens rather than
    // about the process that serves it.
    lines.push(VoiceLine::Separator);
    lines.push(VoiceLine::Input(input_rows(state)));

    lines.push(VoiceLine::Separator);
    lines.push(VoiceLine::Action {
        ask: VoiceAsk::Release,
        label: "Release model from memory".to_owned(),
        enabled: state.worker.as_ref().is_some_and(|w| w.model_loaded),
    });
    lines.push(VoiceLine::Action {
        ask: VoiceAsk::Stop,
        label: "Stop voice worker".to_owned(),
        enabled: state.worker.is_some(),
    });
    lines
}

/// The line that says a dictation is happening, or `None` when none is.
///
/// **`Idle` draws nothing**, which is what makes *Listening…* mean something: a
/// row that were always there, saying *Idle* most of the time, is activity
/// rather than news.
fn live_line(state: koru_voice::State) -> Option<&'static str> {
    match state {
        koru_voice::State::Idle => None,
        // *Starting* covers both the model load and the device opening, which
        // is the same word `apps/koru`'s dictation chip uses for the same two
        // waits.
        koru_voice::State::Loading => Some("Starting…"),
        koru_voice::State::Listening => Some("Listening…"),
        koru_voice::State::Finalising => Some("Finishing…"),
    }
}

/// Why there is no voice, in one line. A platform without it gets the sentence
/// Settings shows, localized; the switch keeps its own, which names what a
/// developer set.
fn unavailable_line(off: koru_voice::Off, lang: koru_i18n::Lang) -> String {
    match off {
        koru_voice::Off::Platform => {
            koru_i18n::t(lang, koru_i18n::Key::VoiceNotOnLinuxYet).to_owned()
        }
        koru_voice::Off::Switch => "Voice input is switched off on this installation".to_owned(),
    }
}

/// **Does this work, and with what** — one line.
fn summary_line(state: &VoiceMenu) -> String {
    match &state.row {
        // Answered by the caller, and repeated here so this stays total rather
        // than relying on a `return` several screens up.
        VoiceRow::Unavailable(off) => unavailable_line(*off, koru_i18n::Lang::En),
        VoiceRow::NotInstalled { megabytes: None } => "Not set up — no model yet".to_owned(),
        VoiceRow::NotInstalled {
            megabytes: Some(mb),
        } => format!("Not set up — a {mb} MB download"),
        VoiceRow::Downloading { percent } => format!("Downloading the voice model… {percent}%"),
        VoiceRow::Installed => {
            let mut line = "Ready".to_owned();
            if let Some(quality) = quality_line(&state.installed) {
                line.push_str(" · ");
                line.push_str(&quality);
            }
            // **The residency is news and the process is not.** A model in
            // memory explains where several hundred megabytes went; a worker
            // with nothing loaded explains why the next dictation pauses before
            // it hears anything. No worker at all is the ordinary state and
            // says nothing.
            match &state.worker {
                Some(worker) if worker.model_loaded => line.push_str(" · model in memory"),
                Some(_) => line.push_str(" · model not loaded"),
                None => {}
            }
            line
        }
    }
}

/// The installed tiers, written the way the Settings section writes them.
///
/// **A tier, never a model name.** `koru_voice_models::ModelTier`'s own note is
/// the rule: a model name is `parakeet-tdt-0.6b-v3`, which tells a person
/// nothing and changes under them when the catalog moves on, while a tier is
/// the trade they chose and survives the model behind it being replaced.
fn quality_line(tiers: &[koru_voice_models::ModelTier]) -> Option<String> {
    if tiers.is_empty() {
        return None;
    }
    Some(
        tiers
            .iter()
            .map(|tier| tier_label(*tier))
            .collect::<Vec<_>>()
            .join(" + "),
    )
}

/// The two words the Settings section already uses, so one product does not
/// name a quality two ways.
fn tier_label(tier: koru_voice_models::ModelTier) -> &'static str {
    match tier {
        koru_voice_models::ModelTier::Compact => "Compact",
        koru_voice_models::ModelTier::HighAccuracy => "High accuracy",
    }
}

/// What the one action row offers.
///
/// It is a label, not a decision: the loop re-reads the state and works out
/// what the press meant, because a model can arrive — or be removed by another
/// channel's process — while the menu sits there. See [`TrayAsk::Voice`].
fn act_label(row: &VoiceRow) -> &'static str {
    match row {
        VoiceRow::Unavailable(_) => "Voice input is unavailable",
        VoiceRow::NotInstalled { .. } => "Set voice input up…",
        VoiceRow::Downloading { .. } => "Show the download",
        VoiceRow::Installed => "Start dictation",
    }
}

/// **The submenu's own stable title.**
///
/// The state and chord live inside the submenu, beside their actions. Keeping
/// the parent as *Voice input* in every state avoids a long, shifting tray row.
fn voice_line(_row: &VoiceRow) -> String {
    "Voice input".to_owned()
}

/// The chord that dictates, written for this platform.
///
/// **From the registry, never spelled here.** `reference/SHORTCUTS.md`,
/// the welcome hint, the `/dictate` row and this row all render
/// `ShortcutId::Dictate`, so the label follows the binding if it is ever
/// changed and is right on macOS without a second table.
///
/// `None` means *draw nothing* — [`voice_menu`] omits the line rather than
/// appending an empty one, which is the whole purpose of the switch.
#[cfg(test)]
pub(crate) fn dictate_chord() -> Option<String> {
    koru_keys::show_hint(koru_keys::ShortcutId::Dictate, koru_keys::Section::Menu)
}

/// What the submenu should say **now**, asked of the switch and the model
/// cache — and of nothing else.
///
/// Two questions and no third: whether this installation offers dictation at
/// all, and what is on the disk. [`VoiceRow::Downloading`] is not reachable
/// from here — the install runs in a window, not in the agent (Q-V67).
///
/// **Nothing here starts a worker, and nothing here can.** The cache is read
/// through `DiskModelStore::offline`, which builds no HTTP client, and the
/// worker is not dialled at all: [`VoiceMenu::worker`] is filled in separately,
/// by a caller that connects to whatever is *already* listening. A menu drawn
/// when nothing is running has to be answerable when nothing is running.
///
/// **Installed state is derived from the disk, every time.** There is no flag
/// beside the consent record saying a model is here, deliberately: a `prune`
/// from another channel or a person with a file manager would leave it
/// answering *installed* for a cache that is gone, and nothing would fail until
/// a dictation did. The tiers and the installed/not-installed answer come off
/// **one** walk of that cache, because they are two readings of the same list —
/// `koru_voice_models::anything_installed` is `installed()` non-empty, and
/// asking twice would walk the directory twice to learn the same thing.
/// `input` is **the authority's answer, handed in** — it used to be read here,
/// out of `preferences.conf`, by a fourth parser of that file which tolerated
/// neither a byte-order mark, nor a comment, nor a space before the `=`. The
/// caller holds a live settings link; this walks the model cache and nothing
/// else. See ADR-266.
pub(crate) fn voice_state(
    root: Option<&std::path::Path>,
    input: koru_voice::InputChoice,
) -> VoiceMenu {
    if let Some(off) = koru_voice::off() {
        return VoiceMenu {
            row: VoiceRow::Unavailable(off),
            installed: Vec::new(),
            worker: None,
            devices: Vec::new(),
            input: koru_voice::InputChoice::SystemDefault,
        };
    }
    let installed = installed_tiers(root);
    let row = if installed.is_empty() {
        VoiceRow::NotInstalled { megabytes: None }
    } else {
        VoiceRow::Installed
    };
    VoiceMenu {
        row,
        installed,
        worker: None,
        // Filled from the socket, separately and off this thread -- see
        // `VoiceMenu::devices`.
        devices: Vec::new(),
        input,
    }
}

/// Which tiers have verified bytes here, cheapest first.
///
/// A cache that cannot be read at all answers *nothing installed*, which is the
/// same reading `anything_installed` takes and is what a person can act on; the
/// specific failure belongs in the log beside it.
fn installed_tiers(root: Option<&std::path::Path>) -> Vec<koru_voice_models::ModelTier> {
    use koru_voice_models::ModelStore;
    let Ok(paths) = koru_voice_models::ModelPaths::resolve(root) else {
        return Vec::new();
    };
    let Ok(models) = koru_voice_models::DiskModelStore::offline(paths).installed() else {
        return Vec::new();
    };
    koru_voice_models::ModelTier::ALL
        .into_iter()
        .filter(|tier| models.iter().any(|model| model.descriptor.tier == *tier))
        .collect()
}

/// How the About row names this build.
///
/// **`build 0` is a number nobody allocated.** A local cargo build has no build
/// number and never had one, so it names what it actually is — the commit when
/// `koru-release` stamped one, otherwise `debug` / `release` — which is
/// `BuildInfo::build_label`, the same fallback Welcome's "Current:" line uses.
/// The two therefore agree, which is the point of not spelling it a second time
/// here.
fn about_line(version: &str, build_number: u64, label: &str) -> String {
    match build_number {
        0 => format!("Koru {version} · {label}"),
        n => format!("Koru {version} · build {n}"),
    }
}

/// This build's immutable release-notes page, or `None` when it has none.
///
/// **Nothing is fetched to work this out.** `koru-release` uploads
/// `releases/<version>/index.html` beside the package for every release and
/// refuses to replace one that exists, so the page for a version is fixed the
/// moment that version is — and the URL follows from the version alone, with no
/// manifest to consult and no network call on a menu click. (The `+build` in a
/// nightly version is literal in a URL *path*; `+` only means a space in a query
/// string, so there is nothing to encode.)
///
/// **A build that was never published has no page**, and this answers `None`
/// there rather than a URL that would 404. Two questions are asked because they
/// are two different ones: `updates_enabled` is *was this built by the release
/// tool* — a cargo build has no build number and no public key — and
/// `has_update_feed` is *does this channel publish anywhere at all*, which
/// Internal does not.
///
/// Public because the tray only decides *whether* to offer the row; opening a
/// URL is the agent's, and it is the caller that knows how this platform does
/// that.
pub fn release_notes_url() -> Option<String> {
    let info = koru_update::build_info();
    let published =
        info.updates_enabled() && koru_channel::ReleaseChannel::of_this_build().has_update_feed();
    notes_url(info.version, published)
}

/// The testable half of [`release_notes_url`], with the two build questions
/// already answered.
fn notes_url(version: &str, published: bool) -> Option<String> {
    let version = version.trim();
    (published && !version.is_empty())
        .then(|| format!("https://updates.koru.do/releases/{version}/index.html"))
}

fn branch() -> Option<&'static str> {
    option_env!("KORU_BRANCH")
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// What hovering the icon says: the build, then where this agent is running.
///
/// **The version leads.** Everything after it answers *which agent* — an
/// explicit root, or the channel, plus an internal build's branch — and those
/// arms are unchanged; what is new is that the icon now also answers *which
/// build*, which is a question somebody with stable and nightly installed side
/// by side asks far more often than they open a menu.
fn tooltip(
    version: &str,
    segment: &str,
    root: Option<&std::path::Path>,
    branch: Option<&str>,
) -> String {
    let base = match root {
        Some(root) => format!("Koru Agent {version} — {}", root.display()),
        None => format!("Koru Agent {version} — {segment}"),
    };
    match branch.filter(|_| segment.starts_with("internal-")) {
        Some(branch) => format!("{base} — {branch}"),
        None => base,
    }
}

/// **Everything the Linux menu draws, as one value.**
///
/// `imp` keeps this state in native items it relabels one by one; a
/// StatusNotifierItem's menu is instead *asked for* — the host calls
/// `GetLayout` over D-Bus and `ksni` answers with `ksni::Tray::menu` — so
/// Linux keeps the state and derives the whole menu from it with
/// [`linux_menu`]. That makes the menu a pure function a test can hold, which
/// is the property the shared helpers below already have.
///
/// Each field is what one of `Tray`'s setters reports, stored the way `imp`
/// stores its equality guards, so the setters can skip a D-Bus round trip
/// when nothing moved.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct MenuState {
    /// The menu's language, decided once when the icon is shown — `imp`'s
    /// rule.
    pub(crate) lang: koru_i18n::Lang,
    /// Which surfaces this build shows (`koru-features`), read once at show
    /// like `imp`'s.
    pub(crate) surfaces: Surfaces,
    pub(crate) shortcut: Shortcut,
    /// `None` until the hotkey thread reports, like `imp`'s `ai_shortcut`.
    pub(crate) ai_shortcut: Option<bool>,
    /// `None` until supervision first reports — *Checking…*.
    pub(crate) store_running: Option<bool>,
    pub(crate) voice: VoiceMenu,
    pub(crate) dictation_shortcut: Shortcut,
    pub(crate) workspaces: Vec<crate::registry::OpenWorkspace>,
    /// [`agents_menu_lines`]' projection, stored rather than the catalog: it
    /// is what `imp` compares, and all the menu needs.
    pub(crate) agents: Vec<AgentsLine>,
    pub(crate) relay: Option<bool>,
    pub(crate) relay_status: koru_network_iroh::RelayConnectionStatus,
    /// [`about_line`]'s text, and whether this build has a release-notes page
    /// to open — decided once, as `imp` decides it.
    pub(crate) about: String,
    pub(crate) about_enabled: bool,
}

/// The `koru-features` switches the menu consults, as a value — so a test can
/// build the simplified product without touching process state.
#[cfg(any(target_os = "linux", test))]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) struct Surfaces {
    pub(crate) ai: bool,
    pub(crate) reminders: bool,
    pub(crate) agents: bool,
}

/// One row of the Linux menu, before it is a `ksni` item.
///
/// **`ask` is what pressing raises, and `None` is a reading** — a heading or a
/// status line. A row can carry an ask and still be greyed (*Release model
/// from memory* with nothing loaded), and then pressing it raises nothing —
/// the guarantee `imp` gets from `tray-icon`, which `linux` makes itself.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum MenuRow {
    Separator,
    Item {
        label: String,
        enabled: bool,
        ask: Option<TrayAsk>,
        /// The registered chord, in DBusMenu's shape — see [`dbus_shortcut`].
        shortcut: Option<Vec<String>>,
    },
    Check {
        label: String,
        enabled: bool,
        checked: bool,
        ask: TrayAsk,
    },
    Submenu {
        label: String,
        rows: Vec<MenuRow>,
    },
}

#[cfg(any(target_os = "linux", test))]
impl MenuRow {
    /// A pressable row.
    fn action(label: impl Into<String>, ask: TrayAsk) -> MenuRow {
        MenuRow::Item {
            label: label.into(),
            enabled: true,
            ask: Some(ask),
            shortcut: None,
        }
    }

    /// A row that says something and cannot be pressed.
    fn reading(label: impl Into<String>) -> MenuRow {
        MenuRow::Item {
            label: label.into(),
            enabled: false,
            ask: None,
            shortcut: None,
        }
    }
}

/// **The Linux menu, as a pure function of what the tray has been told.**
///
/// The same rows, in the same order, with the same labels and the same asks as
/// `imp::Tray::show` builds — every label comes from the helpers `imp` uses, so
/// the two cannot tell different stories. Two rows are deliberately absent,
/// each for a reason that is Linux's own:
///
/// - **Backup Store…** — its folder picker is `rfd`, which this crate links on
///   Windows only, and `service::backup_store` is a no-op everywhere else. On
///   Linux `rfd`'s default backend is gtk3 — exactly the weight ADR-118 kept
///   `tray-icon` off Linux for — so a row here would be a button that does
///   nothing. `imp` leaves it out of the macOS menu for the same shape of
///   reason.
/// - **Links** — `koru://` registration on Linux is a `.desktop` file and
///   `xdg-mime`, desktop-integration work for a later rung;
///   `koru_url_scheme::state` answers `Unsupported` here, and a submenu whose
///   only action cannot act is furniture (ADR-118: prefer omission).
///
/// Quit is last and unconditional — the rule this module exists to keep.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linux_menu(state: &MenuState) -> Vec<MenuRow> {
    let lang = state.lang;
    let mut rows = Vec::new();
    if state.surfaces.ai {
        rows.push(ai_chat_row(state.ai_shortcut));
    }
    rows.push(search_row(&state.shortcut));
    if state.surfaces.reminders {
        rows.push(MenuRow::action(
            koru_i18n::t(lang, koru_i18n::Key::ReminderAll),
            TrayAsk::Reminders,
        ));
    }
    rows.push(MenuRow::Submenu {
        label: voice_line(&state.voice.row),
        rows: voice_rows(&state.voice, &state.dictation_shortcut, lang),
    });
    let mut workspaces = vec![MenuRow::action("Open Workspace", TrayAsk::Restore)];
    if !state.workspaces.is_empty() {
        workspaces.push(MenuRow::Separator);
        for row in &state.workspaces {
            workspaces.push(MenuRow::action(
                row.label.clone(),
                TrayAsk::OpenWorkspace(row.connection),
            ));
        }
    }
    rows.push(MenuRow::Submenu {
        label: "Workspaces".to_owned(),
        rows: workspaces,
    });
    if state.surfaces.agents {
        let mut agents = vec![MenuRow::action(
            koru_i18n::t(lang, koru_i18n::Key::TerminalNew),
            TrayAsk::NewAgent,
        )];
        for line in &state.agents {
            agents.push(match line {
                AgentsLine::Separator => MenuRow::Separator,
                AgentsLine::Header(name) => MenuRow::reading(name.clone()),
                AgentsLine::Session { session, label } => {
                    MenuRow::action(label.clone(), TrayAsk::ShowAgentSession(*session))
                }
            });
        }
        rows.push(MenuRow::Submenu {
            label: koru_i18n::t(lang, koru_i18n::Key::TerminalAgents).to_owned(),
            rows: agents,
        });
    }
    rows.push(MenuRow::Submenu {
        label: "API Gateway".to_owned(),
        rows: vec![MenuRow::action("Docs", TrayAsk::OpenGatewayDocs)],
    });
    rows.push(MenuRow::Submenu {
        label: "Store".to_owned(),
        rows: vec![
            // Pressable, as in `imp`: pressing the status asks for a fresh one.
            MenuRow::action(
                match state.store_running {
                    None => "Store running: Checking…",
                    Some(true) => "Store running: Yes",
                    Some(false) => "Store running: No",
                },
                TrayAsk::RefreshStore,
            ),
            MenuRow::Separator,
            MenuRow::action("Open store stats", TrayAsk::OpenStoreStats),
            MenuRow::action("Open store location", TrayAsk::OpenStoreLocation),
        ],
    });
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::Submenu {
        label: "Data locations".to_owned(),
        rows: vec![
            MenuRow::action("Show root", TrayAsk::OpenLocation),
            MenuRow::action("Show local data", TrayAsk::OpenLocalDataLocation),
            MenuRow::action("Show remote data", TrayAsk::OpenSharedLocation),
        ],
    });
    let relay = relay_row(state.relay);
    rows.push(MenuRow::Check {
        label: koru_i18n::t(lang, koru_i18n::Key::RelayUse).to_owned(),
        enabled: relay.enabled,
        checked: relay.checked,
        ask: TrayAsk::ToggleRelay,
    });
    rows.push(MenuRow::reading(relay_status_line(
        lang,
        &state.relay_status,
    )));
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::Submenu {
        label: "About Koru".to_owned(),
        rows: vec![
            MenuRow::Item {
                label: state.about.clone(),
                enabled: state.about_enabled,
                ask: Some(TrayAsk::About),
                shortcut: None,
            },
            MenuRow::action("Send feedback", TrayAsk::SendFeedback),
        ],
    });
    rows.push(MenuRow::action(
        "Check for updates",
        TrayAsk::CheckForUpdates,
    ));
    rows.push(MenuRow::Separator);
    rows.push(MenuRow::action("Quit Koru Agent", TrayAsk::Quit));
    rows
}

/// The Search row, saying what `imp::Tray::set_shortcut` makes it say.
///
/// **The chord is DBusMenu's `shortcut` property**, the counterpart of `imp`'s
/// accelerator, parsed from the label the registration reported for the same
/// reason: the menu must not be able to name a chord nobody registered. One the
/// property cannot carry is written into the text, as `imp` does with one
/// `Accelerator` cannot parse. Whether the host draws the property is the
/// host's business; not every one does.
#[cfg(any(target_os = "linux", test))]
fn search_row(shortcut: &Shortcut) -> MenuRow {
    let (label, shortcut) = match shortcut {
        Shortcut::Unknown => ("Search".to_owned(), None),
        Shortcut::Chord(chord) => match dbus_shortcut(chord) {
            Some(keys) => ("Search".to_owned(), Some(keys)),
            None => (format!("Search ({chord})"), None),
        },
        Shortcut::Absent => ("Search (no global shortcut)".to_owned(), None),
    };
    MenuRow::Item {
        label,
        enabled: true,
        ask: Some(TrayAsk::Search),
        shortcut,
    }
}

/// The Koru AI Chat row, saying what `imp::Tray::set_ai_shortcut` makes it
/// say. `None` is *not reported yet* and draws the plain label, as `imp`'s
/// freshly built row does.
#[cfg(any(target_os = "linux", test))]
fn ai_chat_row(available: Option<bool>) -> MenuRow {
    let (label, shortcut) = match available {
        None => ("Koru AI Chat", None),
        Some(true) => ("Koru AI Chat", dbus_shortcut("Ctrl+Shift+I")),
        Some(false) => ("Koru AI Chat (no global shortcut)", None),
    };
    MenuRow::Item {
        label: label.to_owned(),
        enabled: true,
        ask: Some(TrayAsk::AiChat),
        shortcut,
    }
}

/// The *Voice input* submenu's rows, from [`voice_menu_localized`] — the lines
/// `imp`'s `fill_voice` draws, with each [`VoiceAsk`] raised as the same
/// [`TrayAsk`] `imp`'s handler maps it to.
#[cfg(any(target_os = "linux", test))]
fn voice_rows(state: &VoiceMenu, dictation: &Shortcut, lang: koru_i18n::Lang) -> Vec<MenuRow> {
    let chord = match dictation {
        Shortcut::Chord(label) => Some(label.as_str()),
        Shortcut::Unknown | Shortcut::Absent => None,
    };
    voice_menu_localized(state, chord, lang)
        .into_iter()
        .map(|line| match line {
            VoiceLine::Separator => MenuRow::Separator,
            VoiceLine::Status(_, text) => MenuRow::reading(text),
            VoiceLine::Input(inputs) => MenuRow::Submenu {
                label: input_title(state),
                rows: inputs
                    .into_iter()
                    .map(|row| MenuRow::Check {
                        label: row.label,
                        enabled: true,
                        checked: row.chosen,
                        ask: TrayAsk::ChooseVoiceInput(row.choice),
                    })
                    .collect(),
            },
            VoiceLine::Action {
                ask,
                label,
                enabled,
            } => MenuRow::Item {
                label,
                enabled,
                ask: Some(match ask {
                    VoiceAsk::Act => TrayAsk::Voice,
                    VoiceAsk::Settings => TrayAsk::VoiceSettings,
                    VoiceAsk::Release => TrayAsk::VoiceRelease,
                    VoiceAsk::Stop => TrayAsk::VoiceStop,
                }),
                shortcut: None,
            },
        })
        .collect()
}

/// A chord label (`Ctrl+Shift+K`) as one DBusMenu key press — modifiers
/// first, then the key, with the modifiers spelled the way the property
/// requires (`Control`, `Alt`, `Shift`, `Super`).
///
/// `None` for anything that is not modifiers-then-one-key, so the caller can
/// say the chord in the text instead of drawing a wrong one.
#[cfg(any(target_os = "linux", test))]
fn dbus_shortcut(label: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = label.split('+').map(str::trim).collect();
    let (key, modifiers) = parts.split_last()?;
    if key.is_empty() || modifiers.is_empty() {
        return None;
    }
    let mut press = modifiers
        .iter()
        .map(|modifier| match *modifier {
            "Ctrl" | "Control" => Some("Control"),
            "Alt" => Some("Alt"),
            "Shift" => Some("Shift"),
            "Win" | "Super" | "Cmd" => Some("Super"),
            _ => None,
        })
        .map(|modifier| modifier.map(str::to_owned))
        .collect::<Option<Vec<String>>>()?;
    press.push((*key).to_owned());
    Some(press)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The seam where the two used to drift.** `hotkey.rs` decides which
    /// chord is registered and reports it as a label; this menu renders that
    /// label. They live in different files, so nothing but this test stops a
    /// candidate being added there that the menu cannot draw — which would
    /// silently leave *Search* with no accelerator on exactly the machines
    /// where the fall-back mattered.
    ///
    /// Gated like `tray-icon` itself, which is where `Accelerator` lives.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn every_shortcut_the_hotkey_can_register_is_one_this_menu_can_draw() {
        use tray_icon::menu::accelerator::Accelerator;

        for action in crate::hotkey::ACTIONS {
            for label in crate::hotkey::candidates(*action) {
                assert!(
                    label.parse::<Accelerator>().is_ok(),
                    "{label} is registered by hotkey.rs but cannot be drawn in the menu"
                );
            }
        }
    }

    #[test]
    fn every_selected_global_dictation_label_reaches_the_voice_menu_exactly() {
        let state = ready();
        for label in crate::hotkey::DICTATION_CANDIDATES {
            assert!(voice_menu(&state, Some(label)).iter().any(|line| {
                matches!(line, VoiceLine::Status(VoiceStatus::Chord, shown) if shown == label)
            }));
        }
        assert!(
            !voice_menu(&state, None)
                .iter()
                .any(|line| matches!(line, VoiceLine::Status(VoiceStatus::Chord, _)))
        );
    }

    /// **A switch that claims nothing is a switch nobody can trust.** Every
    /// channel installs side by side and only one of them opens `koru://`, so
    /// the row has to say which -- and name it, because *which* build owns the
    /// scheme is what decides whether pressing the row below is what somebody
    /// wants.
    #[test]
    fn the_row_names_each_registration_state() {
        use koru_url_scheme::Registration;

        assert_eq!(
            url_scheme_line(&Registration::Absent),
            "koru:// links: not handled"
        );
        assert_eq!(
            url_scheme_line(&Registration::ThisBuild),
            "koru:// links: this build"
        );
        assert!(
            url_scheme_line(&Registration::Other {
                exe: "C:\\Koru-nightly\\koru-agent.exe".into()
            })
            .contains("Koru-nightly"),
            "another build is named, not merely reported"
        );
        assert_eq!(
            url_scheme_line(&Registration::Foreign {
                command: "other.exe".into()
            }),
            "koru:// links: another program"
        );
        assert_eq!(
            url_scheme_line(&Registration::Unsupported),
            "koru:// links: not on this platform"
        );
    }

    /// One row, two meanings. Only *this build's* registration is ours to
    /// remove, and a platform that cannot register offers nothing to press.
    #[test]
    fn the_action_offers_to_stop_only_what_this_build_started() {
        use koru_url_scheme::Registration;

        assert_eq!(
            url_scheme_action(&Registration::ThisBuild),
            "Stop handling koru:// links"
        );
        for state in [
            Registration::Absent,
            Registration::Other { exe: "x".into() },
            Registration::Foreign {
                command: "x".into(),
            },
            Registration::Unsupported,
        ] {
            assert_eq!(url_scheme_action(&state), "Handle koru:// links");
        }
    }

    /// On Windows. On macOS the Links submenu is deliberately absent —
    /// registration is bundle-level work there, deferred with packaging — and
    /// on a platform with no rows the offer would be a lie.
    #[cfg(windows)]
    #[test]
    fn the_icon_offers_to_handle_koru_links() {
        assert!(OFFERED.contains(&TrayAsk::RegisterUrlScheme));
        assert!(OFFERED.contains(&TrayAsk::UnregisterUrlScheme));
    }

    /// **The tick is the state, and only a live network can be switched.**
    /// Checked follows the network's cached answer; an agent whose device
    /// network never started draws the row greyed rather than dropping it —
    /// the About row's rule — and never ticked, because a tick on a switch
    /// that does nothing would claim relays are in use.
    #[test]
    fn the_relay_row_follows_the_network_state() {
        assert_eq!(
            relay_row(Some(true)),
            RelayRow {
                checked: true,
                enabled: true
            }
        );
        assert_eq!(
            relay_row(Some(false)),
            RelayRow {
                checked: false,
                enabled: true
            }
        );
        assert_eq!(
            relay_row(None),
            RelayRow {
                checked: false,
                enabled: false
            }
        );
    }

    /// Decision №6: the switch ships in the agent menu, on both platforms
    /// the tray exists on.
    #[test]
    fn the_icon_offers_the_relay_switch() {
        assert!(OFFERED.contains(&TrayAsk::ToggleRelay));
    }

    /// **The status row names the relay and its state.** `<host> — <state>`
    /// for every connection reading, the host untranslated; off is the
    /// localized whole-row text alone, because the unticked switch above it
    /// already names the switch.
    #[test]
    fn the_relay_status_row_names_the_host_and_the_state() {
        use koru_network_iroh::RelayConnectionStatus as Status;
        let lang = koru_i18n::Lang::En;
        let host = || "use1-1.relay.koru.do".to_owned();
        assert_eq!(
            relay_status_line(lang, &Status::Connected { host: host() }),
            "use1-1.relay.koru.do — connected"
        );
        assert_eq!(
            relay_status_line(lang, &Status::Connecting { host: host() }),
            "use1-1.relay.koru.do — connecting"
        );
        assert_eq!(
            relay_status_line(lang, &Status::Disconnected { host: host() }),
            "use1-1.relay.koru.do — disconnected"
        );
        assert_eq!(relay_status_line(lang, &Status::Off), "Relay off");
        // Every language fills every state — the words come from the
        // compile-enforced i18n table, so one spot check per shape is
        // enough to pin the assembly.
        assert_eq!(
            relay_status_line(koru_i18n::Lang::De, &Status::Connected { host: host() }),
            "use1-1.relay.koru.do — verbunden"
        );
        assert_eq!(
            relay_status_line(koru_i18n::Lang::Ja, &Status::Off),
            "リレーはオフ"
        );
    }

    /// The compiled-in macOS picture decodes into a real image — asserted on
    /// every platform, because the bytes are in the binary either way and a
    /// broken PNG would otherwise be found by the first Mac to show an icon.
    #[test]
    fn the_macos_icon_decodes() {
        let rgba = image::load_from_memory(ICON_PNG)
            .expect("assets/logo.png decodes")
            .to_rgba8();
        assert!(rgba.width() > 0 && rgba.height() > 0);
    }

    /// Three states, not two: *not yet answered* is not *there is none*.
    #[test]
    fn the_menu_starts_out_saying_nothing_about_the_shortcut() {
        assert_eq!(Shortcut::default(), Shortcut::Unknown);
        assert_ne!(Shortcut::Unknown, Shortcut::Absent);
    }

    /// An installation with a model on the disk, a compact one, and nothing
    /// else. The fixture every test below starts from, so what each one is
    /// about is the *one field it changes*.
    fn ready() -> VoiceMenu {
        VoiceMenu {
            row: VoiceRow::Installed,
            installed: vec![koru_voice_models::ModelTier::Compact],
            worker: None,
            devices: Vec::new(),
            input: koru_voice::InputChoice::SystemDefault,
        }
    }

    /// A state with nothing installed and no worker — the shape every
    /// not-yet-set-up fixture starts from.
    fn menu(row: VoiceRow) -> VoiceMenu {
        VoiceMenu {
            row,
            installed: Vec::new(),
            worker: None,
            devices: Vec::new(),
            input: koru_voice::InputChoice::SystemDefault,
        }
    }

    /// A worker sitting there with the weights resident and nothing wrong.
    fn idle_worker() -> VoiceWorker {
        VoiceWorker {
            state: koru_voice::State::Idle,
            model_loaded: true,
            last_error: None,
        }
    }

    /// Every status line the menu draws, in order — what somebody reads.
    fn status_lines(state: &VoiceMenu, chord: Option<&str>) -> Vec<String> {
        voice_menu(state, chord)
            .into_iter()
            .filter_map(|line| match line {
                VoiceLine::Status(_, text) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Every action the menu offers, and whether it can be pressed.
    fn actions(state: &VoiceMenu, chord: Option<&str>) -> Vec<(VoiceAsk, String, bool)> {
        voice_menu(state, chord)
            .into_iter()
            .filter_map(|line| match line {
                VoiceLine::Action {
                    ask,
                    label,
                    enabled,
                } => Some((ask, label, enabled)),
                _ => None,
            })
            .collect()
    }

    /// **The title stays compact and stable.** State belongs in the submenu's
    /// summary and action rows, where it can be explained without stretching
    /// the notification-area menu.
    #[test]
    fn the_voice_title_is_stable_in_every_state() {
        for row in [
            VoiceRow::Unavailable(koru_voice::Off::Switch),
            VoiceRow::NotInstalled { megabytes: None },
            VoiceRow::NotInstalled {
                megabytes: Some(650),
            },
            VoiceRow::Downloading { percent: 42 },
            VoiceRow::Installed,
        ] {
            assert_eq!(voice_line(&row), "Voice input");
        }
    }

    /// **The summary answers *does this work, and with what*** — one line per
    /// state, and each of them a different sentence.
    #[test]
    fn the_summary_reads_differently_in_every_state() {
        let summary = |row: VoiceRow, installed: Vec<koru_voice_models::ModelTier>| {
            summary_line(&VoiceMenu {
                row,
                installed,
                ..menu(VoiceRow::Installed)
            })
        };
        assert_eq!(
            summary(VoiceRow::Unavailable(koru_voice::Off::Switch), Vec::new()),
            "Voice input is switched off on this installation"
        );
        assert_eq!(
            summary(VoiceRow::NotInstalled { megabytes: None }, Vec::new()),
            "Not set up — no model yet"
        );
        assert_eq!(
            summary(
                VoiceRow::NotInstalled {
                    megabytes: Some(57)
                },
                Vec::new()
            ),
            "Not set up — a 57 MB download"
        );
        assert_eq!(
            summary(VoiceRow::Downloading { percent: 42 }, Vec::new()),
            "Downloading the voice model… 42%"
        );
        assert_eq!(
            summary(
                VoiceRow::Installed,
                vec![koru_voice_models::ModelTier::Compact]
            ),
            "Ready · Compact"
        );
        assert_eq!(
            summary(
                VoiceRow::Installed,
                vec![koru_voice_models::ModelTier::HighAccuracy]
            ),
            "Ready · High accuracy",
            "the tier, never the model's name — see ModelTier"
        );
        assert_eq!(
            summary(
                VoiceRow::Installed,
                koru_voice_models::ModelTier::ALL.into()
            ),
            "Ready · Compact + High accuracy"
        );
        assert_eq!(
            summary(VoiceRow::Installed, Vec::new()),
            "Ready",
            "a quality nothing declares is left unsaid rather than guessed"
        );
    }

    /// **The chord is the registry's, and `None` draws nothing.**
    ///
    /// It sits beside the row that uses it, so it is only printed where there
    /// is something for it to start: a chord under *no model yet* would teach a
    /// gesture that opens a download.
    #[test]
    fn the_chord_is_printed_only_where_it_would_start_something() {
        assert!(
            status_lines(&ready(), Some("Ctrl+Shift+D")).contains(&"Ctrl+Shift+D".to_owned()),
            "an installation that can dictate teaches the chord"
        );
        assert!(
            status_lines(&ready(), Some("⇧⌘D")).contains(&"⇧⌘D".to_owned()),
            "written the way the platform writes it, not assembled here"
        );
        // `show_hint` answering `None` means *draw nothing*, and an empty row is
        // not nothing.
        for line in status_lines(&ready(), None) {
            assert!(!line.is_empty());
        }
        assert_eq!(
            status_lines(&ready(), None).len(),
            status_lines(&ready(), Some("Ctrl+Shift+D")).len() - 1,
            "exactly one line goes away, and no empty one takes its place"
        );
        let nothing_to_start = VoiceMenu {
            row: VoiceRow::NotInstalled { megabytes: None },
            ..ready()
        };
        assert!(
            !status_lines(&nothing_to_start, Some("Ctrl+Shift+D"))
                .contains(&"Ctrl+Shift+D".to_owned())
        );
    }

    /// **From the registry, never spelled here.** The row, the welcome hint,
    /// the `/dictate` menu row and `SHORTCUTS.md` all render one binding, which
    /// is what makes the label right on macOS and what makes it follow the
    /// chord if it is ever changed.
    #[test]
    fn the_chord_the_row_teaches_is_the_one_that_is_bound() {
        assert_eq!(
            dictate_chord(),
            koru_keys::show_hint(koru_keys::ShortcutId::Dictate, koru_keys::Section::Menu)
        );
        assert_eq!(
            dictate_chord().as_deref(),
            Some(koru_keys::registry::label_of(koru_keys::ShortcutId::Dictate).as_str())
        );
    }

    /// **A live dictation says so, and a finished one says nothing.**
    ///
    /// The line that is always there, reading *Idle* most of the time, is
    /// activity rather than news — and it is what would make *Listening…* mean
    /// nothing when it appeared.
    #[test]
    fn a_live_session_says_so_and_a_finished_one_does_not() {
        let with = |state| VoiceMenu {
            worker: Some(VoiceWorker {
                state,
                ..idle_worker()
            }),
            ..ready()
        };
        assert!(
            status_lines(&with(koru_voice::State::Listening), None)
                .contains(&"Listening…".to_owned())
        );
        assert!(
            status_lines(&with(koru_voice::State::Loading), None).contains(&"Starting…".to_owned())
        );
        assert!(
            status_lines(&with(koru_voice::State::Finalising), None)
                .contains(&"Finishing…".to_owned())
        );
        let finished = status_lines(&with(koru_voice::State::Idle), None);
        assert!(
            !finished.iter().any(|line| line.contains("Listening")),
            "a dictation that ended leaves no line behind: {finished:?}"
        );
        assert!(
            !finished.iter().any(|line| line.contains("Idle")),
            "and nothing announces that nothing is happening: {finished:?}"
        );
        assert!(
            !status_lines(&ready(), None)
                .iter()
                .any(|line| line.contains("Listening")),
            "no worker at all is not a live session"
        );
    }

    /// **Silent when healthy, and Plan 2.5's sentence when not.**
    ///
    /// *Microphone blocked in Windows settings* is the difference between
    /// broken and fixable; *no errors* is a line a reader could have predicted
    /// without reading it.
    #[test]
    fn an_error_shows_its_sentence_and_a_healthy_worker_shows_none() {
        let failing = VoiceMenu {
            worker: Some(VoiceWorker {
                last_error: Some(koru_voice::ErrorCode::PermissionDenied),
                ..idle_worker()
            }),
            ..ready()
        };
        let sentence = koru_i18n::t(
            koru_i18n::Lang::En,
            koru_voice::ErrorCode::PermissionDenied.sentence_key(),
        );
        assert!(
            status_lines(&failing, None).contains(&sentence.to_owned()),
            "the permission sentence names where the switch is"
        );
        assert!(
            sentence.contains("Microphone"),
            "and it is the platform sentence rather than a new one: {sentence}"
        );
        // Every code has a sentence, and the menu draws whichever one it is
        // handed — walked rather than spot-checked, so a code added later
        // cannot be the one nobody printed.
        for code in koru_voice::ErrorCode::ALL {
            let state = VoiceMenu {
                worker: Some(VoiceWorker {
                    last_error: Some(code),
                    ..idle_worker()
                }),
                ..ready()
            };
            let expected = koru_i18n::t(koru_i18n::Lang::En, code.sentence_key());
            assert!(
                status_lines(&state, None).contains(&expected.to_owned()),
                "{code} draws no sentence"
            );
        }
        let healthy = VoiceMenu {
            worker: Some(idle_worker()),
            ..ready()
        };
        assert_eq!(
            status_lines(&healthy, None),
            vec!["Ready · Compact · model in memory".to_owned()],
            "a working installation says one thing and nothing about errors"
        );
    }

    /// **Two actions, deliberately not one toggle.** Releasing the weights
    /// leaves voice working and costs the next dictation a model load;
    /// stopping ends the process. They are separate rows and separate asks, so
    /// nothing has to guess which somebody meant.
    #[test]
    fn releasing_the_model_and_stopping_the_worker_are_two_different_rows() {
        let running = VoiceMenu {
            worker: Some(idle_worker()),
            ..ready()
        };
        let offered = actions(&running, None);
        assert_eq!(
            offered
                .iter()
                .map(|(ask, label, _)| (*ask, label.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (VoiceAsk::Act, "Start dictation"),
                (
                    VoiceAsk::Settings,
                    koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::VoiceGlobalOpen),
                ),
                (VoiceAsk::Release, "Release model from memory"),
                (VoiceAsk::Stop, "Stop voice worker"),
            ]
        );
        assert!(
            offered.iter().all(|(_, _, enabled)| *enabled),
            "a running worker with the model resident can use all four actions"
        );
    }

    /// **Greyed, not missing** — the About row's rule. A row that vanished
    /// would answer *why is there no way to stop this* with silence, and a
    /// greyed native row raises no menu event at all, so nothing can reach the
    /// loop for one.
    #[test]
    fn the_two_actions_are_greyed_rather_than_left_out_when_there_is_nothing_to_do() {
        let enabled_for = |state: &VoiceMenu, wanted: VoiceAsk| {
            actions(state, None)
                .into_iter()
                .find(|(ask, _, _)| *ask == wanted)
                .map(|(_, _, enabled)| enabled)
        };
        // No worker at all: both rows are there and neither can be pressed.
        assert_eq!(enabled_for(&ready(), VoiceAsk::Release), Some(false));
        assert_eq!(enabled_for(&ready(), VoiceAsk::Stop), Some(false));
        // A worker with nothing resident: there is a process to stop and
        // nothing to release, and the two rows say so separately.
        let unloaded = VoiceMenu {
            worker: Some(VoiceWorker {
                model_loaded: false,
                ..idle_worker()
            }),
            ..ready()
        };
        assert_eq!(enabled_for(&unloaded, VoiceAsk::Release), Some(false));
        assert_eq!(enabled_for(&unloaded, VoiceAsk::Stop), Some(true));
        assert!(
            status_lines(&unloaded, None)
                .contains(&"Ready · Compact · model not loaded".to_owned()),
            "and the summary explains why the next dictation will pause"
        );
    }

    /// The one action row offers what the state means, and the loop decides
    /// what a press *does* from a fresh reading — see [`TrayAsk::Voice`].
    #[test]
    fn the_action_row_offers_what_this_state_means() {
        let label = |row: VoiceRow| {
            actions(&menu(row), None)
                .into_iter()
                .next()
                .map(|(_, label, _)| label)
        };
        assert_eq!(
            label(VoiceRow::NotInstalled { megabytes: None }).as_deref(),
            Some("Set voice input up…")
        );
        assert_eq!(
            label(VoiceRow::Downloading { percent: 3 }).as_deref(),
            Some("Show the download")
        );
        assert_eq!(
            label(VoiceRow::Installed).as_deref(),
            Some("Start dictation")
        );
    }

    #[test]
    fn the_voice_surface_entry_is_localized_in_every_language() {
        for lang in koru_i18n::Lang::ALL {
            let expected = koru_i18n::t(lang, koru_i18n::Key::VoiceGlobalOpen);
            assert!(!expected.trim().is_empty());
            assert!(
                voice_menu_localized(&ready(), None, lang)
                    .iter()
                    .any(|line| {
                        matches!(
                            line,
                            VoiceLine::Action { ask: VoiceAsk::Settings, label, enabled: true }
                                if label == expected
                        )
                    })
            );
        }
    }

    /// **A switched-off installation says so and offers nothing.**
    ///
    /// `KORU_VOICE_DISABLED` means nothing about voice is dialled, started or
    /// waited for (Q-V44) — so the submenu carries one line and no action at
    /// all, and there is no row a press could reach the loop through. The
    /// submenu itself stays openable, because a line explaining *why* there is
    /// nothing here is no use behind a submenu nobody can open.
    #[test]
    fn a_switched_off_installation_says_so_and_offers_nothing() {
        let off = menu(VoiceRow::Unavailable(koru_voice::Off::Switch));
        assert_eq!(
            voice_menu(&off, Some("Ctrl+Shift+D")),
            vec![VoiceLine::Status(
                VoiceStatus::Summary,
                "Voice input is switched off on this installation".to_owned()
            )],
            "one line, no chord, no separator and nothing to press"
        );
        assert!(
            actions(&off, Some("Ctrl+Shift+D")).is_empty(),
            "nothing here can start a worker, because there is nothing here"
        );
        // And a stale worker reading cannot smuggle a *Stop* row back in.
        assert!(actions(&off.keeping_worker(Some(idle_worker())), None).is_empty());
    }

    /// The disk and the worker are read on different events, so re-reading one
    /// must not blank the other — except on a switched-off installation, which
    /// has nothing true to say about a worker at all.
    #[test]
    fn a_fresh_disk_reading_keeps_what_the_worker_said() {
        let kept = ready().keeping_worker(Some(idle_worker()));
        assert_eq!(kept.worker, Some(idle_worker()));
        let off = menu(VoiceRow::Unavailable(koru_voice::Off::Switch))
            .keeping_worker(Some(idle_worker()));
        assert_eq!(off.worker, None);
    }

    /// **What a menu is not for.** Ring overruns, the sample rate, stage
    /// timings, sessions this run and the capture device are `koru-voice-worker
    /// status` and the log; a person's hardware is kept out of everything
    /// (`docs/contributing/operations/LOGGING.md`). This walks the states rather than
    /// trusting the three fields [`VoiceWorker`] happens to hold today.
    #[test]
    fn the_submenu_carries_no_diagnostics() {
        let mut every = vec![
            ready(),
            VoiceMenu {
                worker: Some(idle_worker()),
                ..ready()
            },
            menu(VoiceRow::NotInstalled {
                megabytes: Some(57),
            }),
            menu(VoiceRow::Downloading { percent: 7 }),
        ];
        for code in koru_voice::ErrorCode::ALL {
            every.push(VoiceMenu {
                worker: Some(VoiceWorker {
                    last_error: Some(code),
                    ..idle_worker()
                }),
                ..ready()
            });
        }
        for state in &every {
            for line in status_lines(state, Some("Ctrl+Shift+D")) {
                let lowered = line.to_lowercase();
                for banned in [
                    "overrun",
                    "sample rate",
                    " hz",
                    "session",
                    "microphone (",
                    "headset",
                    "capacity",
                    "ms",
                ] {
                    assert!(
                        !lowered.contains(banned),
                        "{line:?} reads like a diagnostic ({banned})"
                    );
                }
            }
        }
    }

    /// The tray is the one surface present when no window is, so it is where
    /// somebody who never opens Settings finds out dictation exists.
    #[test]
    fn the_icon_offers_voice_input() {
        assert!(OFFERED.contains(&TrayAsk::Voice));
        assert!(OFFERED.contains(&TrayAsk::VoiceSettings));
        assert!(OFFERED.contains(&TrayAsk::VoiceRelease));
        assert!(OFFERED.contains(&TrayAsk::VoiceStop));
    }

    #[test]
    fn quit_is_always_offered() {
        // **The rule.** An agent with no window and no console is a process
        // holding a global hotkey that only Task Manager can end, so the way to
        // stop it must not be conditional on anything.
        assert!(OFFERED.contains(&TrayAsk::Quit));
    }

    #[test]
    fn the_icon_offers_a_way_back_as_well_as_a_way_out() {
        // Search is the shortcut's twin for somebody who has forgotten it;
        // Restore is what brings hidden shells back.
        assert!(OFFERED.contains(&TrayAsk::Search));
        assert!(OFFERED.contains(&TrayAsk::Reminders));
        assert!(OFFERED.contains(&TrayAsk::NewAgent));
        assert!(OFFERED.contains(&TrayAsk::Restore));
        assert!(OFFERED.contains(&TrayAsk::OpenLocation));
        assert!(OFFERED.contains(&TrayAsk::OpenLocalDataLocation));
        assert!(OFFERED.contains(&TrayAsk::OpenSharedLocation));
        assert!(OFFERED.contains(&TrayAsk::OpenGatewayDocs));
        assert!(OFFERED.contains(&TrayAsk::OpenStoreStats));
        assert!(OFFERED.contains(&TrayAsk::OpenStoreLocation));
        // The backup row needs `rfd` and a main-thread NSOpenPanel it cannot
        // have on macOS, so it is offered on Windows only.
        #[cfg(windows)]
        assert!(OFFERED.contains(&TrayAsk::BackupStore));
        #[cfg(not(windows))]
        assert!(!OFFERED.contains(&TrayAsk::BackupStore));
    }

    #[test]
    fn the_icon_says_which_build_this_is_and_can_look_for_a_newer_one() {
        // An agent alone has no window, so the menu is the only place either
        // question can be asked from.
        assert!(OFFERED.contains(&TrayAsk::About));
        assert!(OFFERED.contains(&TrayAsk::CheckForUpdates));
        // Always, and on every build — see `TrayAsk::SendFeedback`.
        assert!(OFFERED.contains(&TrayAsk::SendFeedback));
    }

    #[test]
    fn a_rooted_tray_names_its_running_location() {
        assert_eq!(
            tooltip(
                "0.3.1",
                "internal-debug",
                Some(std::path::Path::new("C:/koru/root")),
                Some("dev")
            ),
            "Koru Agent 0.3.1 — C:/koru/root — dev"
        );
    }

    #[test]
    fn every_internal_build_names_its_branch() {
        for segment in ["internal-debug", "internal-release"] {
            assert_eq!(
                tooltip("0.3.1", segment, None, Some("dev")),
                format!("Koru Agent 0.3.1 — {segment} — dev")
            );
        }
        assert_eq!(
            tooltip("0.3.1", "internal-debug", None, None),
            "Koru Agent 0.3.1 — internal-debug"
        );
    }

    #[test]
    fn a_public_release_does_not_show_a_branch() {
        assert_eq!(
            tooltip("0.3.1", "stable", None, Some("left-over-local-branch")),
            "Koru Agent 0.3.1 — stable"
        );
    }

    #[test]
    fn the_about_row_names_the_build_number_when_one_was_allocated() {
        assert_eq!(
            about_line("0.0.4-nightly.20260815+abc1234", 16, "abc1234"),
            "Koru 0.0.4-nightly.20260815+abc1234 · build 16"
        );
    }

    #[test]
    fn a_cargo_build_says_what_it_is_rather_than_build_zero() {
        // `build 0` is a number nobody allocated, and it reads like a release.
        assert_eq!(about_line("0.3.1", 0, "debug"), "Koru 0.3.1 · debug");
    }

    #[test]
    fn a_published_build_points_at_its_own_immutable_page() {
        // The shape `koru-release` uploads: `releases/<version>/index.html`,
        // written once per version and never replaced, which is what makes it
        // derivable from the version with nothing fetched.
        assert_eq!(
            notes_url("0.0.4", true).as_deref(),
            Some("https://updates.koru.do/releases/0.0.4/index.html")
        );
    }

    /// **Right-click belongs to the menu, and the left button is the one icon
    /// gesture.**
    ///
    /// What the click opens takes the focus when it opens, which dismisses the
    /// menu — so a right click that also answered left the agent unreachable
    /// from the one place it is reachable from. Asserted per button *and* per
    /// button state, because `Click` is emitted for both and matching the
    /// button alone asked twice per click.
    #[cfg(windows)]
    #[test]
    fn only_releasing_the_left_button_is_an_icon_click() {
        use super::imp::is_icon_click;
        use tray_icon::dpi::{PhysicalPosition, PhysicalSize};
        use tray_icon::{MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId};

        let click = |button, button_state| TrayIconEvent::Click {
            id: TrayIconId::new("test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect {
                size: PhysicalSize::new(16, 16),
                position: PhysicalPosition::new(0.0, 0.0),
            },
            button,
            button_state,
        };

        assert!(is_icon_click(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        // The press is the same click arriving twice; only the release counts.
        assert!(!is_icon_click(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        // The menu's button, in both states.
        assert!(!is_icon_click(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(!is_icon_click(&click(
            MouseButton::Right,
            MouseButtonState::Down
        )));
        assert!(!is_icon_click(&click(
            MouseButton::Middle,
            MouseButtonState::Up
        )));
    }

    #[test]
    fn an_unpublished_build_has_no_page_and_is_not_given_one() {
        // **The row goes inert instead.** Opening a URL that 404s is worse than
        // a version somebody can read and not click, and every developer build
        // in the tree lands here.
        assert_eq!(notes_url("0.3.1", false), None);
        // And a version that is somehow blank names no page either, rather than
        // pointing at the directory listing of every release.
        assert_eq!(notes_url("  ", true), None);
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    fn with_devices(names: &[(&str, bool)], input: koru_voice::InputChoice) -> VoiceMenu {
        VoiceMenu {
            row: VoiceRow::Installed,
            installed: vec![koru_voice_models::ModelTier::Compact],
            worker: None,
            devices: names
                .iter()
                .map(|(name, default)| koru_voice::InputDevice::new(*name, *default))
                .collect(),
            input,
        }
    }

    /// **The picker always offers *System default*, and it is a choice.**
    ///
    /// With nothing having answered, that is the whole list — and it is still
    /// a row somebody can press, because choosing it means *follow the OS*
    /// rather than clearing a field.
    #[test]
    fn the_system_default_is_always_offered() {
        let rows = input_rows(&with_devices(&[], koru_voice::InputChoice::SystemDefault));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].label, "System default");
        assert!(rows[0].chosen);
        assert_eq!(rows[0].choice, koru_voice::InputChoice::SystemDefault);
    }

    /// Exactly one row carries the tick, in every state — a menu with two or
    /// none has stopped saying what is true.
    #[test]
    fn the_input_submenu_ticks_exactly_one() {
        let devices = [("Microphone Array", true), ("Yeti Nano", false)];
        for input in [
            koru_voice::InputChoice::SystemDefault,
            koru_voice::InputChoice::named("Yeti Nano"),
            koru_voice::InputChoice::named("Microphone Array"),
            // The one that used to be the hard case: a device that is not
            // there any more.
            koru_voice::InputChoice::named("a headset that is asleep"),
        ] {
            let rows = input_rows(&with_devices(&devices, input.clone()));
            assert_eq!(
                rows.iter().filter(|r| r.chosen).count(),
                1,
                "{input:?} -> {rows:?}"
            );
        }
    }

    /// **A remembered device that is gone is still offered and still ticked**,
    /// and its row says it is not connected.
    ///
    /// Dropping the row would move the choice to *System default* with nothing
    /// to put it back with — a silent correction of something the person said,
    /// which is the failure this whole arrangement exists to avoid.
    #[test]
    fn a_remembered_device_that_is_gone_is_still_offered_and_says_so() {
        let state = with_devices(
            &[("Microphone Array", true)],
            koru_voice::InputChoice::named("a headset that is asleep"),
        );
        let rows = input_rows(&state);
        let remembered = rows
            .iter()
            .find(|r| r.choice.name() == Some("a headset that is asleep"))
            .expect("the remembered device is still a row");
        assert!(remembered.chosen, "and it is still the chosen one");
        assert!(
            remembered.label.contains("not connected"),
            "{}",
            remembered.label
        );
        assert!(
            rows.iter().any(|r| r.choice.name().is_none()),
            "and there is still a way back to the system default"
        );
    }

    /// **The title is a pure function of the state**, and it names the
    /// microphone rather than counting them: *which one* is the question
    /// somebody opens this to answer.
    #[test]
    fn the_input_title_says_which_microphone() {
        let devices = [("Yeti Nano", false)];
        assert_eq!(
            input_title(&with_devices(
                &devices,
                koru_voice::InputChoice::SystemDefault
            )),
            "Input: System default"
        );
        assert_eq!(
            input_title(&with_devices(
                &devices,
                koru_voice::InputChoice::named("Yeti Nano")
            )),
            "Input: Yeti Nano"
        );
        // **The fall-back is in the title too**, so the person affected does
        // not have to open the submenu to find out.
        assert_eq!(
            input_title(&with_devices(
                &devices,
                koru_voice::InputChoice::named("a headset that is asleep")
            )),
            "Input: a headset that is asleep (not connected)"
        );
    }

    /// A remembered device *is* in the list when the worker can see it, and it
    /// is not repeated.
    #[test]
    fn a_connected_device_appears_once() {
        let rows = input_rows(&with_devices(
            &[("Yeti Nano", false), ("Microphone Array", true)],
            koru_voice::InputChoice::named("Yeti Nano"),
        ));
        assert_eq!(
            rows.iter()
                .filter(|r| r.choice.name() == Some("Yeti Nano"))
                .count(),
            1,
            "{rows:?}"
        );
        assert_eq!(rows.len(), 3, "System default plus two devices: {rows:?}");
    }

    /// The submenu carries the picker, and it sits between the action that
    /// starts a dictation and the two that act on the process.
    #[test]
    fn the_voice_submenu_carries_the_input_picker() {
        let state = with_devices(
            &[("Yeti Nano", true)],
            koru_voice::InputChoice::SystemDefault,
        );
        let lines = voice_menu(&state, Some("Ctrl+Shift+D"));
        let input_at = lines
            .iter()
            .position(|line| matches!(line, VoiceLine::Input(_)))
            .expect("an Input line");
        let act_at = lines
            .iter()
            .position(|line| {
                matches!(
                    line,
                    VoiceLine::Action {
                        ask: VoiceAsk::Act,
                        ..
                    }
                )
            })
            .expect("the action row");
        let release_at = lines
            .iter()
            .position(|line| {
                matches!(
                    line,
                    VoiceLine::Action {
                        ask: VoiceAsk::Release,
                        ..
                    }
                )
            })
            .expect("the release row");
        assert!(act_at < input_at && input_at < release_at, "{lines:?}");
    }

    /// **An installation with dictation switched off offers no picker.**
    /// Nothing about voice is dialled, started or offered there, and a
    /// microphone list would be a control leading nowhere.
    #[test]
    fn a_switched_off_installation_has_no_input_picker() {
        let state = VoiceMenu {
            row: VoiceRow::Unavailable(koru_voice::Off::Switch),
            installed: Vec::new(),
            worker: None,
            devices: vec![koru_voice::InputDevice::new("Yeti Nano", true)],
            input: koru_voice::InputChoice::SystemDefault,
        };
        let lines = voice_menu(&state, None);
        assert!(
            !lines.iter().any(|line| matches!(line, VoiceLine::Input(_))),
            "{lines:?}"
        );
    }

    /// A disk reading must not empty the picker: what is plugged in and what
    /// is installed move independently.
    #[test]
    fn re_reading_the_disk_keeps_the_device_list() {
        let devices = vec![koru_voice::InputDevice::new("Yeti Nano", true)];
        let fresh = VoiceMenu {
            row: VoiceRow::Installed,
            installed: Vec::new(),
            worker: None,
            devices: Vec::new(),
            input: koru_voice::InputChoice::SystemDefault,
        };
        assert_eq!(fresh.keeping_devices(devices.clone()).devices, devices);
    }

    // ------------------------------------------------- the Agents submenu

    /// A running raw session for the grouping fixtures. Only the fields the
    /// menu reads vary; everything else is the quietest legal value.
    fn session(
        profile: &str,
        project: Option<koru_session_protocol::ProjectId>,
        created_at_ms: i64,
    ) -> koru_session_protocol::SessionSummary {
        use std::str::FromStr;
        koru_session_protocol::SessionSummary {
            id: koru_session_protocol::SessionId::mint(),
            parent_session_id: None,
            profile: koru_session_protocol::SessionProfileKey::BuiltIn(
                koru_session_protocol::BuiltInProfileKey::from_str(profile).expect("a built-in"),
            ),
            display_name: profile.to_owned(),
            working_directory: None,
            mode: koru_session_protocol::SessionMode::RawPty,
            process: koru_session_protocol::ProcessState::Running,
            controller: koru_session_protocol::ControllerState::Detached,
            managed: None,
            active_turn_id: None,
            attention: koru_session_protocol::AttentionState::default(),
            pending_attachment_count: 0,
            capabilities: koru_session_protocol::CapabilitySet::default(),
            project_id: project,
            handover: Default::default(),
            model: None,
            origin: Default::default(),
            created_at_ms,
        }
    }

    fn project(name: &str) -> koru_session_protocol::ProjectSummary {
        koru_session_protocol::ProjectSummary {
            id: koru_session_protocol::ProjectId::mint(),
            name: name.to_owned(),
            root_directory: None,
            workspace_scope: None,
            created_at_ms: 0,
        }
    }

    /// The headings, in order — what a person scans the menu by.
    fn headers(lines: &[AgentsLine]) -> Vec<&str> {
        lines
            .iter()
            .filter_map(|line| match line {
                AgentsLine::Header(name) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    /// **The menu mirrors the navigator's grouping**: projects by case-folded
    /// name, each with its sessions by creation time, then Unassigned last —
    /// and a session placed in a project the registry does not know files
    /// under the bucket rather than under a heading that names nothing.
    #[test]
    fn the_agents_menu_groups_sessions_by_project_like_the_navigator() {
        let beta = project("beta");
        let alpha = project("Alpha");
        let late = session("claude", Some(alpha.id), 20);
        let early = session("powershell", Some(alpha.id), 10);
        let placed_nowhere = session("bash", Some(koru_session_protocol::ProjectId::mint()), 5);
        let unplaced = session("opencode", None, 6);
        let catalog = crate::agents_menu::AgentsCatalog {
            sessions: vec![
                late.clone(),
                placed_nowhere.clone(),
                early.clone(),
                unplaced.clone(),
            ],
            projects: vec![beta.clone(), alpha.clone()],
        };

        let lines = agents_menu_lines(&catalog, koru_i18n::Lang::En);
        // "Alpha" before "beta" (case-folded), Unassigned last; "beta" has no
        // sessions and draws no heading at all.
        assert_eq!(headers(&lines), ["Alpha", "Unassigned"], "{lines:?}");
        let rows: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                AgentsLine::Session { session, label } => Some((*session, label.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            [
                // Alpha's sessions, creation order.
                (early.id, "PowerShell"),
                (late.id, "Claude"),
                // The bucket: the orphaned placement and the unplaced one.
                (placed_nowhere.id, "Bash"),
                (unplaced.id, "OpenCode"),
            ],
            "{lines:?}"
        );
        // Every group is introduced by a separator, so the rows cannot run
        // into the pinned New above them.
        assert!(
            matches!(lines.first(), Some(AgentsLine::Separator)),
            "{lines:?}"
        );
    }

    /// No sessions, no rows: the submenu is New and nothing else — an empty
    /// Unassigned heading would be furniture.
    #[test]
    fn an_empty_catalog_draws_no_group_at_all() {
        let catalog = crate::agents_menu::AgentsCatalog {
            sessions: Vec::new(),
            projects: vec![project("Alpha")],
        };
        assert!(agents_menu_lines(&catalog, koru_i18n::Lang::En).is_empty());
    }

    /// A profile key this build no longer ships still names its row — the
    /// stable token, never a blank — and a custom profile gets the honest
    /// generic, exactly as the tab strip labels the same session.
    #[test]
    fn an_unresolvable_profile_still_gets_an_honest_label() {
        use std::str::FromStr;
        let mut gone = session("powershell", None, 1);
        gone.profile = koru_session_protocol::SessionProfileKey::BuiltIn(
            koru_session_protocol::BuiltInProfileKey::from_str("retired-agent").expect("a token"),
        );
        let mut custom = session("bash", None, 2);
        custom.profile = koru_session_protocol::SessionProfileKey::Custom(
            koru_session_protocol::SessionProfileId::mint(),
        );
        let catalog = crate::agents_menu::AgentsCatalog {
            sessions: vec![gone, custom],
            projects: Vec::new(),
        };
        let lines = agents_menu_lines(&catalog, koru_i18n::Lang::En);
        let labels: Vec<_> = lines
            .iter()
            .filter_map(|line| match line {
                AgentsLine::Session { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["retired-agent", "Session"], "{lines:?}");
    }
}

#[cfg(test)]
mod menu_tests;
