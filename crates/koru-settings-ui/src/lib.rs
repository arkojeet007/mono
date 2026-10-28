// SPDX-License-Identifier: MPL-2.0

//! **One Settings surface: a sidebar of pages, each a stack of cards.**
//!
//! Charter: *the Settings surface: page composition, rows, and asynchronous
//! use of the typed settings client; it reads and writes no files and names no
//! document, workspace, or session.*
//!
//! Before this crate the preference rows were methods on `apps/koru`'s
//! `WelcomeView`. The precedent is `koru-search-ui` and `koru-ai-ui`: one
//! reusable surface, a thin host.
//!
//! ## Pages
//!
//! The sidebar lists [`PageId`]s — Appearance, Editor, Reminders, Terminal,
//! System — with a search box that narrows them by what they hold. One page is
//! laid out at a time, so a page that is not showing has no rects and cannot be
//! pressed. **Reminders is the host's page**: its rows belong to `apps/koru`
//! and are drawn into the slot [`host_section_rect`](SettingsSurface::host_section_rect)
//! reports; this crate knows only its title.
//!
//! Every control is a shared `koru-ui` component (see [`page`]), so the
//! inspector shows `KoruCard`, `KoruCardRow`, `KoruSegmented`, `KoruDropdown`
//! and the rest here exactly as it does anywhere else.
//!
//! ## Where a value comes from, and where a press goes
//!
//! Three routes, and which one a row takes is a fact about the row rather than
//! a style:
//!
//! - **A live process global** — theme, caret, density, the four font roles,
//!   the default view mode. The row reads `koru_draw::theme` and writes it
//!   back on press, and the shell's per-field patch carries it to the
//!   authority.
//! - **The authority, through this surface's own link** — `start_on_login` and
//!   the whole Terminal page. These have no process global and no window
//!   mirror, so the surface reads them through [`SettingsLinks`] and patches
//!   them per field on press.
//! - **The host** — everything whose effect belongs to somebody else:
//!   `renderer.conf`, the voice model cache, the consent record, every open
//!   document tab. The press raises a [`Request`] and the host performs it.
//!
//! ## Immediate saves, and the two states they owe
//!
//! There is no Apply button. A press takes effect at once and the patch goes
//! out at once (SP-6b's press-time pattern), which leaves two states a surface
//! that pretended otherwise would not have:
//!
//! - **A write that did not land reconciles visibly.** The link re-reads the
//!   section after a refusal, so what comes back is what is stored; the control
//!   moves back to it and a quiet [`Notice`] says so (S23).
//! - **A Gateway that is not answering makes the rows read-only.** They stay
//!   visible, dimmed, carrying their last authoritative values, and the page
//!   says why (S9's idiom).

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use koru_i18n::{Key, Lang};
use koru_settings::{
    CaretColorChoice, CaretHeightChoice, CaretWidthChoice, CloseBehaviorChoice, DensityChoice,
    DictationGesture, DictationInput, DictationLanguage, DictationTier, FontSetting, GlobalPatch,
    GlobalSettings, LanguageChoice, TerminalCursorHeight, TerminalCursorWidth, TerminalPatch,
    TerminalSettings, ThemeChoice, ViewModeChoice,
};
use koru_ui::{
    ActionButton, Card, CardRow, Dropdown, DropdownOption, InputStyle, Menu, NavItem, NavList,
    PageShell, PreviewTile, Step, TextInput, Toggle,
};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

pub mod action;
pub mod links;
mod page;
pub mod state;

mod global;
mod terminal;

#[cfg(test)]
mod dependency;
#[cfg(test)]
mod tests;

pub use action::{FontRole, Request};
pub use links::SettingsLinks;
pub use page::PageId;
pub use state::{AiSlot, Availability, DictionaryRow, Notice, RendererRow, VoiceFacts};

use action::Action;
use page::{Block, Builder, Laid, Target, TilePreview, Widget};

/// Text lines one notch of the wheel moves — three, which is what the document
/// editor does and what Windows itself means by a wheel click.
const WHEEL_LINES: f32 = 3.0;

/// The alpha a read-only row draws at. `koru_ui::icon_button`'s disabled
/// tone, so a dimmed control here reads as the same kind of dimmed everywhere.
const DIMMED: f32 = 0.45;

/// **Where the nav and the page go is the shared [`PageShell`]** — Manage
/// lays itself out by the same geometry. Settings' nav column: its width, its
/// left air, and where its search box starts — level with the page's title.
const SHELL: PageShell = PageShell::SETTINGS;
/// Named for the suite, which places presses by them.
#[cfg(test)]
const NAV_W: f32 = SHELL.nav_w;
const NAV_TOP: f32 = PageShell::TOP;
/// The search box, and the air between it and the list (or chips) under it.
const SEARCH_H: f32 = 30.0;
const SEARCH_GAP: f32 = 14.0;
/// The room kept at the search box's right end for the *Ctrl+,* hint.
const SEARCH_HINT_W: f32 = 44.0;
/// The page column's side air.
const PAGE_MARGIN: f32 = PageShell::PAGE_MARGIN;
/// **Below this width the nav has no column of its own**: the search box and
/// the sections as chips sit above the page instead, so a narrow window or a
/// split pane keeps the page's full width.
#[cfg(test)]
const COMPACT_BELOW: f32 = PageShell::COMPACT_BELOW;
const COMPACT_TOP: f32 = PageShell::COMPACT_TOP;
const COMPACT_SEARCH_MAX: f32 = 260.0;
const COMPACT_CHIPS_GAP: f32 = 12.0;

/// A page title: the one large line on the surface — the same title role
/// every first-run and Welcome screen uses, so Koru has one size of title.
pub(crate) fn title_style() -> theme::TextStyle {
    theme::role::title()
}

/// **Which sections this host shows.**
///
/// Koru shows every page. The two other constructors remain for the
/// standalone Terminal and AI windows until they are removed; they are not
/// designed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sections {
    pub global: bool,
    pub ai: bool,
    pub terminal: bool,
    /// The host fills a Reminders page.
    pub reminders: bool,
}

impl Sections {
    /// Koru's window: everything.
    pub fn koru() -> Self {
        Self {
            global: true,
            ai: true,
            terminal: true,
            reminders: true,
        }
    }

    /// Koru AI's window: what applies to the app, and its own providers.
    pub fn koru_ai() -> Self {
        Self {
            global: true,
            ai: true,
            terminal: false,
            reminders: false,
        }
    }

    /// Koru Terminal's window: what applies to the app, and its own appearance.
    pub fn terminal() -> Self {
        Self {
            global: true,
            ai: false,
            terminal: true,
            reminders: false,
        }
    }

    /// The pages these sections make, in sidebar order.
    pub fn pages(self) -> Vec<PageId> {
        PageId::ALL
            .into_iter()
            .filter(|page| match page {
                PageId::Appearance | PageId::Editor => self.global,
                PageId::Reminders => self.reminders,
                PageId::Terminal => self.terminal,
                PageId::System => self.global || self.ai,
            })
            .collect()
    }
}

/// A dropdown whose list is showing.
struct OpenDropdown {
    /// Which of the page's dropdowns, so a relayout can tell it still exists.
    index: usize,
    menu: Menu<DropdownOption>,
    actions: Vec<Action>,
}

/// Where the pointer is, in the surface's own vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointed {
    Search,
    Nav(usize),
    /// A hit of the page, by index into [`Laid::hits`].
    Hit(usize),
}

/// The Settings surface.
pub struct SettingsSurface {
    sections: Sections,
    lang: Lang,
    engine: TextEngine,
    viewport: (f32, f32),
    pointer: (f32, f32),
    /// The page's vertical scroll. The sidebar does not scroll.
    scroll: f32,

    page: PageId,
    search: TextInput,
    search_focused: bool,
    open: Option<OpenDropdown>,

    /// Geometry reserved on the Reminders page for rows that belong to the
    /// host. The shared surface knows only its height.
    host_section_height: f32,

    /// The authority's link, and the two mirrors it fills.
    links: SettingsLinks,
    global: GlobalSettings,
    terminal: TerminalSettings,

    /// **The four values a host keeps a copy of**, fed through
    /// [`adopt_preferences`](Self::adopt_preferences). `pinned_show_title` and
    /// `close_behavior` are drawn from these. `view_mode` and `canvas_background`
    /// are also process globals — the rows read those live, the same values a
    /// new document opens with, so a stale copy cannot show the wrong choice.
    view_mode: theme::ViewMode,
    canvas_background: Option<[u8; 4]>,
    pinned_show_title: bool,
    close_behavior: CloseBehaviorChoice,

    /// The dictation preferences, seeded and re-seeded by the host. They live
    /// in process globals this crate may not reach (`koru-workspace-host`'s
    /// `voice_prefs`), so the host mirrors them here and performs the writes.
    voice_language: DictationLanguage,
    voice_input: DictationInput,
    voice_tier: DictationTier,
    voice_gesture: DictationGesture,

    /// The spelling preferences, the same host-mirrored route as voice.
    spelling_enabled: bool,
    spelling_language: LanguageChoice,
    /// The choices the host offers — Automatic first, then each dictionary
    /// language by endonym. Labels come from the host so this crate never
    /// names the closed language set.
    spelling_languages: Vec<(LanguageChoice, String)>,
    /// The D6 sentence when "automatic" resolves to no dictionary.
    spelling_note: Option<String>,
    /// The row offering a missing dictionary's download, when one is missing.
    dictionary: Option<DictionaryRow>,
    /// The personal dictionary's size, as the host last read it.
    personal_words: Option<usize>,
    /// *Apply to all* was pressed while this tab was open.
    layout_applied: bool,
    /// The System page's Advanced card is unfolded.
    advanced_open: bool,
    /// *Share anonymous usage data*, as the host last read this device's
    /// record. `None` draws no row: there is no record to show a switch for
    /// (the events worker is not there, or has not answered yet).
    telemetry: Option<bool>,

    voice: VoiceFacts,
    renderer: RendererRow,
    ai: AiSlot,

    availability: Availability,
    notice: Option<Notice>,

    laid: Laid,
    /// The compact nav's chips, in viewport space, and the band they sit in.
    nav_chips: Vec<Rect>,
    nav_band_h: f32,
    requests: Vec<Request>,
}

impl SettingsSurface {
    /// A surface with no link, for a host that supplies every section itself
    /// and for every test. It never dials a Gateway.
    pub fn new(sections: Sections) -> Self {
        Self::with_links(sections, SettingsLinks::disabled())
    }

    /// A surface that reads and patches through its own links.
    pub fn with_links(sections: Sections, links: SettingsLinks) -> Self {
        let mut engine = TextEngine::new();
        let search = TextInput::new(&mut engine, theme::role::row());
        Self {
            sections,
            lang: Lang::default(),
            engine,
            viewport: (900.0, 560.0),
            pointer: (0.0, 0.0),
            scroll: 0.0,
            page: sections
                .pages()
                .first()
                .copied()
                .unwrap_or(PageId::Appearance),
            search,
            search_focused: false,
            open: None,
            host_section_height: 0.0,
            global: links.global_authoritative(),
            terminal: links.terminal_authoritative(),
            links,
            view_mode: theme::default_view_mode(),
            canvas_background: theme::default_canvas_bg(),
            pinned_show_title: false,
            close_behavior: CloseBehaviorChoice::default(),
            voice_language: DictationLanguage::default(),
            voice_input: DictationInput::default(),
            voice_tier: DictationTier::default(),
            voice_gesture: DictationGesture::default(),
            spelling_enabled: true,
            spelling_language: LanguageChoice::default(),
            spelling_languages: Vec::new(),
            spelling_note: None,
            dictionary: None,
            personal_words: None,
            layout_applied: false,
            advanced_open: false,
            telemetry: None,
            voice: VoiceFacts::default(),
            renderer: RendererRow::none(),
            ai: AiSlot::default(),
            availability: Availability::Live,
            notice: None,
            laid: Laid::default(),
            nav_chips: Vec::new(),
            nav_band_h: 0.0,
            requests: Vec::new(),
        }
    }

    /// **The Terminal's Settings tab, ready to be hosted.** Kept until the
    /// standalone Terminal is removed.
    pub fn for_terminal(segment: &str, root: Option<std::path::PathBuf>) -> Self {
        Self::with_links(
            Sections::terminal(),
            SettingsLinks::open(segment, root, true, true),
        )
    }

    // ------------------------------------------------------- the host feeds it

    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
    }

    pub fn sections(&self) -> Sections {
        self.sections
    }

    /// The four values the host owns a copy of.
    pub fn adopt_preferences(
        &mut self,
        view_mode: theme::ViewMode,
        canvas_background: Option<[u8; 4]>,
        pinned_show_title: bool,
        close_behavior: CloseBehaviorChoice,
    ) {
        self.view_mode = view_mode;
        self.canvas_background = canvas_background;
        self.pinned_show_title = pinned_show_title;
        self.close_behavior = close_behavior;
    }

    /// What the host is showing for *When the last window closes*.
    pub fn close_behavior(&self) -> CloseBehaviorChoice {
        self.close_behavior
    }

    pub fn pinned_show_title(&self) -> bool {
        self.pinned_show_title
    }

    pub fn view_mode(&self) -> theme::ViewMode {
        self.view_mode
    }

    pub fn canvas_background(&self) -> Option<[u8; 4]> {
        self.canvas_background
    }

    /// The host cycled the canvas color and hands back what it landed on.
    pub fn set_canvas_background(&mut self, background: Option<[u8; 4]>) {
        self.canvas_background = background;
    }

    /// The dictation preferences as the host's own globals hold them.
    pub fn set_voice_preferences(
        &mut self,
        language: DictationLanguage,
        input: DictationInput,
        tier: DictationTier,
        gesture: DictationGesture,
    ) {
        self.voice_language = language;
        self.voice_input = input;
        self.voice_tier = tier;
        self.voice_gesture = gesture;
    }

    /// A fresh reading of what this machine offers dictation.
    pub fn set_voice_facts(&mut self, facts: VoiceFacts) {
        self.voice = facts;
    }

    /// The spelling preferences and their choices, as the host's globals hold
    /// them. One call carries all four so the page can never show a
    /// half-updated state.
    pub fn set_spelling(
        &mut self,
        enabled: bool,
        language: LanguageChoice,
        languages: Vec<(LanguageChoice, String)>,
        note: Option<String>,
    ) {
        self.spelling_enabled = enabled;
        self.spelling_language = language;
        self.spelling_languages = languages;
        self.spelling_note = note;
    }

    /// The missing-dictionary row, or `None` when the files are here.
    pub fn set_dictionary(&mut self, row: Option<DictionaryRow>) {
        self.dictionary = row;
    }

    /// How many words the personal dictionary holds; `None` until the host
    /// has read it, when the row says nothing about its size.
    pub fn set_personal_dictionary_words(&mut self, words: Option<usize>) {
        self.personal_words = words;
    }

    pub fn set_renderer(&mut self, row: RendererRow) {
        self.renderer = row;
    }

    pub fn set_ai_slot(&mut self, slot: AiSlot) {
        self.ai = slot;
    }

    /// This device's telemetry answer, as the host last read it: `Some(on)`
    /// draws the switch in that position, `None` draws no row. Answers
    /// whether anything moved, so a host asks for a frame only when it did.
    pub fn set_telemetry_consent(&mut self, granted: Option<bool>) -> bool {
        let changed = self.telemetry != granted;
        self.telemetry = granted;
        changed
    }

    /// Take an authoritative Global section the **host's** link produced, for a
    /// host that already holds one. Answers whether anything this surface shows
    /// moved.
    pub fn adopt_global(&mut self, section: &GlobalSettings) -> bool {
        let moved = self.global != *section;
        self.reconcile_global(section);
        self.global = section.clone();
        moved
    }

    /// The same for the Terminal section.
    pub fn adopt_terminal(&mut self, section: &TerminalSettings) -> bool {
        let moved = self.terminal != *section;
        self.reconcile_terminal(section);
        self.terminal = section.clone();
        moved
    }

    /// Say so directly, for a host that knows better than the inference in
    /// [`poll`](Self::poll) — one that is already watching the Gateway.
    pub fn set_availability(&mut self, availability: Availability) {
        self.availability = availability;
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// The links this surface holds, so a host can assert what it handed over
    /// — chiefly that a test build handed over nothing that dials.
    pub fn links(&self) -> &SettingsLinks {
        &self.links
    }

    /// The Terminal section as this surface holds it — its authority's value,
    /// with any press already applied.
    pub fn terminal(&self) -> &TerminalSettings {
        &self.terminal
    }

    /// The quiet sentence a failed write left behind, if there is one.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// What only the host can do. Drained, so a request is performed once.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    // ----------------------------------------------------------- the links

    /// **Take whatever the links have, and decide whether anyone is home.**
    ///
    /// Never blocks: every read, every patch and the bounded startup wait
    /// happen on the link's own worker, and this is a `try_recv` and a clock
    /// comparison. Answers whether anything moved.
    ///
    /// **Liveness is inferred, not observed.** `SectionLink` publishes a
    /// section on every outcome a reachable authority can produce and nothing
    /// at all when the Gateway is unreachable, so "nothing has published inside
    /// the deadline" is the signal — noticed at the first edit after a loss,
    /// which is exactly the moment SP-6b describes.
    pub fn poll(&mut self) -> bool {
        let mut moved = false;
        if let Some(section) = self.links.poll_global() {
            self.availability = Availability::Live;
            moved |= self.adopt_global(&section);
        }
        if let Some(section) = self.links.poll_terminal() {
            self.availability = Availability::Live;
            moved |= self.adopt_terminal(&section);
        }
        if self.links.is_overdue() && !self.availability.is_down() {
            self.availability = Availability::Down;
            moved = true;
        }
        moved
    }

    /// When this surface next wants a pass even if nothing happens.
    pub fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        self.links.deadline()
    }

    /// Hand down the call that makes the host's loop take another pass.
    pub fn on_update(&self, wake: impl Fn() + Send + Clone + 'static) {
        self.links.on_update(wake);
    }

    // --------------------------------------------------------------- pages

    /// The page showing.
    pub fn page(&self) -> PageId {
        self.page
    }

    /// Whether a dropdown's list is open — it is drawn above everything, so a
    /// host's own rows must not take the pointer while it is.
    pub fn menu_open(&self) -> bool {
        self.open.is_some()
    }

    /// Show `page`, from its top. Answers whether anything changed; a page this
    /// host does not have is refused.
    pub fn show_page(&mut self, page: PageId) -> bool {
        if !self.sections.pages().contains(&page) {
            return false;
        }
        let moved = self.page != page || self.scroll != 0.0;
        self.page = page;
        self.scroll = 0.0;
        self.open = None;
        moved
    }

    /// The pages the sidebar lists now — the host's pages, narrowed by the
    /// search box.
    fn listed(&self) -> Vec<PageId> {
        let query = self.search.value();
        self.sections
            .pages()
            .into_iter()
            .filter(|page| page.matches(self.lang, &query))
            .collect()
    }

    // ------------------------------------------------------------ geometry

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
    }

    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                y * theme::text_metrics::DOC_BODY.line_height * WHEEL_LINES
            }
            // A trackpad already reports the distance the finger asked for.
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.open = None;
        self.scroll = (self.scroll - lines).max(0.0);
    }

    /// Open the Editor page with the Voice input card at the top.
    ///
    /// Welcome links here directly, instead of making a new user hunt for it
    /// after choosing *set up your mic*. The card is found through a semantic
    /// layout anchor, so localized copy cannot change where the link lands.
    pub fn scroll_to_voice(&mut self) -> bool {
        let paged = self.show_page(PageId::Editor);
        self.relayout();
        let Some(y) = self
            .laid
            .anchors
            .iter()
            .find(|(anchor, _)| *anchor == page::Anchor::Voice)
            .map(|(_, y)| *y)
        else {
            return paged;
        };
        let next = (y - Card::TITLE_H - Card::TITLE_GAP - page::TOP).max(0.0);
        let moved = (self.scroll - next).abs() > f32::EPSILON;
        self.scroll = next;
        paged || moved
    }

    /// Scroll only as far as needed to reveal a content-space rectangle.
    ///
    /// Host-owned rows use the same content coordinates as the shared rows.
    /// Keeping this operation here preserves one scroll authority.
    pub fn reveal_content_rect(&mut self, rect: Rect) -> bool {
        const MARGIN: f32 = 16.0;

        self.relayout();
        let viewport_h = self.viewport.1.max(0.0);
        let max_scroll = (self.laid.bottom - viewport_h).max(0.0);
        let next = if rect.y < self.scroll + MARGIN {
            rect.y - MARGIN
        } else if rect.y + rect.h > self.scroll + viewport_h - MARGIN {
            rect.y + rect.h + MARGIN - viewport_h
        } else {
            self.scroll
        }
        .clamp(0.0, max_scroll);
        let changed = next != self.scroll;
        self.scroll = next;
        changed
    }

    /// Reserve room for the host's rows on the Reminders page.
    ///
    /// Non-finite and negative values mean no rows.
    pub fn set_host_section_height(&mut self, height: f32) {
        self.host_section_height = if height.is_finite() {
            height.max(0.0)
        } else {
            0.0
        };
    }

    /// The host's slot in content coordinates after the last
    /// [`relayout`](Self::relayout) — `None` unless the Reminders page is
    /// showing. At height zero it still reports the insertion point, so a host
    /// can measure its rows before reserving their height.
    pub fn host_section_rect(&self) -> Option<Rect> {
        self.laid.host
    }

    /// Map a viewport-space pointer coordinate into the scrolled page space
    /// used by [`host_section_rect`](Self::host_section_rect).
    pub fn content_y(&self, viewport_y: f32) -> f32 {
        viewport_y + self.scroll
    }

    /// Whether the nav sits above the page as chips rather than beside it.
    fn compact(&self) -> bool {
        SHELL.compact(self.viewport.0)
    }

    /// The nav's own region: a column at the centred group's left, or —
    /// compact — a band across the top holding the search box and the chips.
    fn sidebar_rect(&self) -> Rect {
        SHELL.sidebar(self.viewport, self.nav_band_h)
    }

    fn search_rect(&self) -> Rect {
        if self.compact() {
            Rect::new(
                PAGE_MARGIN,
                COMPACT_TOP,
                COMPACT_SEARCH_MAX.min((self.viewport.0 - PAGE_MARGIN * 2.0).max(0.0)),
                SEARCH_H,
            )
        } else {
            let nav = SHELL.nav_area(self.viewport, self.nav_band_h, NAV_TOP);
            Rect::new(nav.x, nav.y, nav.w, SEARCH_H)
        }
    }

    fn nav_rect(&self) -> Rect {
        let search = self.search_rect();
        SHELL.nav_area(
            self.viewport,
            self.nav_band_h,
            search.y + search.h + SEARCH_GAP,
        )
    }

    /// Where the compact chips start: under the search box.
    fn chips_area(&self) -> Rect {
        let search = self.search_rect();
        SHELL.chips_area(self.viewport.0, search.y + search.h + COMPACT_CHIPS_GAP)
    }

    /// The page area: right of the nav column, or under the compact band.
    fn page_area(&self) -> Rect {
        SHELL.page_area(self.viewport, self.nav_band_h)
    }

    /// Where the page's column sits: right after the nav (wide) or centred
    /// under the chips (compact), and never wider than the column width.
    fn column_rect(&self) -> Rect {
        SHELL.column(self.viewport, self.nav_band_h)
    }

    /// Lay the compact chips out, and so the band's height. Before the page,
    /// because the page starts under the band.
    fn relayout_nav(&mut self) {
        if !self.compact() {
            self.nav_chips.clear();
            self.nav_band_h = 0.0;
            return;
        }
        let listed = self.listed();
        let lang = self.lang;
        let items: Vec<NavItem<'_>> = listed
            .iter()
            .map(|page| NavItem::new(page.label(lang)))
            .collect();
        let area = self.chips_area();
        self.nav_chips = koru_ui::ChipNav::layout(&mut self.engine, area, &items);
        let chips_h = koru_ui::ChipNav::height(&self.nav_chips);
        self.nav_band_h = PageShell::band_h(area, chips_h);
    }

    /// **Lay the showing page out.** Public because the draw pass is not the
    /// only caller that needs the rects: a test presses a control without a
    /// GPU, and a host that wants a content height before it paints can ask.
    pub fn relayout(&mut self) {
        self.relayout_nav();
        let rect = self.column_rect();
        let Self {
            sections,
            lang,
            engine,
            page,
            host_section_height,
            global,
            terminal,
            pinned_show_title,
            close_behavior,
            voice_language,
            voice_input,
            voice_tier,
            voice_gesture,
            spelling_enabled,
            spelling_language,
            spelling_languages,
            spelling_note,
            dictionary,
            personal_words,
            layout_applied,
            advanced_open,
            telemetry,
            voice,
            renderer,
            ai,
            availability,
            notice,
            laid,
            ..
        } = self;
        let lang = *lang;
        let mut b = Builder::new(engine, rect.x, rect.w, rect.y);
        b.title(page.label(self.lang), page.description(self.lang));
        // The one sentence a read-only surface owes, first thing on the page.
        if availability.is_down() && *page != PageId::Reminders {
            b.note(koru_i18n::t(lang, Key::SettingsUnavailableReadOnly));
        }
        if let Some(notice) = notice.as_ref() {
            b.note(&notice.text);
        }
        let rows = global::Rows {
            lang,
            section: global,
            pinned_show_title: *pinned_show_title,
            close_behavior: *close_behavior,
            renderer,
            voice,
            voice_language,
            voice_input,
            voice_tier: *voice_tier,
            voice_gesture: *voice_gesture,
            spelling_enabled: *spelling_enabled,
            spelling_language,
            spelling_languages,
            spelling_note: spelling_note.as_deref(),
            dictionary: dictionary.as_ref(),
            personal_words: *personal_words,
            layout_applied: *layout_applied,
            advanced_open: *advanced_open,
            telemetry: *telemetry,
            ai: if sections.ai { ai } else { &EMPTY_AI },
        };
        match page {
            PageId::Appearance => global::appearance(&mut b, &rows),
            PageId::Editor => global::editor(&mut b, &rows),
            PageId::Reminders => {
                b.host(*host_section_height);
            }
            PageId::Terminal => terminal::lay_out(&mut b, lang, terminal),
            PageId::System => global::system(&mut b, &rows),
        }
        *laid = b.finish();
        // A relayout can drop the dropdown a list was opened from — the page
        // changed, or the setting it belonged to went away.
        if let Some(open) = self.open.as_ref()
            && open.index >= self.laid.dropdowns.len()
        {
            self.open = None;
        }
    }

    /// The bottom of the laid-out page, the scroll range's upper bound.
    pub fn content_bottom(&self) -> f32 {
        self.laid.bottom
    }

    // --------------------------------------------------------------- input

    /// What the pointer is over, against the last layout. `(x, y)` is in
    /// viewport space.
    fn pointed(&self, x: f32, y: f32) -> Option<Pointed> {
        let sidebar = self.sidebar_rect();
        if sidebar.contains(x, y) {
            if self.search_rect().contains(x, y) {
                return Some(Pointed::Search);
            }
            if self.compact() {
                return koru_ui::ChipNav::chip_at(&self.nav_chips, x, y).map(Pointed::Nav);
            }
            let listed = self.listed();
            return NavList::row_at(self.nav_rect(), listed.len(), x, y).map(Pointed::Nav);
        }
        let cy = y + self.scroll;
        self.laid
            .hits
            .iter()
            .position(|hit| hit.rect.contains(x, cy))
            .map(Pointed::Hit)
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let moved = self.pointer != (x, y);
        self.pointer = (x, y);
        if let Some(open) = self.open.as_mut() {
            let viewport = self.viewport;
            open.menu.hover = open.menu.item_at(viewport, x, y).or(open.menu.hover);
        }
        moved
    }

    pub fn mouse_up(&mut self) -> bool {
        self.search.release();
        false
    }

    /// Put the keyboard in the search box with the query it holds selected,
    /// so the next thing typed replaces the last search rather than
    /// extending it. The chord's way in: `Ctrl+,` opens Settings to find
    /// something, and a person who arrived by keyboard should not have to
    /// reach for the mouse to say what. An open list closes; it was the
    /// mouse's.
    pub fn focus_search(&mut self) {
        self.open = None;
        self.search_focused = true;
        self.search.select_all(&mut self.engine);
    }

    /// Whether the search box has the keyboard.
    pub fn search_focused(&self) -> bool {
        self.search_focused
    }

    /// **A press, gated by what is actually drawn.**
    ///
    /// The hit test runs against the controls the last layout produced, so a
    /// page that is not showing has no rects and cannot be pressed, and blank
    /// space cannot mutate a hidden setting.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> bool {
        // An open list takes the next press whatever it lands on: a row
        // chooses, anything else closes it, and nothing underneath is pressed.
        if let Some(open) = self.open.take() {
            if let Some(item) = open.menu.item_at(self.viewport, x, y) {
                self.choose(open, item);
            }
            return true;
        }
        match self.pointed(x, y) {
            Some(Pointed::Search) => {
                self.search_focused = true;
                let rect = self.search_text_rect();
                self.search.click(&mut self.engine, rect, x, y);
                true
            }
            Some(Pointed::Nav(index)) => {
                self.search_focused = false;
                if let Some(page) = self.listed().get(index).copied() {
                    self.show_page(page);
                }
                true
            }
            Some(Pointed::Hit(index)) => {
                self.search_focused = false;
                // **A read-only surface consumes the press and changes nothing.**
                if self.availability.is_down() {
                    return true;
                }
                self.press_hit(index);
                true
            }
            None => {
                let was = self.search_focused;
                self.search_focused = false;
                was
            }
        }
    }

    fn press_hit(&mut self, index: usize) {
        let Some(hit) = self.laid.hits.get(index) else {
            return;
        };
        match &hit.target {
            Target::Action(action) => {
                let action = clone_action(action);
                self.perform(action);
            }
            Target::Dropdown(dropdown) => {
                let dropdown = *dropdown;
                self.open_dropdown(dropdown);
            }
        }
    }

    fn open_dropdown(&mut self, index: usize) {
        let Some(laid) = self.laid.dropdowns.get(index) else {
            return;
        };
        // The menu draws in viewport space, above the scrolled page.
        let field = Rect::new(
            laid.field.x,
            laid.field.y - self.scroll,
            laid.field.w,
            laid.field.h,
        );
        let options: Vec<DropdownOption> = laid
            .options
            .iter()
            .map(|(_, option)| option.clone())
            .collect();
        let actions = laid
            .options
            .iter()
            .map(|(action, _)| clone_action(action))
            .collect();
        let menu = Dropdown::menu(&mut self.engine, field, options);
        self.open = Some(OpenDropdown {
            index,
            menu,
            actions,
        });
    }

    fn choose(&mut self, open: OpenDropdown, item: usize) {
        if self.availability.is_down() {
            return;
        }
        if let Some(action) = open.actions.into_iter().nth(item) {
            self.perform(action);
        }
    }

    /// **The keys this surface claims**: the open list's (arrows, Enter,
    /// Escape) and the search box's. Everything else belongs to the host,
    /// which sees it first.
    pub fn key(&mut self, key: &WinitKey, text: Option<&str>, mods: ModifiersState) -> bool {
        if let Some(open) = self.open.as_mut() {
            let enabled = vec![true; open.menu.items().len()];
            match key {
                WinitKey::Named(NamedKey::ArrowDown) => open.menu.step(&enabled, 1),
                WinitKey::Named(NamedKey::ArrowUp) => open.menu.step(&enabled, -1),
                WinitKey::Named(NamedKey::Escape) => self.open = None,
                WinitKey::Named(NamedKey::Enter | NamedKey::Space) => {
                    if let Some(open) = self.open.take()
                        && let Some(item) = open.menu.hover
                    {
                        self.choose(open, item);
                    }
                }
                _ => return false,
            }
            return true;
        }
        if self.search_focused {
            match key {
                WinitKey::Named(NamedKey::Escape) => {
                    if self.search.is_empty() {
                        self.search_focused = false;
                    } else {
                        self.search.set_text(&mut self.engine, "");
                    }
                    return true;
                }
                // Enter goes to the first page the search kept.
                WinitKey::Named(NamedKey::Enter) => {
                    if let Some(page) = self.listed().first().copied() {
                        self.show_page(page);
                    }
                    self.search_focused = false;
                    return true;
                }
                _ => {
                    let typed = self.search.key(&mut self.engine, key, text, mods);
                    return typed == koru_ui::text_field::Typed::Handled;
                }
            }
        }
        false
    }

    /// Koru does not use the hand cursor (`docs/features/design/DESIGN.md`), so both
    /// answers are an arrow; the hit test stays because it is the register of
    /// what this screen can be pressed on.
    pub fn cursor_icon(&self) -> CursorIcon {
        if self.availability.is_down() {
            return koru_ui::cursor::IDLE;
        }
        match self.pointed(self.pointer.0, self.pointer.1) {
            Some(Pointed::Search) => CursorIcon::Text,
            Some(_) => koru_ui::cursor::PRESS,
            None => koru_ui::cursor::IDLE,
        }
    }

    /// Brushes are baked into parley layouts and every layout here is built
    /// fresh each frame, so a theme flip needs nothing kept.
    pub fn invalidate(&mut self) {
        self.engine.clear_pill_cache();
    }

    // --------------------------------------------------------------- draw

    pub fn draw(&mut self, scene_out: &mut Scene, root: Affine) {
        let p = theme::p();
        let (vw, vh) = self.viewport;
        scene::fill(scene_out, root, p.bg, &scene::rect(0.0, 0.0, vw, vh));

        self.relayout();
        let bottom = self.laid.bottom;
        let max_scroll = (bottom - vh).max(0.0);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }

        let area = self.page_area();
        {
            let _page = inspect::scope(
                "Settings page",
                "container",
                root,
                scene::rect(area.x, area.y, area.w, area.h),
                |r| r.property("page", self.page.label(self.lang), "view state"),
            );
            scene_out.push_clip_layer(
                vello::peniko::Fill::NonZero,
                root,
                &scene::rect(area.x, area.y, area.w, area.h),
            );
            let content = root * Affine::translate((0.0, -(self.scroll as f64)));
            let pointed = (!self.availability.is_down() && self.open.is_none())
                .then(|| self.pointed(self.pointer.0, self.pointer.1))
                .flatten();
            let hovered = match pointed {
                Some(Pointed::Hit(index)) => Some(index),
                _ => None,
            };
            let blocks = std::mem::take(&mut self.laid.blocks);
            for block in &blocks {
                self.draw_block(scene_out, content, block, hovered);
            }
            self.laid.blocks = blocks;
            scene_out.pop_layer();
        }

        if max_scroll > 0.0 {
            self.draw_scrollbar(scene_out, root, vw, vh, bottom, max_scroll);
        }

        self.draw_sidebar(scene_out, root);

        if let Some(open) = self.open.as_ref() {
            let enabled = vec![!self.availability.is_down(); open.menu.items().len()];
            open.menu
                .draw(scene_out, root, &mut self.engine, self.viewport, &enabled);
        }
    }

    fn alpha(&self) -> f32 {
        if self.availability.is_down() {
            DIMMED
        } else {
            1.0
        }
    }

    fn draw_sidebar(&mut self, scene_out: &mut Scene, root: Affine) {
        let p = theme::p();
        let s = self.sidebar_rect();
        let _sidebar = inspect::scope(
            "Settings sidebar",
            "container",
            root,
            scene::rect(s.x, s.y, s.w, s.h),
            |_| {},
        );
        // No fill and no edge: the nav sits on the page's own background, and
        // its rules are what set it apart.
        self.draw_search(scene_out, root);

        let listed = self.listed();
        let items: Vec<NavItem<'_>> = listed
            .iter()
            .map(|page| NavItem::new(page.label(self.lang)))
            .collect();
        let current = listed.iter().position(|page| *page == self.page);
        let hovered = match self
            .open
            .is_none()
            .then(|| self.pointed(self.pointer.0, self.pointer.1))
            .flatten()
        {
            Some(Pointed::Nav(index)) => Some(index),
            _ => None,
        };
        let nav = self.nav_rect();
        if self.compact() {
            koru_ui::ChipNav::draw(
                scene_out,
                root,
                &mut self.engine,
                &self.nav_chips,
                &items,
                current,
                hovered,
                None,
            );
        } else {
            NavList::draw(
                scene_out,
                root,
                &mut self.engine,
                nav,
                &items,
                current,
                hovered,
                None,
            );
        }
        if items.is_empty() {
            koru_ui::text_paragraph::draw_line(
                scene_out,
                root,
                &mut self.engine,
                koru_i18n::t(self.lang, Key::SettingsSearchEmpty),
                nav.x + 8.0,
                nav.y + 8.0,
                theme::role::meta(),
                "theme::role::meta()",
                p.text_dim,
            );
        }
    }

    /// The search box's text area: the box, less the magnifier's slot.
    fn search_text_rect(&self) -> Rect {
        let r = self.search_rect();
        let hint = if self.compact() { 0.0 } else { SEARCH_HINT_W };
        Rect::new(r.x + 26.0, r.y, (r.w - 30.0 - hint).max(0.0), r.h)
    }

    fn draw_search(&mut self, scene_out: &mut Scene, root: Affine) {
        let p = theme::p();
        let r = self.search_rect();
        let _search = inspect::scope(
            "Settings search",
            "part",
            root,
            scene::rect(r.x, r.y, r.w, r.h),
            |rec| rec.property("focused", self.search_focused, "view state"),
        );
        // A filled pill, outlined only while it has the keyboard.
        let shape = scene::rrect(r.x, r.y, r.w, r.h, 8.0);
        scene::fill(scene_out, root, p.status_bg, &shape);
        if self.search_focused {
            scene::stroke(scene_out, root, p.focus_ring, theme::BORDER_W, &shape);
        }
        // The chord that opens Settings, inside the box's right end — where
        // the sidebar heading used to carry it.
        if !self.compact()
            && let Some(chord) =
                koru_keys::show_hint(koru_keys::ShortcutId::Settings, koru_keys::Section::Heading)
        {
            let style = theme::role::hint();
            let hint = self.engine.simple_layout(&chord, style);
            koru_ui::text_paragraph::draw_line(
                scene_out,
                root,
                &mut self.engine,
                &chord,
                r.x + r.w - 8.0 - hint.width(),
                r.y + (r.h - scene::first_line_height(&hint)) * 0.5,
                style,
                "theme::role::hint()",
                p.placeholder,
            );
        }
        let glass = self.engine.icon_layout(icons::SEARCH, 13.0);
        scene::draw_layout(
            scene_out,
            root,
            &glass,
            r.x + 9.0,
            r.y + (r.h - glass.height()) * 0.5,
            Some(p.placeholder),
        );
        let text = self.search_text_rect();
        self.search.draw(
            scene_out,
            root,
            &mut self.engine,
            text,
            InputStyle {
                fill: None,
                border: None,
                ..InputStyle::search()
            },
            Some(koru_i18n::t(self.lang, Key::SettingsSearchPlaceholder)),
            self.search_focused,
        );
    }

    fn draw_block(
        &mut self,
        scene_out: &mut Scene,
        root: Affine,
        block: &Block,
        hovered: Option<usize>,
    ) {
        let p = theme::p();
        let alpha = self.alpha();
        match block {
            Block::Title {
                title,
                description,
                x,
                y,
                w,
                title_h,
            } => {
                koru_ui::text_paragraph::KoruTextParagraph::new(
                    title,
                    title_style(),
                    "theme::role::title()",
                    p.text,
                )
                .draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    Rect::new(*x, *y, *w, 0.0),
                );
                koru_ui::text_paragraph::KoruTextParagraph::new(
                    description,
                    theme::role::ui(),
                    "theme::role::ui()",
                    p.text_dim,
                )
                .draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    Rect::new(*x, *y + *title_h + 8.0, *w, 0.0),
                );
            }
            Block::Note { text, rect } => {
                koru_ui::text_paragraph::KoruTextParagraph::new(
                    text,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim,
                )
                .draw(scene_out, root, &mut self.engine, *rect);
            }
            Block::Tiles { title, y, x, tiles } => {
                if let Some(title) = title {
                    koru_ui::text_paragraph::draw_line(
                        scene_out,
                        root,
                        &mut self.engine,
                        title,
                        *x,
                        *y,
                        theme::role::meta(),
                        "theme::role::meta()",
                        p.text_dim.multiply_alpha(alpha),
                    );
                }
                for tile in tiles {
                    PreviewTile::new(&tile.label, tile.selected).draw(
                        scene_out,
                        root,
                        &mut self.engine,
                        tile.rect,
                        hovered == Some(tile.hit),
                        alpha,
                        |canvas, root, area| paint_theme_preview(canvas, root, area, tile.preview),
                    );
                }
            }
            Block::Card(card) => {
                let dividers: Vec<f32> = card.rows.iter().skip(1).map(|row| row.rect.y).collect();
                let _card = Card {
                    title: card.title.as_deref(),
                    aside: card.aside.as_deref(),
                }
                .draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    card.rect,
                    &dividers,
                    alpha,
                );
                if card.font_preview {
                    let samples = [
                        koru_ui::FontSample {
                            text: "body",
                            stack: theme::body_stack(),
                            weight: theme::FontWeight::Medium,
                        },
                        koru_ui::FontSample {
                            text: "Heading",
                            stack: theme::headings_font().stack(),
                            weight: theme::FontWeight::SemiBold,
                        },
                        koru_ui::FontSample {
                            text: "code",
                            stack: theme::code_stack(),
                            weight: theme::FontWeight::Medium,
                        },
                    ];
                    koru_ui::FontPreview {
                        lead: koru_i18n::t(self.lang, Key::SettingsPreview),
                        samples: &samples,
                    }
                    .draw(
                        scene_out,
                        root,
                        &mut self.engine,
                        card.rect.x + card.rect.w,
                        card.rect.y,
                        alpha,
                    );
                }
                for row in &card.rows {
                    let _row = CardRow {
                        label: &row.label,
                        hint: row.hint.as_deref(),
                        link: row.link.as_ref().map(|(text, _)| text.as_str()),
                        chip: row.chip.as_deref(),
                    }
                    .draw(
                        scene_out,
                        root,
                        &mut self.engine,
                        row.rect,
                        row.text_w,
                        alpha,
                    );
                    self.draw_widget(scene_out, root, &row.widget, hovered, alpha);
                }
            }
            // The host draws its own rows through `draw_host_section`.
            Block::Caption { title, x, y, link } => {
                let _caption = inspect::scope(
                    "Settings caption",
                    "part",
                    root,
                    scene::rect(
                        *x,
                        *y,
                        link.as_ref().map_or(0.0, |(_, r, _)| r.x + r.w - *x),
                        Card::TITLE_H,
                    ),
                    |r| r.property("title", title.as_str(), "page content"),
                );
                koru_ui::text_paragraph::draw_line(
                    scene_out,
                    root,
                    &mut self.engine,
                    title,
                    *x,
                    *y,
                    theme::role::meta(),
                    "theme::role::meta()",
                    p.text_dim.multiply_alpha(alpha),
                );
                if let Some((label, rect, hit)) = link {
                    koru_ui::text_paragraph::draw_line(
                        scene_out,
                        root,
                        &mut self.engine,
                        label,
                        rect.x,
                        rect.y,
                        theme::role::meta(),
                        "theme::role::meta() — a link",
                        if hovered == Some(*hit) {
                            p.text
                        } else {
                            p.accent
                        }
                        .multiply_alpha(alpha),
                    );
                }
            }
            Block::Host => {}
        }
    }

    fn draw_widget(
        &mut self,
        scene_out: &mut Scene,
        root: Affine,
        widget: &Widget,
        hovered: Option<usize>,
        alpha: f32,
    ) {
        match widget {
            Widget::None => {}
            Widget::Segmented {
                control,
                labels,
                selected,
                first,
            } => {
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let hovered = hovered
                    .and_then(|h| h.checked_sub(*first))
                    .filter(|h| *h < labels.len());
                control.draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    &refs,
                    *selected,
                    hovered,
                    alpha,
                );
            }
            Widget::Dropdown {
                rect,
                value,
                face,
                hit,
                dropdown,
            } => {
                let mut field = Dropdown::new(value)
                    .open(self.open.as_ref().is_some_and(|o| o.index == *dropdown));
                if let Some(stack) = face {
                    field = field.in_face(stack);
                }
                field.draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    *rect,
                    hovered == Some(*hit),
                    alpha,
                );
            }
            Widget::Toggle { rect, on, hit } => {
                Toggle::new(*on).enabled(alpha >= 1.0).draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    *rect,
                    false,
                    hovered == Some(*hit),
                    false,
                );
            }
            Widget::Swatches {
                rects,
                colors,
                selected,
                first,
            } => {
                if let Some(at) = rects.first() {
                    let row = koru_ui::ColorPickerRow::new(colors)
                        .selected(*selected)
                        .hovered(
                            hovered
                                .and_then(|hit| hit.checked_sub(*first))
                                .filter(|index| *index < rects.len()),
                        )
                        .alpha(alpha);
                    let rect = Rect::new(at.x, at.y, row.width(), row.height());
                    row.draw(scene_out, root, &mut self.engine, rect);
                }
            }
            Widget::Button {
                rect,
                label,
                style,
                hit,
            } => {
                ActionButton {
                    label,
                    style: *style,
                    trailing_icon: None,
                }
                .draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    *rect,
                    hovered == Some(*hit),
                );
            }
            Widget::Stepper {
                control,
                value,
                less,
                more,
            } => {
                let step = match hovered {
                    Some(h) if Some(h) == *less => Some(Step::Less),
                    Some(h) if Some(h) == *more => Some(Step::More),
                    _ => None,
                };
                control.draw(
                    scene_out,
                    root,
                    &mut self.engine,
                    value,
                    less.is_some(),
                    more.is_some(),
                    step,
                    alpha,
                );
            }
            Widget::Pair(first, second) => {
                self.draw_widget(scene_out, root, first, hovered, alpha);
                self.draw_widget(scene_out, root, second, hovered, alpha);
            }
        }
    }

    /// Draw the host-owned rows with the same clip and scroll transform as
    /// the shared rows. Call after [`draw`](Self::draw), which refreshes the
    /// slot. Nothing is drawn unless the Reminders page is showing.
    pub fn draw_host_section(
        &self,
        scene_out: &mut Scene,
        root: Affine,
        draw: impl FnOnce(&mut Scene, Affine, Rect),
    ) {
        let Some(slot) = self.laid.host.filter(|slot| slot.h > 0.0) else {
            return;
        };
        let area = self.page_area();
        scene_out.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(area.x, area.y, area.w, area.h),
        );
        let content_root = root * Affine::translate((0.0, -(self.scroll as f64)));
        draw(scene_out, content_root, slot);
        scene_out.pop_layer();
    }

    fn draw_scrollbar(
        &self,
        scene_out: &mut Scene,
        root: Affine,
        vw: f32,
        vh: f32,
        bottom: f32,
        max_scroll: f32,
    ) {
        let thumb_h = (vh * vh / bottom).max(theme::SCROLLBAR_MIN_THUMB).min(vh);
        let travel = (vh - thumb_h).max(0.0);
        let thumb_top = travel * (self.scroll / max_scroll);
        let gutter_x = (vw - theme::SCROLLBAR_EDGE_CLEAR - theme::SCROLLBAR_GUTTER_W).max(0.0);
        let gutter_w = theme::SCROLLBAR_GUTTER_W.min(vw - gutter_x).max(0.0);
        let over = self.pointer.0 >= gutter_x
            && self.pointer.0 <= gutter_x + gutter_w
            && self.pointer.1 >= 0.0
            && self.pointer.1 <= vh;
        let tw = theme::SCROLLBAR_W.min(gutter_w);
        let tx = gutter_x + gutter_w - tw;
        scene::fill(
            scene_out,
            root,
            theme::scrollbar_thumb_color(over),
            &scene::rrect(tx, thumb_top, tw, thumb_h, tw * 0.5),
        );
    }
}

static EMPTY_AI: std::sync::LazyLock<AiSlot> = std::sync::LazyLock::new(AiSlot::default);

/// What a theme tile shows: a sliver of that theme's page and ink, from the
/// real palettes rather than a drawing of them.
fn paint_theme_preview(canvas: &mut Scene, root: Affine, area: Rect, preview: TilePreview) {
    let sheet = |canvas: &mut Scene, rect: Rect, palette: &theme::Palette| {
        scene::fill(
            canvas,
            root,
            palette.bg,
            &scene::rect(rect.x, rect.y, rect.w, rect.h),
        );
        let bar = |canvas: &mut Scene, y: f32, w: f32, color: Color| {
            scene::fill(
                canvas,
                root,
                color,
                &scene::rrect(rect.x + 14.0, rect.y + y, w, 6.0, 3.0),
            );
        };
        bar(
            canvas,
            16.0,
            rect.w * 0.55,
            palette.text.multiply_alpha(0.35),
        );
        bar(
            canvas,
            29.0,
            rect.w * 0.7,
            palette.text_dim.multiply_alpha(0.3),
        );
        bar(
            canvas,
            42.0,
            rect.w * 0.4,
            palette.accent.multiply_alpha(0.6),
        );
    };
    match preview {
        TilePreview::Light => sheet(canvas, area, &theme::LIGHT),
        TilePreview::Dark => sheet(canvas, area, &theme::DARK),
        TilePreview::System => {
            let half = area.w * 0.5;
            sheet(
                canvas,
                Rect::new(area.x, area.y, half, area.h),
                &theme::LIGHT,
            );
            sheet(
                canvas,
                Rect::new(area.x + half, area.y, area.w - half, area.h),
                &theme::DARK,
            );
        }
    }
}

// ------------------------------------------------------------------ presses

impl SettingsSurface {
    /// Do what a control says. The action is read out of the layout the press
    /// was tested against, so a control cannot do something other than what it
    /// was drawn as.
    fn perform(&mut self, action: Action) {
        match action {
            Action::Theme(choice) => theme::set_theme_preference(theme_preference(choice)),
            Action::CaretColor(choice) => theme::set_caret_color(caret_color(choice)),
            Action::CaretWidth(choice) => theme::set_caret_width(caret_width(choice)),
            Action::CaretHeight(choice) => theme::set_caret_height(caret_height(choice)),
            Action::ViewMode(choice) => {
                let mode = view_mode(choice);
                self.view_mode = mode;
                // Live: a document opened now picks this up, before the next save.
                theme::set_default_view_mode(mode);
            }
            Action::CanvasBackground => self.requests.push(Request::CycleCanvasBackground),
            Action::ApplyLayoutToAll => {
                self.layout_applied = true;
                self.requests.push(Request::ApplyLayoutToAllDocuments);
            }
            Action::ShowAdvanced(open) => self.advanced_open = open,
            Action::Density(choice) => theme::set_density(density(choice)),
            Action::PinnedShowTitle(show) => self.pinned_show_title = show,
            Action::Font(role, family) => role.set(family),
            Action::VoiceLanguage(language) => {
                self.voice_language = language.clone();
                self.requests.push(Request::SetVoiceLanguage(language));
            }
            Action::VoiceInput(input) => {
                self.voice_input = input.clone();
                self.requests.push(Request::SetVoiceInput(input));
            }
            Action::VoiceTier(tier) => {
                self.voice_tier = tier;
                self.requests.push(Request::SetVoiceTier(tier));
            }
            Action::VoiceGesture(gesture) => {
                self.voice_gesture = gesture;
                self.requests.push(Request::SetVoiceGesture(gesture));
            }
            Action::SpellcheckEnabled(on) => {
                self.spelling_enabled = on;
                self.requests.push(Request::SetSpellcheckEnabled(on));
            }
            Action::SpellcheckLanguage(language) => {
                self.spelling_language = language.clone();
                self.requests.push(Request::SetSpellcheckLanguage(language));
            }
            Action::ClearPersonalDictionary => self.requests.push(Request::ClearPersonalDictionary),
            Action::DownloadDictionary => self.requests.push(Request::DownloadDictionary),
            Action::VoiceModels => self.requests.push(if self.voice.model_removes {
                Request::RemoveVoiceModels
            } else {
                Request::InstallVoiceModels
            }),
            Action::RevokeVoiceConsent => self.requests.push(Request::RevokeVoiceConsent),
            Action::Renderer(token) => self.requests.push(Request::SetRenderer(token)),
            Action::CloseBehavior(choice) => {
                if choice != self.close_behavior {
                    self.close_behavior = choice;
                    self.requests.push(Request::SetCloseBehavior(choice));
                }
            }
            Action::StartOnLogin(value) => self.patch_global(GlobalPatch {
                start_on_login: Some(value),
                ..GlobalPatch::default()
            }),
            // The switch moves at once and the host performs the write off
            // the event loop; the host's next reading of the record puts it
            // back if the write did not land.
            Action::TelemetryConsent(on) => {
                self.telemetry = Some(on);
                self.requests.push(Request::SetTelemetryConsent(on));
            }
            Action::TerminalTheme(choice) => self.patch_terminal(TerminalPatch {
                theme: Some(choice),
                ..TerminalPatch::default()
            }),
            Action::TerminalFont(family) => self.patch_terminal(TerminalPatch {
                font: Some(FontSetting::Chosen(family)),
                ..TerminalPatch::default()
            }),
            Action::TerminalZoom(factor) => self.patch_terminal(TerminalPatch {
                zoom: Some(factor),
                ..TerminalPatch::default()
            }),
            Action::TerminalCursorColor(choice) => self.patch_terminal(TerminalPatch {
                cursor_color: Some(choice),
                ..TerminalPatch::default()
            }),
            Action::TerminalCursorWidth(choice) => self.patch_terminal(TerminalPatch {
                cursor_width: Some(choice),
                ..TerminalPatch::default()
            }),
            Action::TerminalCursorHeight(choice) => self.patch_terminal(TerminalPatch {
                cursor_height: Some(choice),
                ..TerminalPatch::default()
            }),
            Action::TerminalCursorBlink(choice) => self.patch_terminal(TerminalPatch {
                cursor_blink: Some(choice),
                ..TerminalPatch::default()
            }),
            Action::TerminalComposerDefault(value) => self.patch_terminal(TerminalPatch {
                composer_default: Some(value),
                ..TerminalPatch::default()
            }),
            // One patch, two fields: the switch is the two of them.
            Action::TerminalMatchCaret(true) => self.patch_terminal(TerminalPatch {
                cursor_width: Some(TerminalCursorWidth::Application),
                cursor_height: Some(TerminalCursorHeight::Application),
                ..TerminalPatch::default()
            }),
            Action::TerminalMatchCaret(false) => {
                let (width, height) = terminal::caret_now();
                self.patch_terminal(TerminalPatch {
                    cursor_width: Some(width),
                    cursor_height: Some(height),
                    ..TerminalPatch::default()
                })
            }
            Action::OpenAiSetup => self.requests.push(Request::OpenAiSetup),
        }
    }

    /// **Applied here, sent at once.** The mirror moves before the patch goes
    /// out so the control answers the press immediately; the authority's reply
    /// then replaces it, which is what makes a refusal reconcile rather than
    /// compound.
    fn patch_global(&mut self, patch: GlobalPatch) {
        patch.clone().apply(&mut self.global);
        self.links.save_global(patch);
        self.notice = None;
    }

    fn patch_terminal(&mut self, patch: TerminalPatch) {
        patch.clone().apply(&mut self.terminal);
        self.links.save_terminal(patch);
        self.notice = None;
    }

    /// **Did what came back match what was asked for?**
    ///
    /// Only the fields this surface itself patched are compared, and only while
    /// one of them is outstanding — a field this surface never named moving is
    /// another window's change arriving, which is convergence rather than
    /// refusal.
    fn reconcile_global(&mut self, arriving: &GlobalSettings) {
        let Some(outstanding) = self.links.take_outstanding_global() else {
            return;
        };
        if outstanding.start_on_login.is_some()
            && arriving.start_on_login != self.global.start_on_login
        {
            self.notice = Some(self.not_saved());
        }
    }

    fn reconcile_terminal(&mut self, arriving: &TerminalSettings) {
        let Some(outstanding) = self.links.take_outstanding_terminal() else {
            return;
        };
        let mut wanted = self.terminal.clone();
        outstanding.apply(&mut wanted);
        if *arriving != wanted {
            self.notice = Some(self.not_saved());
        }
    }

    fn not_saved(&self) -> Notice {
        Notice {
            text: koru_i18n::t(self.lang, Key::SettingsNotSaved).to_owned(),
            at: std::time::Instant::now(),
        }
    }
}

// ------------------------------------------------- the two vocabularies meet

/// The registry's token for a theme, as `koru-draw`'s live global spells it.
///
/// Both sides parse by token, and `koru_settings`' doc comment says the tokens
/// are `koru_draw::theme`'s — so a round trip through the token is the one
/// conversion that cannot drift when either enum grows.
fn theme_preference(choice: ThemeChoice) -> theme::ThemePreference {
    theme::ThemePreference::parse(choice.token())
}

fn theme_choice(preference: theme::ThemePreference) -> ThemeChoice {
    ThemeChoice::parse(preference.key())
}

fn caret_color(choice: CaretColorChoice) -> theme::CaretColor {
    theme::CaretColor::parse(choice.token())
}

fn caret_color_choice(color: theme::CaretColor) -> CaretColorChoice {
    CaretColorChoice::parse(color.key())
}

fn caret_width(choice: CaretWidthChoice) -> theme::CaretWidth {
    theme::CaretWidth::parse(choice.token())
}

fn caret_height(choice: CaretHeightChoice) -> theme::CaretHeight {
    theme::CaretHeight::parse(choice.token())
}

fn density(choice: DensityChoice) -> theme::Density {
    theme::Density::parse(choice.token())
}

fn view_mode(choice: ViewModeChoice) -> theme::ViewMode {
    match choice {
        ViewModeChoice::Page => theme::ViewMode::Page,
        ViewModeChoice::Narrow => theme::ViewMode::Narrow,
        ViewModeChoice::Full => theme::ViewMode::Full,
    }
}

fn view_mode_choice(mode: theme::ViewMode) -> ViewModeChoice {
    match mode {
        theme::ViewMode::Page => ViewModeChoice::Page,
        theme::ViewMode::Narrow => ViewModeChoice::Narrow,
        theme::ViewMode::Full => ViewModeChoice::Full,
    }
}

/// `Action` is not `Clone` — it carries owned device names — so the one place
/// that needs a copy names the cost rather than deriving it everywhere.
fn clone_action(action: &Action) -> Action {
    match action {
        Action::Theme(v) => Action::Theme(*v),
        Action::CaretColor(v) => Action::CaretColor(*v),
        Action::CaretWidth(v) => Action::CaretWidth(*v),
        Action::CaretHeight(v) => Action::CaretHeight(*v),
        Action::ViewMode(v) => Action::ViewMode(*v),
        Action::CanvasBackground => Action::CanvasBackground,
        Action::ApplyLayoutToAll => Action::ApplyLayoutToAll,
        Action::Density(v) => Action::Density(*v),
        Action::PinnedShowTitle(v) => Action::PinnedShowTitle(*v),
        Action::Font(role, family) => Action::Font(*role, *family),
        Action::VoiceLanguage(v) => Action::VoiceLanguage(v.clone()),
        Action::VoiceInput(v) => Action::VoiceInput(v.clone()),
        Action::VoiceTier(v) => Action::VoiceTier(*v),
        Action::VoiceGesture(v) => Action::VoiceGesture(*v),
        Action::SpellcheckEnabled(v) => Action::SpellcheckEnabled(*v),
        Action::SpellcheckLanguage(v) => Action::SpellcheckLanguage(v.clone()),
        Action::ClearPersonalDictionary => Action::ClearPersonalDictionary,
        Action::DownloadDictionary => Action::DownloadDictionary,
        Action::VoiceModels => Action::VoiceModels,
        Action::RevokeVoiceConsent => Action::RevokeVoiceConsent,
        Action::Renderer(v) => Action::Renderer(v.clone()),
        Action::CloseBehavior(v) => Action::CloseBehavior(*v),
        Action::StartOnLogin(v) => Action::StartOnLogin(*v),
        Action::TelemetryConsent(v) => Action::TelemetryConsent(*v),
        Action::ShowAdvanced(v) => Action::ShowAdvanced(*v),
        Action::TerminalTheme(v) => Action::TerminalTheme(*v),
        Action::TerminalFont(v) => Action::TerminalFont(*v),
        Action::TerminalZoom(v) => Action::TerminalZoom(*v),
        Action::TerminalCursorColor(v) => Action::TerminalCursorColor(*v),
        Action::TerminalCursorWidth(v) => Action::TerminalCursorWidth(*v),
        Action::TerminalCursorHeight(v) => Action::TerminalCursorHeight(*v),
        Action::TerminalCursorBlink(v) => Action::TerminalCursorBlink(*v),
        Action::TerminalComposerDefault(v) => Action::TerminalComposerDefault(*v),
        Action::TerminalMatchCaret(v) => Action::TerminalMatchCaret(*v),
        Action::OpenAiSetup => Action::OpenAiSetup,
    }
}

/// The canvas swatch's colour. `None` follows the theme canvas, which is what
/// *Auto* means and is why the swatch is a colour rather than a word.
fn canvas_swatch_color(rgba: Option<[u8; 4]>) -> Color {
    match rgba {
        Some([r, g, b, a]) => Color::from_rgba8(r, g, b, a),
        None => theme::p().canvas,
    }
}
