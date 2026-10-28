// SPDX-License-Identifier: MPL-2.0

//! The workspace-aware multi-window host.
//!
//! This crate hosts workspace-backed views in Koru's multi-window loop and owns
//! the workspace-specific lifecycle around them: settings, startup, and
//! application input/import coordination. Generic platform window behaviour
//! belongs to `koru-window`; terminal and process-session behaviour belongs to
//! the session crates. The host is spelled in winit, vello, `koru-core` and
//! `koru-workspace` terms and **names no editor** — check `Cargo.toml` rather
//! than believing this sentence. A calendar that is not a list of paragraphs
//! plugs into the same loop, which is the point of the boundary.
//!
//! Two traits:
//!
//! - [`View`] is what a window shows. One per pane.
//! - [`Host`] is what *makes* views — the one thing the loop cannot know, since
//!   opening a document means constructing a kind of view this crate has never
//!   heard of.
//!
//! The document is not here either. It lives once, in [`Workspace`], and every
//! pane borrows it for the length of a call. That is what lets two windows show
//! one document and see each other's typing.

/// Semantic identity, the AccessKit projection, and the routing of a screen
/// reader's request back to the behaviour that already owns it. The platform
/// half — when an adapter exists at all — is `koru_window::accessibility`.
pub mod accessibility;
pub mod account_ui;
pub mod app;
mod binding_markers;
pub mod onboarding;

pub mod cli;
mod clipboard_html;
mod clipboard_markdown;
mod clipboard_rtf;
pub mod diagram;
pub mod dialogs;
pub mod environment;
pub mod events_host;
pub mod export;
pub mod formats;
pub mod images;
mod inspector;
/// What the window actually received, on demand — `KORU_INPUT_DEBUG=1`.
///
/// **Lives in `koru-ui`** as `keys::debug`, beside the `ctrl_letter` whose
/// answer it prints, so a reusable surface can say what key it received without
/// linking a workspace to say it. Re-exported here under its old name, which is
/// what every call site in `apps/koru` already says.
pub use koru_ui::keys::debug as inputdebug;
pub mod attachdebug;
#[cfg(target_os = "macos")]
mod macos_urls;
/// The macOS menu bar. Stays here rather than in `koru-window`: what a row
/// *does* is a key press dispatched into a view, which is this crate's subject
/// and not a window's.
pub mod menubar;
pub mod menus;
pub mod native_surface;
/// This window's side of the settings authority: the bounded startup wait, the
/// per-field patches, and the two conversions between the registry's section
/// and what `Settings` holds.
pub mod preferences;
mod profile_activation;
mod profile_lifecycle_decision;
mod profile_runtime;
pub mod reveal;
/// Who owns the keyboard, factored so the container and the built-in
/// document cannot drift apart about it. See the module's own note.
pub mod routing;
/// The pill a delegation leaves in the document it came from (D7, D8).
pub mod runpill;
/// The schedule counterpart to `runpill`: a durable recurring-work crumb.
pub mod schedulepill;
pub mod searchdebug;
pub mod settings;
pub mod spelling;
mod url_file;

pub mod startup;
/// The desktop task wrapper: scheduling and attribution around the six
/// canonical commands, and nothing else.
pub mod tasks;
/// This device's telemetry answer, read and written off the event loop.
pub mod telemetry_consent;
/// First use: the consent step, the record it writes, and the download it
/// authorises. See the module note for where each half of the gate lives.
pub mod voice_devices;
pub mod voice_install;
/// The three dictation preferences that follow the person — language, quality
/// and hold-versus-toggle — held as process globals over `Settings`.
pub mod voice_prefs;

/// Write search routing/index-read diagnostics only when
/// `KORU_SEARCH_DEBUG` is present in the process environment.
#[macro_export]
macro_rules! search_debug {
    ($($arg:tt)*) => {
        $crate::searchdebug::write(format_args!($($arg)*))
    };
}

/// Attachment slash / pill / open-save diagnostics when `KORU_ATTACH_DEBUG=1`.
#[macro_export]
macro_rules! attach_debug {
    ($($arg:tt)*) => {
        $crate::attachdebug::write(format_args!($($arg)*))
    };
}

use std::path::PathBuf;

use koru_core::schema::WorkspaceDoc;
use koru_core::{Heading, ListStyle, MarkSet, NodeId, ProfileId, WorkspaceId};
use koru_draw::ImageCache;
use koru_window::WindowRequest;
use vello::Scene;
use vello::kurbo::Affine;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReminderStartupRequestId(uuid::Uuid);

impl ReminderStartupRequestId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ReminderStartupRequestId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderStartupRequest {
    pub id: ReminderStartupRequestId,
    pub accepted: bool,
}

impl ReminderStartupRequest {
    pub fn new(accepted: bool) -> Self {
        Self {
            id: ReminderStartupRequestId::new(),
            accepted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderStartupResult {
    pub id: ReminderStartupRequestId,
    pub actual: koru_window::ReminderStartupFacts,
    pub error: Option<String>,
}

/// A notification-originated request to begin snooze management for one
/// already-validated occurrence. It deliberately carries no duration or UI
/// placement; the receiving reminder controller owns the semantic chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSnoozeIntent {
    pub profile: koru_store_core::ProfileId,
    pub reminder: koru_store_core::ReminderId,
    pub occurrence: koru_store_core::OccurrenceKey,
    pub entity: koru_store_core::EntityRef,
}
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use koru_workspace::Workspace;

pub use app::{run, run_with_args};
/// Markdown into [`RichBlock`]s — the same reader a Markdown paste goes through.
///
/// Exposed for the *What's new* tab, which is a document built from a release's
/// own notes. See the note on the function.
pub use clipboard_markdown::parse as parse_markdown;
/// What a view hands the shell when *Save as…* or *Copy image* is asked on
/// a drawn diagram.
pub use diagram::{CopyDiagram, SaveDiagram};
pub use settings::{
    DocViewPrefs, MAX_RECENTS, PinnedTabRef, PopoutClose, Settings, TabRef, TasksTabRef,
};

/// A decoded image ready to become a block, produced by the shell from the
/// clipboard. The bytes are already in the blob store under `hash` — the view
/// never touches disk, so it takes only the reference and the geometry.
#[derive(Clone)]
pub struct PastedImage {
    /// The content-addressed hash, as written by `BlobStore::put`.
    pub hash: String,
    /// The MIME type, e.g. `image/png`.
    pub mime: String,
    /// True pixel width, if the decoder reported one.
    pub width: Option<u32>,
    /// True pixel height, if the decoder reported one.
    pub height: Option<u32>,
    /// A source name (filename stem, `<img alt>`, …) for the explore column;
    /// `None` when the source carried none (bitmap, RTF `\pict`).
    pub name: Option<String>,
}

/// Where a deferred block paste belongs.
///
/// Clipboard decoding can finish after the editor action that requested it, so
/// the shell must carry the selection that existed at that moment rather than
/// ask whichever rows happen to be selected later. An empty `replacing` list
/// means insert below `anchor`; otherwise the listed selection roots are
/// removed and the payload takes `anchor`'s slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasteTarget {
    pub anchor: NodeId,
    pub replacing: Vec<NodeId>,
}

/// A paragraph of text plus the block-level structure it arrived with — a list
/// style, a heading level — so a paste from Word or Google Docs keeps its shape
/// rather than collapsing to flat prose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RichTextBlock {
    /// The paragraph's text, entities already decoded.
    pub text: String,
    /// The list style and nesting depth, when the source wrapped this paragraph
    /// in `<ul>`/`<ol>` (HTML) or carried list numbering (RTF).
    pub list: Option<RichListSpec>,
    /// A heading level, when the source marked the paragraph as `<h1>`–`<h6>`.
    pub heading: Option<Heading>,
    /// The semantic text block carried by the source.
    pub kind: RichTextKind,
    /// Marked byte ranges within `text`. Unmarked text needs no run: a new node
    /// starts plain, and these ranges apply the formatting the source asserted.
    pub runs: Vec<RichTextRun>,
    /// Link labels are ordinary text in `text`; these byte ranges carry the
    /// destination that is not visible there. The document replaces each range
    /// with its native labelled-link pill after applying text marks.
    pub links: Vec<RichTextLink>,
}

impl RichTextBlock {
    /// A plain paragraph with no list or heading structure.
    pub fn from_text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            list: None,
            heading: None,
            kind: RichTextKind::Paragraph,
            runs: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// One formatted byte range in a rich clipboard paragraph. This reuses Koru's
/// mark vocabulary; the shell transports it and the document applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextRun {
    pub start: usize,
    pub end: usize,
    pub marks: MarkSet,
}

/// One labelled hyperlink in a rich clipboard paragraph. `start..end` is a
/// UTF-8 byte range in [`RichTextBlock::text`], matching Koru and parley's text
/// boundary convention; `url` is kept separately so the label can be edited or
/// rendered without losing its destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTextLink {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

/// A Markdown table carried intact from the platform clipboard. The document
/// stores this source in one `Kind::Table` node; parsing and rendering remain
/// table-block behaviour rather than shell behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichTableBlock {
    pub source: String,
}

/// Semantic text blocks the shell can carry without depending on the core
/// node vocabulary. Keeping this small makes unsupported HTML fall back to
/// prose instead of letting clipboard input manufacture arbitrary node kinds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RichTextKind {
    #[default]
    Paragraph,
    Quote,
    /// A fenced or indented CommonMark code block. The optional info string is
    /// the source language (the first word after the opening fence).
    Code {
        language: Option<String>,
    },
}

/// A list marker carried in a [`RichTextBlock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RichListSpec {
    pub style: ListStyle,
    /// 0-based nesting depth inferred from the source's list nesting.
    pub depth: u16,
}

/// One piece of a mixed-content paste (text and images interleaved, in document
/// order). Built by the shell from the clipboard's RTF and handed to
/// [`View::paste_rich`] so a paste from Word lands its paragraphs and pictures
/// in the right order in one undo step.
pub enum RichBlock {
    /// A paragraph of text, possibly carrying list/heading structure.
    Text(RichTextBlock),
    /// One GFM table, source preserved for lossless editing and copying.
    Table(RichTableBlock),
    /// A decoded image, bytes already stored under `hash`.
    Image(PastedImage),
}

/// Whether a view already has a document open as a tab.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TabPresence {
    Pinned,
    Open,
    Absent,
}

/// What a window shows.
///
/// The mouse and key methods return whether anything changed, so the loop only
/// redraws when it has to rather than spinning.
pub trait View {
    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool;
    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool;
    fn mouse_move(&mut self, x: f32, y: f32) -> bool;
    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool;
    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool;

    /// A right-click at `(x, y)`. Open a context menu if this view has one.
    ///
    /// Kept separate from [`View::mouse_down`](#method.mouse_down) — which only
    /// carries a `ctrl` flag — so a view that has no context menu does not have
    /// to change, and so the many test harnesses that call `mouse_down` are
    /// untouched. Default no-op: only a container that owns chrome implements it.
    /// Returns whether anything changed (a menu opened or closed).
    fn context_menu(&mut self, _ws: &mut WorkspaceDoc, _x: f32, _y: f32) -> bool {
        false
    }
    fn scroll_by(&mut self, delta: MouseScrollDelta);
    fn set_viewport(&mut self, w: f32, h: f32);

    /// **Primary+wheel, offered to the view before it zooms the app.**
    ///
    /// Ctrl+wheel (⌘+wheel on macOS) — and a Windows precision-touchpad pinch,
    /// which arrives as the same thing — is the display zoom's by default. A
    /// view with a zoom of its own, a canvas, claims it by answering `true`,
    /// and the app's zoom does not move. `false`, the default, leaves the
    /// display zoom exactly as it was.
    ///
    /// `delta` is the **raw** delta rather than its sign, so a canvas zooms by
    /// how far the wheel or the pinch moved; `(x, y)` is the pointer in the
    /// view's own coordinates — the space `mouse_move` is handed, display zoom
    /// already divided out — because a canvas zooms around the point under it.
    fn zoom_wheel(&mut self, _delta: MouseScrollDelta, _x: f32, _y: f32) -> bool {
        false
    }

    /// **Ctrl+= / Ctrl+- / Ctrl+0, offered to the view before they zoom the
    /// app** — [`zoom_wheel`](Self::zoom_wheel)'s keyboard half, with the same
    /// contract: `true` claims the chord and the display zoom does not move.
    fn zoom_key(&mut self, _intent: ZoomIntent) -> bool {
        false
    }

    /// **A key coming up**, after the shell has settled `down_keys` and the
    /// modifiers for it.
    ///
    /// Until the canvas no view needed a release: a gesture that ends on one —
    /// a held arrow key nudging a card, written once when the last arrow is let
    /// go — has nowhere else to end. The document is handed over because that
    /// write happens here; the shell treats a change it makes the way it treats
    /// a key press's. `true` asks for a frame. The default declines, so every
    /// other view is exactly as it was.
    fn key_up(&mut self, _ws: &mut WorkspaceDoc, _key: &Key) -> bool {
        false
    }

    /// The current modifier keys, pushed whenever they change, so a mouse drag
    /// — which has no modifiers argument — can still tell whether Shift is held.
    fn set_mods(&mut self, _mods: ModifiersState) {}
    /// Event-driven refresh seam; never a polling timer.
    fn set_window_focused(&mut self, _focused: bool) {}

    /// Whether the window now fills the screen, pushed whenever it changes.
    ///
    /// A view that draws its own caption row needs this — a maximise button has
    /// to become a restore button — and it has no `Window` to ask. Pushed
    /// rather than polled for the same reason `set_mods` is: it changes rarely,
    /// and reading it per frame would be a system call per frame.
    fn set_maximized(&mut self, _maximized: bool) {}

    /// Whether the window is now pinned above other windows, pushed whenever it
    /// changes. Per-window: only the window whose pin was toggled is told.
    ///
    /// Same contract as [`set_maximized`](Self::set_maximized) — a caption that
    /// draws its own pin button needs to show it pressed, and has no `Window` to
    /// ask for the `WindowLevel`.
    fn set_pinned(&mut self, _pinned: bool) {}

    /// Whether this pane is the dedicated global-search window.  The shell
    /// uses this only to focus an existing one instead of opening duplicates.
    fn is_global_search(&self) -> bool {
        false
    }

    /// Open a target selected by search in a view that can host tabs.  A
    /// dedicated search pane declines; the container accepts.
    fn open_search_target(
        &mut self,
        _ws: &WorkspaceDoc,
        _document: Option<NodeId>,
        _node: Option<NodeId>,
        _trash_root: Option<NodeId>,
        _match_text: Option<&str>,
    ) -> bool {
        false
    }

    /// Open the profile-global reminder destination in this tab host.
    fn open_all_reminders(&mut self) -> bool {
        false
    }

    /// Reveal an opaque file selected in global search. A file is a manifest
    /// entry rather than an editable document, so the container switches its
    /// navigator to Files and selects the row instead of trying to open a tab.
    fn open_file_search_target(&mut self, _file: NodeId) -> bool {
        false
    }

    /// An install finished; show this build's own release notes.
    ///
    /// Declined by anything that is not a tab host, exactly as
    /// [`Self::open_search_target`] is — a dedicated search window has nowhere
    /// to put a page, and answering `false` is how it says so.
    ///
    /// **The version comes from the agent rather than from `build_info()`.**
    /// They are the same string whenever the four binaries are the ones that
    /// were installed together, and they stop being the same the moment a stale
    /// `koru.exe` is talking to a freshly updated agent — in which case the
    /// agent is the one that knows what was installed.
    fn open_whats_new(&mut self, _version: &str) -> bool {
        false
    }

    /// Open or focus the Send feedback tab in this container.
    fn open_feedback_tab(&mut self) -> bool {
        false
    }

    /// Whether this pane is the standalone Feedback window the tray opens.
    fn is_feedback_window(&self) -> bool {
        false
    }

    /// Close a Feedback tab in this container so the tray can pop it out.
    fn close_feedback_tab(&mut self) -> bool {
        false
    }

    /// The pointer shape for wherever the pointer was last put. Asked after
    /// every move, so it is a question about the view's own state rather than
    /// about a point — `mouse_move` has already been told where that is.
    fn cursor_icon(&self) -> CursorIcon {
        CursorIcon::Default
    }

    /// The document changed underneath this view — a remote edit, or another
    /// window's typing. Rebuilding is always the right answer.
    fn adopt(&mut self, ws: &WorkspaceDoc);

    /// The theme is process-global and baked into every layout, so a flip in one
    /// window invalidates every other window's cached drawing too.
    fn invalidate(&mut self);

    /// What this view offers a screen reader, in this window's **logical**
    /// coordinates.
    ///
    /// **`&self`, and that is the load-bearing part.** A semantic query reads
    /// prepared in-memory state and must not lay anything out;
    /// `Editing::reflow` takes `&mut`, so a view physically cannot call it
    /// from here. Nor is a `Workspace`, a session or an image cache passed in,
    /// so this cannot read the Store either. The rule is enforced by the
    /// signature rather than by a comment somebody has to obey.
    ///
    /// `view` is the instance slot the host allocated for this view; every
    /// occurrence a view mints is scoped by it, which is what keeps the same
    /// document shown in two windows two independently focusable things.
    ///
    /// `scope` is what the host is publishing: a
    /// [`Whole`](crate::accessibility::Scope::Whole) window, which a newly
    /// attached client is owed and which is the only thing that may follow one
    /// detaching, or a [`Delta`](crate::accessibility::Scope::Delta), which is
    /// everything about this view that changed since the last publish. **A view
    /// may always answer a `Delta` with everything**, and one whose projection
    /// is a handful of controls should: the difference is a cost decision, and
    /// it is only worth taking where the whole is O(the document). A view that
    /// *does* take it returns loose subtrees whose parents the client already
    /// holds, and names anything that has gone through
    /// [`accessibility_retired`](Self::accessibility_retired).
    ///
    /// Empty by default: a view has nothing to say until its own plan gives it
    /// a projection, and an empty contribution costs nothing.
    fn semantics(
        &self,
        _view: crate::accessibility::ViewSlot,
        _scope: crate::accessibility::Scope,
    ) -> Vec<crate::accessibility::SemanticNode> {
        Vec::new()
    }

    /// **Controls this view has taken away since the last publish.**
    ///
    /// Only meaningful after a [`Scope::Delta`](crate::accessibility::Scope)
    /// [`semantics`](Self::semantics): a whole snapshot is authoritative on its
    /// own and the host empties the occurrence map for one. In a difference,
    /// nothing else says a control has gone — the consumer drops an unreachable
    /// node by itself, but the map an action is *resolved* against is
    /// accumulated, so a paragraph that left the document would stay routable
    /// forever.
    ///
    /// **Drained, and `&self`.** Asked once, immediately after
    /// [`semantics`](Self::semantics) and against the same borrow, so the two
    /// describe one pass. The interior mutability that lets a `&self` method
    /// drain is the same `RefCell` that lets a view mint AccessKit ids from a
    /// semantic query; neither makes `Editing::reflow` or the Store reachable,
    /// which is what the `&self` is for.
    fn accessibility_retired(&self) -> Vec<crate::accessibility::Occurrence> {
        Vec::new()
    }

    /// **Tell this view which window it is in.**
    ///
    /// Called once, at pane creation, before the window has ever been shown.
    /// An [`Occurrence`](crate::accessibility::Occurrence) is scoped by window
    /// *and* view, and a view does not choose which window it is put in — so
    /// the pair is handed over here rather than threaded through
    /// [`semantics`](Self::semantics), whose signature is deliberately the
    /// narrowest thing that can answer the question (§5: a semantic query
    /// reads prepared in-memory state and nothing else).
    ///
    /// A view that publishes nothing ignores it, which is the default.
    fn seat_accessibility(
        &mut self,
        _window: crate::accessibility::WindowSlot,
        _view: crate::accessibility::ViewSlot,
    ) {
    }

    /// **Which control this view considers focused** — the same state its
    /// focus ring is drawn from, said in the accessibility vocabulary.
    ///
    /// `&self` and a second method rather than a field on the tree, so that
    /// §7.1's *visible focus and accessible focus share the same state* is a
    /// property of where the answer is read from rather than a promise. Both
    /// this and [`semantics`](Self::semantics) run against the same borrow of
    /// the same view in the same pass, so they cannot disagree; a focus naming
    /// a control that is not in the tree is counted as
    /// `Counters::unresolved_focus` rather than published.
    ///
    /// `None` means *the window itself*, which is the honest answer while
    /// nothing inside has the keyboard.
    fn accessibility_focus(
        &self,
        _view: crate::accessibility::ViewSlot,
    ) -> Option<crate::accessibility::Occurrence> {
        None
    }

    /// **Where this view draws the window's own commands.**
    ///
    /// The window performs minimise, maximise and close; the view paints them,
    /// because a Koru window has no OS caption. Answering `default()` — which
    /// every view that draws no caption does — leaves them with no rectangle,
    /// which AccessKit serves as an empty one.
    fn window_command_bounds(&self) -> crate::accessibility::CaptionGeometry {
        crate::accessibility::CaptionGeometry::default()
    }

    /// **A number that changes exactly when this view's semantics change.**
    ///
    /// The host has no other way to know without building the tree, which is
    /// the work the counter exists to avoid. Bump it where the projection's
    /// inputs change — a tab opened, a selection moved, a menu opened — and
    /// *not* on a scroll, a caret blink, an animation frame or a query, none of
    /// which change what is published.
    ///
    /// A constant by default, which is the right answer for a view that
    /// publishes nothing.
    fn semantics_revision(&self) -> u64 {
        0
    }

    /// **Do what a screen reader asked, through the owner that already does
    /// it.**
    ///
    /// `target` has already been resolved against the tree currently on screen,
    /// so a request naming a closed menu or a departed tab never arrives here.
    /// What arrives may still be an action this view does not perform on this
    /// control — Windows sends `Focus` before every invoke whatever a node
    /// advertised — so the answer to one of those is `false` and nothing else.
    ///
    /// **Not a second command path.** An implementation routes to the same
    /// function the keyboard and the pointer reach; inventing an
    /// accessibility-only command is the thing §5 forbids.
    ///
    /// `data` is whatever the request carried — a text range for
    /// [`Action::SetTextSelection`](accesskit::Action::SetTextSelection), a
    /// string for [`SetValue`](accesskit::Action::SetValue), a scroll hint. It
    /// is passed through rather than dropped because a range is the *whole* of
    /// what a selection request means, and a router that had to guess one would
    /// select text the person did not.
    fn accessibility_action(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _target: crate::accessibility::Occurrence,
        _action: accesskit::Action,
        _data: Option<&accesskit::ActionData>,
    ) -> bool {
        false
    }

    /// Draw into `scene` under `root`.
    ///
    /// `root` rather than a DPI factor because a view no longer necessarily owns
    /// the window: it carries the scale *and* wherever the view has been put, so
    /// a view hosted inside another one draws in its own coordinates and the
    /// transform does the moving. A view that fills its window is handed a plain
    /// `Affine::scale`.
    ///
    /// The scene arrives **already reset**, and may already have furniture in it.
    /// Drawing starts by painting your own background over your own extent, not
    /// by clearing.
    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    );

    /// The document this view is showing, if it is showing one. The shell asks
    /// so a second click on the same pill focuses the window that already has it
    /// open rather than making another.
    fn document_root(&self) -> Option<NodeId> {
        None
    }

    /// **The answer to [`PaneRequest::ChooseDelegateFolder`]** (ADR-365).
    ///
    /// Called only when somebody actually chose a folder; cancelling says
    /// nothing, because a cancelled picker changed nothing and a view that was
    /// told about it would have to decide not to react. `true` if the view
    /// changed and wants redrawing.
    fn delegate_folder_chosen(&mut self, _chosen: DelegateFolder) -> bool {
        false
    }

    /// **The answer to [`PaneRequest::ChooseRecordingPath`]** (SESSIONRECORD).
    /// Same contract as [`delegate_folder_chosen`](Self::delegate_folder_chosen):
    /// called only on a real choice, `true` when the view wants redrawing.
    fn recording_path_chosen(&mut self, _chosen: std::path::PathBuf) -> bool {
        false
    }

    /// **The answer to [`PaneRequest::ChooseAiChatFolder`]** (AGENTUX UC8).
    /// Called only on a real choice; `true` when the view wants redrawing.
    fn ai_chat_folder_chosen(&mut self, _chosen: std::path::PathBuf) -> bool {
        false
    }

    /// **A recording lifecycle event** (SESSIONRECORD): the shell's start and
    /// finalize workers answer through here. `true` when the view changed and
    /// wants redrawing. Default ignores them — a view that never records.
    fn recording_event(&mut self, _event: crate::RecordingEvent) -> bool {
        false
    }

    /// **The observations a stop takes with it** (SESSIONRECORD). Called by
    /// the shell as the stop is performed, so the finalize worker writes both
    /// artifacts — the one-writer rule. Default has nothing to hand over.
    fn take_recording_telemetry(&mut self) -> Option<koru_recording::SessionTelemetry> {
        None
    }

    /// Whether this view has `document` open as a tab, and whether that tab is pinned.
    fn tab_presence(&self, _document: koru_core::DocumentId) -> TabPresence {
        TabPresence::Absent
    }

    /// Unpin `document` if this view has it pinned.
    fn unpin_document(&mut self, _document: koru_core::DocumentId) -> bool {
        false
    }

    fn pinned_search_rows(
        &self,
    ) -> Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)> {
        Vec::new()
    }

    /// A document the view asked to open. Cleared by the asking.
    ///
    /// A view can only *request* this: performing it needs a new window, which
    /// is not a view's to make.
    ///
    /// **The node only.** A view may also have asked for a *new tab* or a *new
    /// window* rather than this one (Ctrl+Enter and Shift+Enter on a document
    /// pill), and a window with no tabs in it has nowhere to put that
    /// distinction — every disposition collapses to "a window here". The
    /// container reads the richer request through its own path, because it is
    /// the only side that owns a tab set. This crate does not depend on
    /// `koru-editor` and deliberately never learns that vocabulary.
    fn take_wants_open(&mut self) -> Option<NodeId> {
        None
    }

    /// After opening a URL without activating the browser, keep this window focused.
    fn take_wants_stay_focused(&mut self) -> bool {
        false
    }

    /// Insert `document` as a background tab. `ws` is that document's tree,
    /// already loaded without becoming the window's active document.
    /// `home` is the document's real workspace (from Store locate).
    fn open_background_tab(
        &mut self,
        _ws: &WorkspaceDoc,
        _document: koru_core::DocumentId,
        _home: Option<WorkspaceId>,
    ) -> bool {
        false
    }

    /// Open several documents as tabs in a **new** window (same process when
    /// they share this window's workspace).
    fn open_documents_in_new_window(
        &mut self,
        _ws: &WorkspaceDoc,
        _documents: &[koru_core::DocumentId],
    ) -> bool {
        false
    }

    /// A document the container asked to **pop out** into its own window.
    ///
    /// Distinct from [`take_wants_open`](Self::take_wants_open): that one a
    /// container drain *as a tab* in hand, whereas this asks the shell for a
    /// new OS window. Cleared by the asking. Only a container that owns tabs
    /// ever returns one.
    fn take_wants_popout(&mut self) -> Option<NodeId> {
        None
    }

    /// The document an in-flight pane drag is carrying, if there is one —
    /// the shell's question when the drag's pointer crosses into another
    /// window, where a pop-out is no longer the answer. Window-local tabs
    /// (sessions, chat) answer nothing, exactly as they never pop out.
    /// Default `None`: a view with no tab drags carries nothing.
    fn pane_drag_document(&self) -> Option<NodeId> {
        None
    }

    /// Preview where a tab dragged from **another** window would land at
    /// `(x, y)`: the lane slot or the edge split, the same answers this
    /// container's own drag gives. Answers whether the point is over a pane
    /// that can take the document — the shell needs that to know the drag has
    /// somewhere to go. Default `false`: not a tab host.
    fn foreign_tab_hover(&mut self, _ws: &WorkspaceDoc, _doc: NodeId, _x: f32, _y: f32) -> bool {
        false
    }

    /// Clear the preview [`foreign_tab_hover`](Self::foreign_tab_hover) set —
    /// the drag moved on or ended. Default no-op.
    fn foreign_tab_leave(&mut self) {}

    /// Land a tab dragged from another window at `(x, y)` — the drop the hover
    /// previewed. A refusal (`false`) sends the source window back to its
    /// pop-out answer. Default `false`: not a tab host.
    fn foreign_tab_drop(&mut self, _ws: &WorkspaceDoc, _doc: NodeId, _x: f32, _y: f32) -> bool {
        false
    }

    /// The source half of a landed [`foreign_tab_drop`](Self::foreign_tab_drop):
    /// the release happened over another window, so this container's own
    /// `mouse_up` never ran — take the dragged tab out of its strip and
    /// settle, without the pop-out an ordinary outside release would ask for.
    /// Default no-op.
    fn complete_foreign_drop(&mut self) {}

    /// Tell the source of a cross-window tab drag whether another window is
    /// currently accepting it, so its "release to pop out" marker only shows
    /// when a pop-out is what a release would do. Pushed on every move of the
    /// drag — the shell owns the geometry, the view owns the marker. Answers
    /// whether anything visible changed. Default `false`.
    fn set_foreign_targeted(&mut self, _targeted: bool) -> bool {
        false
    }

    /// A **session** the container asked to move to a new window (Q-E7 P8's
    /// pop-out). The shell drains it and duplicates a container seated on the
    /// session, the same route [`take_wants_popout`](Self::take_wants_popout)
    /// takes for a document. Cleared by the asking. Only a container that owns
    /// session tabs ever returns one; this crate never dials the session host.
    fn take_wants_session_popout(&mut self) -> Option<koru_session_protocol::SessionId> {
        None
    }

    /// Seat an existing `session` as a tab in this container — the receiving
    /// end of another window's *Move to new window*, and of the notification
    /// area's Agents rows. Default no-op: only a container that owns session
    /// tabs implements it.
    fn open_session_tab(&mut self, _session: koru_session_protocol::SessionId) -> bool {
        false
    }

    /// Whether this container is showing a tab for `session` right now — the
    /// routing fact behind [`AgentAsk::ShowSession`]: the pane already showing
    /// a session is the one asked to focus it, before any fallback. Default
    /// `false`: a view with no session tabs shows none.
    fn shows_session(&self, _session: koru_session_protocol::SessionId) -> bool {
        false
    }

    /// Open the container's new-tab chooser — the `+` menu — so the person
    /// can start a new agent session ([`AgentAsk::NewAgentSession`]). Which
    /// agent to start is their choice; nothing here picks one. Default no-op:
    /// only a container with a `+` menu implements it.
    fn open_new_session_chooser(&mut self) -> bool {
        false
    }

    /// A document a popped-out window asked to **restore as a tab** before it
    /// closes. Cleared by the asking. The shell routes it to a container pane
    /// via [`restore_tab`](Self::restore_tab).
    fn take_wants_restore_tab(&mut self) -> Option<NodeId> {
        None
    }

    /// Open `doc` as a tab in this container. Default no-op: only a container
    /// that owns tabs implements it. This is the receiving end of
    /// [`take_wants_restore_tab`](Self::take_wants_restore_tab).
    fn restore_tab(&mut self, _ws: &WorkspaceDoc, _doc: NodeId) {}

    /// The window was asked to close. Return `true` to **veto** the close — the
    /// view will close itself later by emitting [`WindowRequest::Close`] once it
    /// is ready. Default is `false`: close at once.
    ///
    /// A popped-out document window uses this to put up its close-choice dialog
    /// instead of disappearing the moment its ✕ is pressed.
    fn request_close(&mut self) -> bool {
        false
    }

    /// Whether this view is a tab container (the thing a popped-out document
    /// restores *into*). Default `false`. The shell asks so a "restore as tab"
    /// request reaches a container rather than another document window.
    fn is_container(&self) -> bool {
        false
    }

    /// A new remembered choice for what closing a popped-out window does, if the
    /// user asked to remember one this run. The shell writes it to `Settings`.
    /// Default `None`.
    fn take_popout_close_setting(&mut self) -> Option<PopoutClose> {
        None
    }

    /// The remembered close choice read from `Settings`, pushed by the shell
    /// when a popped-out window is opened so it can skip its dialog. Default
    /// no-op: only a document pane that owns a close dialog implements it.
    fn set_popout_close(&mut self, _choice: PopoutClose) {}

    /// The same for sharing, which belongs to the document rather than a view.
    fn take_wants_sharing_toggle(&mut self) -> bool {
        false
    }

    /// A window-level action the view asked for — see [`WindowRequest`].
    /// Drained inside the press itself, because dragging has to be.
    fn take_window_request(&mut self) -> Option<WindowRequest> {
        None
    }

    /// The document under this view was **replaced** — a workspace switch —
    /// rather than merely edited.
    ///
    /// **Required, with no default, on purpose.** Every `ChangeTracker` a view
    /// holds is a Loro subscription bound to one `LoroDoc`; after a swap they
    /// are all subscribed to a document nobody holds. A view that forgets to
    /// rebuild would render once and then go silent, with no error anywhere —
    /// so forgetting has to be a compile error. `adopt` is not enough for this
    /// and never will be: it *patches* what a subscription reported, and there
    /// is no subscription left to report anything.
    fn reseat(&mut self, ws: &WorkspaceDoc);

    /// Show the document the workspace has just made active, in a tab.
    ///
    /// The shell swaps which document is open; the *tab* is the view's, so it
    /// has to be told. Without this a created or clicked document became the
    /// active one with nothing on screen changing — the sidebar updated and the
    /// window did not.
    ///
    /// **`active` is which document `ws` now is, and it is a parameter rather
    /// than something the pane already knows.** It used to arrive separately,
    /// through [`set_entries`](Self::set_entries), *after* this call — so the
    /// container decided "is this document already in a tab?" against the
    /// **previous** one, matched the tab it was already in, selected that, and
    /// opened nothing. The first click on a document did nothing and the second
    /// worked, because by then the push had landed. Passing it here is what
    /// makes the order impossible to get wrong.
    fn show_active_document(
        &mut self,
        _ws: &koru_core::schema::WorkspaceDoc,
        _active: Option<koru_core::DocumentId>,
    ) -> bool {
        false
    }

    /// Tell the pane what the workspace holds.
    ///
    /// Separate from [`set_pane_context`](Self::set_pane_context) rather than a
    /// fifth argument on it: that one changes when a *profile* does, which is
    /// almost never, and this changes whenever a document is created, renamed or
    /// moved. Pushing both on the same call would rebuild the switcher's rows on
    /// every keystroke in a title.
    ///
    /// **And calling it only when that one changed is the same mistake wearing a
    /// different hat**, which is what the shell did until Explore stopped
    /// showing new documents until you switched workspace and back. The two are
    /// pushed independently because they change independently.
    ///
    /// `active` is **which of them is open**, and it comes with the list rather
    /// than being worked out from the view's own tab. A row in this list is keyed
    /// by `DocumentId`; a tab's root is a `NodeId` *inside* a document. Those are
    /// different uuids, so a view left to answer "which row am I on" from what it
    /// holds gets `None` for every row — which is exactly what happened, and it
    /// showed up as no row in Explore ever drawing as the current one.
    fn set_entries(
        &mut self,
        _entries: Vec<koru_core::explore::ExploreEntry>,
        _active: Option<koru_core::DocumentId>,
    ) -> bool {
        false
    }

    /// Replace a pending entity-picker query with the document pill whose id
    /// the Store has just minted. The shell owns the round trip; the view owns
    /// the authored range that must be replaced.
    fn complete_new_page_link(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _profile: ProfileId,
        _workspace: WorkspaceId,
        _document: koru_core::DocumentId,
        _title: String,
    ) -> bool {
        false
    }

    /// The destination write of a selection move succeeded — replace the
    /// selection with its trail pill. `node` is the heading the picker drilled
    /// into, when it did, so the pill deep-links to where the content went.
    /// The view answers `false` when the pending move is gone (Escape, a
    /// closed tab) — the content is then duplicated, never deleted.
    fn complete_selection_move(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _document: koru_core::DocumentId,
        _node: Option<koru_core::NodeId>,
        _title: String,
    ) -> bool {
        false
    }

    /// The destination write failed; drop the pending move so nothing later
    /// deletes a selection whose copy never landed.
    fn cancel_selection_move(&mut self) -> bool {
        false
    }

    /// What is in the workspace's trash.
    ///
    /// Pushed beside the library rather than derived from it: they are two
    /// halves of one walk and both come from the manifest, which a view holds
    /// none of. The Trash screen read the *document's* hidden `Kind::Trash` node
    /// before this — correct while a document was a node in the library tree,
    /// and empty ever since documents became manifest entries.
    fn set_trashed(&mut self, _trashed: Vec<koru_core::explore::ExploreEntry>) -> bool {
        false
    }

    /// Which of this workspace's files are listed here but held on another
    /// device.
    ///
    /// Pushed for the same reason the library is: answering it needs a Store
    /// read per file plus the local blob list, and a view holds neither. It is
    /// the complement of the listing rather than part of it — a file's reference
    /// replicates with the manifest while its bytes do not, so two devices
    /// showing the same list can disagree about every row here, and correctly.
    fn set_remote_files(&mut self, _remote: Vec<koru_core::DocumentId>) -> bool {
        false
    }

    /// One file's fetch started or finished, without a whole sweep behind it.
    fn set_file_fetching(&mut self, _file: koru_core::DocumentId, _fetching: bool) {}

    /// Feed a workspace's document listing into an open Move to… picker.
    fn set_move_to_workspace_entries(
        &mut self,
        _workspace: WorkspaceId,
        _entries: Vec<koru_core::explore::ExploreEntry>,
    ) -> bool {
        false
    }

    /// A cross-workspace transfer failed; drop optimistic undo/toast state.
    fn transfer_failed(&mut self) -> bool {
        false
    }

    /// Tell the view whether this window is reaching the store.
    ///
    /// `None` means it is. Pushed rather than asked for because a view holds no
    /// workspace — and pushed **every tick** rather than on a change, because
    /// what matters is partly *how long* it has been true.
    fn set_store_trouble(&mut self, _trouble: Option<koru_workspace::StoreTrouble>) -> bool {
        false
    }

    /// **Tell the view what the Store has confirmed, per open document.**
    ///
    /// Pushed for [`set_store_trouble`](Self::set_store_trouble)'s reason — a
    /// view holds no workspace — and a *second* hook rather than a field on
    /// that one because the two have different costs and different audiences.
    /// `set_store_trouble` speaks for the window, is free to compute, and runs
    /// on every tick of every window forever. This asks each open document for
    /// its version vector, so it runs **only while a screen reader is
    /// attached**: the caller is `service_accessibility`, behind the same
    /// `PaneAccessibility::listening` gate that keeps
    /// [`semantics_revision`](Self::semantics_revision) from being asked at
    /// all in an ordinary session.
    ///
    /// A consequence worth stating rather than discovering: a view learns
    /// about durability *from the moment a client attaches*, not before. The
    /// current state is therefore always answerable, and a transition that
    /// happened while nobody was listening is not news anybody missed.
    ///
    /// Default no-op: a view with no persistence projection ignores it.
    fn set_durability(&mut self, _durability: &koru_workspace::durability::Durability) {}

    /// Tell the view which profiles and workspaces it is showing, so the pane's
    /// popovers can list them.
    ///
    /// `profiles` is **every** profile the machine knows about — the registry's
    /// whole list, not just the open one. Passing only the open one is what made
    /// the profile switcher unable to switch profiles.
    ///
    /// `current` is `None` while the window is showing its local Welcome
    /// placeholder. That document has an internal UUID for rendering, but it is
    /// not a Store workspace and cannot own durable content.
    ///
    /// Default no-op: only a container draws a pane. Returns whether anything
    /// changed, so an unchanged call costs no frame (ADR-26).
    fn set_pane_context(
        &mut self,
        _profiles: Vec<koru_core::ProfileSummary>,
        _current_profile: ProfileId,
        _workspaces: Vec<koru_core::WorkspaceEntry>,
        _current: Option<WorkspaceId>,
    ) -> bool {
        false
    }
    /// Active local Workspace counts for every visible Profile. Produced by the
    /// existing off-thread Store catalog worker.
    fn set_profile_workspace_counts(
        &mut self,
        _counts: std::collections::HashMap<ProfileId, usize>,
    ) -> bool {
        false
    }
    /// Every Profile's workspaces, so the switcher can show a Profile that is
    /// not open. Produced by the same off-thread Store catalog worker.
    fn set_profile_workspaces(
        &mut self,
        _workspaces: std::collections::HashMap<ProfileId, Vec<koru_core::WorkspaceEntry>>,
    ) -> bool {
        false
    }
    /// Push the Agent's content-light account state. No view polls IPC itself.
    fn set_account_state(&mut self, _state: koru_agent_wire::AccountState) -> bool {
        false
    }

    /// Focus the Welcome tab after onboarding has durably created its Profile
    /// and Workspace. `owner` is the display name onboarding asked for — the
    /// greeting uses its first word — and `profile` is the Profile it names.
    fn show_first_minutes(&mut self, _owner: &str, _profile: &str, _workspace: &str) -> bool {
        false
    }

    /// Deliver one account-operation completion to the tab that can present it.
    fn account_result(
        &mut self,
        _generation: u64,
        _result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        false
    }

    fn account_scoped_result(
        &mut self,
        _owner_scope: u64,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        self.account_result(generation, result)
    }

    fn set_hidden_profile_count(&mut self, _count: usize) -> bool {
        false
    }
    /// Full retained inventory for exact owned completion, independent of the visible picker.
    fn set_retained_profiles(&mut self, _profiles: Vec<koru_core::ProfileSummary>) -> bool {
        false
    }

    fn account_command_failed(&mut self, _failure: account_ui::CommandFailure) -> bool {
        false
    }
    fn account_command_admitted(&mut self, _admission: account_ui::CommandAdmission) -> bool {
        false
    }

    fn local_profile_account_bound(&mut self, _profile: ProfileId) -> bool {
        false
    }

    /// Return the in-tab connected Profile form to an editable state after a
    /// failure before Store Profile acknowledgement.
    fn connected_profile_creation_failed(&mut self, _message: &str) -> bool {
        false
    }
    fn connected_profile_connection_paused(
        &mut self,
        _owner_scope: u64,
        _profile: ProfileId,
        _retry: koru_agent_wire::AccountCommand,
    ) -> bool {
        false
    }
    fn connected_profile_ready(&mut self, _owner_scope: u64, _profile: ProfileId) -> bool {
        false
    }

    fn local_profile_restored(&mut self, _profile: ProfileId) -> bool {
        false
    }

    fn local_profile_restore_failed(&mut self, _message: &str) -> bool {
        false
    }

    fn local_profile_account_bind_failed(&mut self, _profile: ProfileId, _code: &str) -> bool {
        false
    }

    fn local_profile_removed(&mut self, _profile: ProfileId) -> bool {
        false
    }

    fn local_profile_remove_failed(&mut self, _profile: ProfileId, _code: &str) -> bool {
        false
    }

    fn deleted_profile_continued_offline(&mut self, _profile: ProfileId) -> bool {
        false
    }

    fn deleted_profile_continue_offline_failed(&mut self, _profile: ProfileId) -> bool {
        false
    }

    /// Push the settings authority's machine-local startup facts into reminder state.
    fn set_reminder_startup(&mut self, _facts: koru_window::ReminderStartupFacts) -> bool {
        false
    }

    /// Complete one explicit first-reminder startup answer.
    fn reminder_startup_result(&mut self, _result: ReminderStartupResult) -> bool {
        false
    }

    /// Begin semantic snooze management after Agent routed its entity here.
    fn reminder_snooze(&mut self, _intent: ReminderSnoozeIntent) -> bool {
        false
    }

    /// What each workspace costs and where it is, for a view that lists them.
    ///
    /// Same contract as [`set_pane_context`]: default no-op, returns whether
    /// anything changed. Pushed **on demand**, never on a timer — measuring a
    /// folder is a directory walk, and anything put in the idle loop is paid for
    /// as long as the app is open.
    ///
    /// [`set_pane_context`]: View::set_pane_context
    fn set_workspace_stats(&mut self, _stats: Vec<WorkspaceStat>) -> bool {
        false
    }

    /// Put an error in front of the person at the keyboard.
    ///
    /// Same shape and same contract as [`set_pane_context`]: a default no-op,
    /// returning whether anything changed so an ignored call costs no frame.
    /// A view that cannot show one says so by not overriding this, and the shell
    /// falls back to a native dialog rather than to silence.
    ///
    /// [`set_pane_context`]: View::set_pane_context
    fn report_error(&mut self, _error: ErrorReport) -> bool {
        false
    }

    /// Show a transient non-error notice. Returns whether this view has a
    /// surface for it, so the shell may try another pane.
    fn show_notice(&mut self, _message: String) -> bool {
        false
    }

    /// A notice with an **Open another Profile** action. Views without a
    /// Profile switcher still surface the message without inventing another
    /// navigation mechanism.
    fn show_notice_open_another_profile(&mut self, message: String) -> bool {
        self.show_notice(message)
    }

    /// A notice with a **Show** action that reveals `path` in the OS file
    /// manager. Default falls back to a plain notice (no action) so a view that
    /// only implements [`show_notice`] still surfaces the message.
    ///
    /// [`show_notice`]: View::show_notice
    fn show_notice_reveal(&mut self, message: String, path: PathBuf) -> bool {
        let _ = path;
        self.show_notice(message)
    }

    /// Start a persistent content-import notice. An indeterminate notice asks
    /// for animated feedback; a determinate one carries a zero-through-one
    /// fraction. Closing the notice does not cancel the import.
    fn show_import_progress(&mut self, message: String, _progress: ImportNoticeProgress) -> bool {
        self.show_notice(message)
    }

    /// Update a content-import notice if it is still visible. A view that has
    /// already let the person close it consumes the update without recreating
    /// the surface.
    fn update_import_progress(
        &mut self,
        _message: String,
        _progress: ImportNoticeProgress,
    ) -> bool {
        false
    }

    /// The running import accepted a Stop request. Keep its persistent progress
    /// surface, replace the stage copy, and remove the action so it cannot be
    /// requested twice.
    fn cancel_import_progress(&mut self, message: String) -> bool {
        self.update_import_progress(message, ImportNoticeProgress::Indeterminate)
    }

    /// Remove a running-import notice without disturbing a different toast.
    fn clear_import_progress(&mut self) -> bool {
        false
    }

    /// A completed content import with an action that lands on the root result.
    fn show_import_notice(&mut self, message: String, _target: ImportNoticeTarget) -> bool {
        self.show_notice(message)
    }

    /// **Ask whether voice input may be set up**, naming the download size and
    /// the model's licence.
    ///
    /// A modal question rather than a notice, because a person has to answer
    /// it: nothing is downloaded and no microphone is opened until they do.
    /// The default here is `false` and that is the *safe* default — a view with
    /// no consent step cannot agree to anything, and the shell reads `false` as
    /// "nobody was asked" rather than as "go ahead".
    fn show_voice_consent(&mut self, _offer: voice_install::VoiceOffer) -> bool {
        false
    }

    /// Start the persistent progress surface for a model download. `None` is an
    /// indeterminate bar; `Some(fraction)` is zero through one.
    ///
    /// Its action is **Stop**, which requests cooperative cancellation. Closing
    /// the surface only hides progress and never changes the download — DESIGN
    /// is explicit about that, and it is the difference between the × and the
    /// action beside the bar.
    fn show_voice_download(&mut self, message: String, _fraction: Option<f32>) -> bool {
        self.show_notice(message)
    }

    /// Update it if it is still visible. A view that has already let somebody
    /// close it consumes the update without recreating the surface.
    fn update_voice_download(&mut self, _message: String, _fraction: Option<f32>) -> bool {
        false
    }

    /// The download accepted a Stop. Keep the surface, replace the stage copy,
    /// and remove the action so it cannot be requested twice.
    fn cancel_voice_download(&mut self, message: String) -> bool {
        self.update_voice_download(message, None)
    }

    /// Remove a model-download surface without disturbing a different toast.
    fn clear_voice_download(&mut self) -> bool {
        false
    }

    /// Put the read-only legacy workspace preview in front of the user.
    ///
    /// The shell owns the prepared conversion and performs the eventual Store
    /// call; a view owns modal layout and input. The answer returns through
    /// [`PaneRequest::FinishWorkspaceImport`], keeping filesystem and Store
    /// work out of the view.
    fn show_import_preview(&mut self, _preview: ImportPreview) -> bool {
        false
    }

    /// Something the pane's workspace/account switcher asked for — see
    /// [`PaneRequest`]. Cleared by the asking.
    ///
    /// Every one of these ends in a document being swapped, a folder picker, or
    /// a new process, and none of those is a view's to perform: a view holds no
    /// data directory, no profile document and no `Window`.
    fn take_pane_request(&mut self) -> Option<PaneRequest> {
        None
    }

    /// The shell could not create the document this view asked for
    /// ([`PaneRequest::CreateDocument`]). Default: nothing — most views hold
    /// no state about the ask. The container clears its promotion latch here,
    /// so the next keystroke in a draft asks again rather than typing forever
    /// into a tab that can never become real: without this, one transient
    /// store failure left a draft permanently un-promotable and silent.
    fn create_document_failed(&mut self) {}

    /// A node the user wants to become an image (the `/image` slash command).
    /// The shell opens the file picker and calls [`insert_image`](Self::insert_image)
    /// with the resulting blob — both need disk, which a view does not touch.
    fn take_wants_image(&mut self) -> Option<NodeId> {
        None
    }

    /// A row whose `/attach` command should become a routed file pill.
    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        None
    }

    /// **`/dictate` was chosen: begin a dictation in this window.**
    ///
    /// The slash command's whole seam. It answers the same thing the chord
    /// does — the shell asks the one [`koru_dictation::Dictation`] this process
    /// has and routes `Begin` back through [`dictate`](Self::dictate) — so the
    /// command teaches the shortcut rather than being a second way to do it.
    ///
    /// A `bool` rather than a node: the controller's `Begin` is routed to *the
    /// focused editable surface*, which is the answer a view already computes
    /// for a keystroke, so there is nothing here for it to name.
    fn take_wants_dictation(&mut self) -> bool {
        false
    }

    /// Complete `/attach` after the shell has imported the selected file(s).
    /// First file replaces the trigger row; each further file becomes a new
    /// sibling row immediately below.
    /// A tag to create in the workspace tag table and then apply to these
    /// nodes — the tag chooser's "Create tag" row. The table lives in the
    /// workspace **system document** (ADR-176), which a view cannot see, so
    /// the shell creates the tag through `Workspace::create_tag` and applies
    /// the fresh id to the nodes in the active document.
    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        None
    }

    /// "Add to workspace dictionary": `(language tag, word)`. Only the shell
    /// can reach the workspace's system document, so the write travels the
    /// same road tag creation does; the shell refreshes the spelling overlay
    /// after it lands.
    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        None
    }

    /// The workspace tag table, resolved for display, pushed whenever it
    /// changes — a tag created here, or a peer's create/rename/merge arriving
    /// through the system document's room. Returns whether anything on screen
    /// changed. Default no-op: only a view that draws attribute chips cares.
    fn set_tag_table(&mut self, _tags: &[koru_core::system::TagEntry]) -> bool {
        false
    }

    /// A list to create in the workspace **list table** — the tasks rail's
    /// inline "+ New list" field, committed.
    ///
    /// The tag request's counterpart, and for the identical reason: the table
    /// lives in the system document (ADR-176), a view cannot see one, and the
    /// name has already been typed by the time this is asked. The colour is an
    /// `AccentColor` token from the shared swatch set, never an RGB value.
    fn take_wants_create_list(&mut self) -> Option<(String, Option<String>)> {
        None
    }

    /// A list to create **and put these nodes in** — `/task_list`'s *New
    /// list* row, after its colour was chosen. Both halves are the host's:
    /// the table is the system document's, and the join is the same one
    /// step a chosen list is (the row made a task first, then the
    /// membership), which the editor could not make without the id.
    fn take_wants_join_new_list(&mut self) -> Option<(Vec<NodeId>, String, Option<String>)> {
        None
    }

    /// **Changes to lists that already exist** — renamed, recoloured, deleted.
    ///
    /// [`take_wants_create_list`](Self::take_wants_create_list)'s counterpart,
    /// through the same seam and for the same reason: the list table is in the
    /// system document and a view cannot see one. A `Vec` rather than an
    /// `Option` because a rename and a recolour can both have happened between
    /// two turns, and dropping either would be a lost gesture.
    ///
    /// A delete arrives here **already confirmed** — the dialog is the view's,
    /// and the shell performs what it is handed.
    fn take_wants_list_edits(&mut self) -> Vec<tasks::ListEdit> {
        Vec::new()
    }

    /// **Task changes a view asked for but cannot make**, in the order it asked.
    ///
    /// [`take_wants_create_list`](Self::take_wants_create_list)'s shape, for the
    /// same reason one layer down: a Tasks surface lists tasks from every
    /// document in the workspace, so ticking a row is a write into a document
    /// nobody has open — which needs a `&mut Workspace` to park it, join its
    /// room and publish, and a view holds one `&mut WorkspaceDoc` at most.
    ///
    /// A `Vec` rather than an `Option`, because a capture and a tick can both
    /// have happened between two turns and dropping either would be a lost
    /// gesture.
    fn take_wants_task_commands(&mut self) -> Vec<tasks::TaskCommand> {
        Vec::new()
    }

    /// **Pills a delegation asked to leave behind**, in the order it asked.
    ///
    /// [`take_wants_task_commands`](Self::take_wants_task_commands)'s shape and
    /// for its reason: the document a delegation came from is the one the
    /// *gesture* named, which is not necessarily the one the view is showing by
    /// the time the run id comes back, and reaching it needs a `&mut Workspace`
    /// to park it and publish.
    ///
    /// A `Vec`, because two delegations can be accepted between two turns.
    fn take_wants_run_pills(&mut self) -> Vec<runpill::RunPill> {
        Vec::new()
    }

    /// Schedule references accepted from a document-origin Repeat gesture.
    /// They travel separately from Run pills because the two targets have
    /// different identities and open different product surfaces.
    fn take_wants_schedule_pills(&mut self) -> Vec<schedulepill::SchedulePill> {
        Vec::new()
    }

    /// **A row or a chip asked to be shown on the Tasks page.**
    ///
    /// The node alone: a view knows which document it is showing and the
    /// container knows which tab is over which document, so carrying the pair
    /// would be one of them stating the other's fact. Default `None`: only a
    /// document surface ever raises one.
    fn take_wants_task_view(&mut self) -> Option<koru_core::NodeId> {
        None
    }

    /// One of those commands refused, with a sentence for a person.
    ///
    /// **The return channel exists because of `Create`.** A capture that failed
    /// has taken somebody's typing with it, and losing what was typed is the one
    /// unrecoverable bug — so the view is handed the command back and can put
    /// the text where it was. Answers whether anything on screen changed.
    fn task_command_failed(&mut self, _command: &tasks::TaskCommand, _error: &str) -> bool {
        false
    }

    /// **A task command the shell ran, and what it landed as.**
    ///
    /// The other half of the return channel, and the one every successful
    /// command takes: the receipt names the document written and its version
    /// the moment the write landed, so a Tasks surface can ask the read model
    /// for an answer that includes it rather than draw the first one back. It
    /// arrives **after** the write, which is what lets a surface's *ask again*
    /// be caused by the write rather than by the intention to make one.
    /// Answers whether anything on screen changed. Default no-op.
    fn task_command_landed(&mut self, _landed: &tasks::Landed) -> bool {
        false
    }

    /// The workspace list table, pushed whenever it changes — a list made
    /// here, or a peer's arriving through the system document's room. Answers
    /// whether anything on screen changed. Default no-op: only a view that
    /// draws the tasks rail cares.
    fn set_list_table(&mut self, _lists: &[koru_core::system::ListEntry]) -> bool {
        false
    }

    /// **Which workspace this view is showing**, pushed beside the tag table
    /// and for the same reason: a document's own `LoroDoc` carries no
    /// workspace id — the store hands out an empty one — so only the host can
    /// say. Answers whether anything changed.
    ///
    /// The clipboard is what needs it. A **custom** attribute means what the
    /// workspace's system document says it means, so a copy records the
    /// workspace it left and a paste compares
    /// ([ADR-212](../../../docs/decisions/ADR-201-225.md#adr-212-blockcopy--copying-a-block-copies-the-blocks-attributes-and-a-tag-crosses-a-workspace-by-name)). Default
    /// no-op: only a view that pastes blocks cares.
    fn set_workspace(&mut self, _workspace: Option<koru_core::WorkspaceId>) -> bool {
        false
    }

    /// Complete `/attach` after the shell has imported the selected file.
    fn insert_attachment(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _node: NodeId,
        _files: &[(koru_core::DocumentId, String)],
    ) {
    }

    /// The file picker for `/attach` was cancelled — clear the trigger text.
    fn attachment_cancelled(&mut self, _ws: &mut WorkspaceDoc, _node: NodeId) {}

    /// An attachment pill the user activated (open / reveal / save).
    fn take_wants_file_action(&mut self) -> Option<(koru_core::DocumentId, FileAction)> {
        None
    }

    /// *Save as…* asked on a drawn diagram, if it was: the drawing the view
    /// shows and the page's title, for the shell to take through the dialog,
    /// the raster and the write. `ws` is where the title is read; the view
    /// does not keep one unless it draws its own caption.
    fn take_wants_save_diagram_as(&mut self, _ws: &WorkspaceDoc) -> Option<SaveDiagram> {
        None
    }

    /// *Copy image* asked on a drawn diagram, if it was: the drawing the
    /// view shows, for the shell to rasterise and put on the clipboard — the
    /// shell owns both, as it does for an image block's copy.
    fn take_wants_copy_diagram(&mut self) -> Option<CopyDiagram> {
        None
    }

    /// Blob hash of an image the user copied or cut. The shell owns the decoded
    /// bytes and the system clipboard, so it performs the copy.
    fn take_wants_copy_image(&mut self) -> Option<String> {
        None
    }

    /// Finish an image cut after the shell wrote the bitmap (delete the row).
    fn apply_pending_image_cut(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        false
    }

    /// A CF_HTML payload the most recent text/row copy armed, for the shell to
    /// write as "HTML Format" so a paste into Word or Google Docs keeps the
    /// structure. Default is `None`; a view whose editor builds the payload
    /// forwards it.
    /// `(cf_html_with_placeholders, plain_text)` for one multi-format write.
    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        None
    }

    /// A non-text clipboard the user pasted (Ctrl+V). Holds the captured block
    /// target. Cleared by the shell, which reads the clipboard, decodes, stores
    /// the bytes, and calls [`paste_images`].
    ///
    /// [`paste_images`]: Self::paste_images
    fn take_wants_paste_image(&mut self) -> Option<PasteTarget> {
        None
    }

    /// Turn an existing node into an image referencing blob `hash`. Default is a
    /// no-op: only a view that can show images implements it.
    fn insert_image(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _node: NodeId,
        _hash: &str,
        _mime: &str,
        _width: Option<u32>,
        _height: Option<u32>,
    ) {
    }

    /// Append a brand-new image node (a file dropped on the window). Default is
    /// a no-op; a document appends under its root.
    fn append_image(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _hash: &str,
        _mime: &str,
        _width: Option<u32>,
        _height: Option<u32>,
    ) {
    }

    /// Insert one or more pasted images at `target` (or append when it is
    /// `None`). Called once with every decoded image so a
    /// multi-image paste costs a single `rebuild` and `reflow`, and a single
    /// undo step. Default is a no-op; only a view that shows images implements it.
    fn paste_images(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _target: Option<PasteTarget>,
        _images: &[PastedImage],
    ) {
    }

    /// A foreign-text paste in a document that may carry images (a Word copy).
    /// Holds the captured block target **and the foreign text itself**:
    /// the editor defers the whole paste to the shell (so it can interleave any
    /// RTF pictures with the text), and passes the text along rather than making
    /// the shell re-read a clipboard it just touched with `clipboard-win` (that
    /// re-read races and can come back empty, silently dropping plain text).
    /// Cleared by the shell, which reads the RTF and either calls `paste_rich`
    /// (text + images interleaved) or `paste_text` (the carried text, when there
    /// are no pictures). Default is `None` — only a document view arms it.
    fn take_wants_paste_rich(&mut self) -> Option<(PasteTarget, String)> {
        None
    }

    /// Paste plain text the shell reached via the carried request (a rich paste
    /// that turned out to carry no pictures). The document view implements it as
    /// a call back into the editor's plain-text paste.
    fn paste_text(&mut self, _ws: &mut WorkspaceDoc, _text: &str) {}

    /// Paste mixed text/image blocks in document order at `target` (or append
    /// when it is `None`). Called once with every decoded piece so the whole
    /// paste costs a single `rebuild` and `reflow`, and a single undo step.
    /// Default is a no-op; only a view that shows images implements it.
    fn paste_rich(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _target: Option<PasteTarget>,
        _blocks: &[RichBlock],
    ) {
    }

    /// Adopt what the last run remembered, and hand back what this one should.
    ///
    /// Two hooks for what would otherwise force this crate to know what a
    /// completed task is: `hide_completed` is a toggle only an outline has, and
    /// `Settings` is the only thing that outlives the process.
    ///
    /// `restore` takes the document because reopening a tab means *building* a
    /// view of a node, and a view cannot be built without one. It runs before
    /// any window exists, so anything it opens should come back unlaid-out and
    /// reflow when it is first shown — restoring ten tabs must not cost ten
    /// layouts on the startup path.
    /// Reopen what was left open.
    ///
    /// **The whole `Workspace`, not one document.** A tab is over a document and
    /// a window has several, so rebuilding them needs every open document rather
    /// than the active one — the shell opens the remembered ones before calling
    /// this, and `Workspace::document` hands each over.
    fn restore(&mut self, _ws: &Workspace, _settings: &Settings) {}

    /// **A preference changed somewhere else, and this view keeps a copy.**
    ///
    /// Most views need nothing here: the theme, the fonts and the caret are
    /// process globals, so an invalidate is the whole of the update. This is
    /// for the handful of values a view holds a *field* of — the settings
    /// surface's default view mode, canvas colour and pinned-tab title — which
    /// the next [`capture`](Self::capture) would otherwise write straight back
    /// over the change, and in two windows would ping-pong forever.
    ///
    /// Deliberately **not** [`restore`](Self::restore): that reopens the
    /// session, and a settings change is not a reason to rebuild a tab strip.
    fn adopt_preferences(&mut self, _settings: &Settings) {}

    /// **The authority answered for the first time.** [`restore`](Self::restore)
    /// ran before it could — the link is asynchronous and the profile file no
    /// longer carries the preferences — so every default it resolved against
    /// was the registry's, not the person's. A document that follows the
    /// default page width was therefore restored at `Full` whatever the
    /// authority holds, and the next `capture` would have written that `Full`
    /// back as an override of its own, pinning the document there for good.
    ///
    /// Called once, in place of [`adopt_preferences`](Self::adopt_preferences),
    /// when the first section arrives. A view that restored nothing against a
    /// default needs only the ordinary adoption, which is what this does unless
    /// overridden. Later answers are genuine changes and take the ordinary path:
    /// an open document keeps its width when the default moves, by design.
    fn adopt_startup_preferences(&mut self, settings: &Settings) {
        self.adopt_preferences(settings);
    }

    /// Live workspace switch: refresh Explore / collapsed state without
    /// rebuilding the open tab strip. Cold launch still uses [`restore`].
    fn on_workspace_switched(&mut self, _ws: &Workspace, _settings: &Settings) {}
    fn capture(&self, _settings: &mut Settings) {}

    /// Whether this view has motion in flight and needs another frame.
    ///
    /// The loop's contract is that whoever changes state asks for the frame, so
    /// waking on a timer is deliberately not a reason to draw. An animation is
    /// the one kind of state that changes on a clock rather than on input, so it
    /// is the one thing that has to keep saying so.
    ///
    /// **It must return to `false`**, or the loop spins at frame rate forever.
    /// Drive it from a deadline rather than a per-frame decrement, so a dropped
    /// frame cannot leave it stuck on.
    fn animating(&self) -> bool {
        false
    }

    /// The host's side of the native-surface handshake, handed over once the
    /// pane's window exists and before the view can create a child of it.
    /// A view that never hosts a native surface ignores it.
    fn attach_surface_host(&mut self, _host: native_surface::SurfaceHost) {}

    /// The native child surface the host should place now, or `None`:
    /// **nothing for the host to touch** — either this view owns no surface,
    /// or it has hidden its own (a view can always hide an object it owns;
    /// the host is never needed to hide something it cannot see).
    fn native_surface(
        &mut self,
    ) -> Option<(native_surface::SharedSurface, native_surface::Placement)> {
        None
    }

    /// A placement or hide call on the view's native surface failed; the
    /// view decides what that means for its own state.
    fn native_surface_failed(&mut self, _error: native_surface::SurfaceError) {}

    /// Something this pane wants done to itself off the input path — see
    /// [`native_surface::BackgroundIntent`]. Drained by the host in both loop
    /// paths, so it reaches a hidden window too.
    fn take_background_intent(&mut self) -> Option<native_surface::BackgroundIntent> {
        None
    }

    /// An auxiliary window — the support chat, its floating icon — is not a
    /// document window: closing it never asks the last-window question or
    /// the unsent-work guard, and it does not count as an open window for
    /// either. The generalisation of `is_global_search`.
    fn is_auxiliary(&self) -> bool {
        false
    }

    /// The singleton support window, for find-or-open.
    fn is_support_window(&self) -> bool {
        false
    }

    /// The size this view's window should open at, when it is not the
    /// remembered main-window size. Logical pixels: (width, height) and the
    /// minimum.
    fn preferred_window_size(&self) -> Option<((f32, f32), (f32, f32))> {
        None
    }

    /// A small always-on-top window with no chrome that never takes focus
    /// when shown and is not resizable — the floating support icon. Its
    /// position is remembered as the floating position.
    fn floating_window(&self) -> bool {
        false
    }

    /// A window whose width is fixed at its preferred size and which resizes
    /// in height only -- the support chat, which is the Messenger's own
    /// width the way the web draws it. The host pins the maximum width and
    /// offers only the top and bottom edges to resize.
    fn fixed_width(&self) -> bool {
        false
    }

    /// Work that must continue while the application is hidden in the tray:
    /// none for a document; the support chat's receiver and deadline; the
    /// icon's visibility from the tray table. `app_hidden` is the host's
    /// state, passed in so the view can notice the transition.
    fn background_deadline(&self) -> Option<std::time::Instant> {
        None
    }
    fn background_maintain(&mut self, _now: std::time::Instant, _app_hidden: bool) -> bool {
        false
    }

    /// Whether `show_from_tray` should show this window. A chat hidden by a
    /// close choice and an icon hidden because the chat was opened both
    /// answer `false`.
    fn visible_after_restore(&self) -> bool {
        true
    }

    /// The host showed this window (Contact support on an existing one, or a
    /// restore from the tray). A view whose state depends on being on screen
    /// — the chat's badge and baseline — updates here.
    fn window_shown(&mut self) {}

    /// Support replied: show the notice in this window if it can. `false`
    /// means it cannot; the host offers it to no other window (D18: the
    /// active window, or nothing).
    fn show_support_reply(&mut self) -> bool {
        false
    }

    /// A native popup row was chosen; `id` is the row's id as a string. A
    /// view that showed a popup recognises its own rows and ignores the rest.
    fn popup_chosen(&mut self, _id: &str) -> bool {
        false
    }

    /// A low-frequency background deadline owned by the view. Unlike an
    /// animation this does not imply a frame; it lets debounced derived work
    /// (such as local search indexing) share the event loop's sleep precisely.
    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// Perform due background work. Return true only when visible view state
    /// changed and therefore needs a redraw.
    fn maintain(&mut self, _ws: &mut WorkspaceDoc, _now: std::time::Instant) -> bool {
        false
    }

    /// Move `delta` tabs from the active one, wrapping. Returns whether anything
    /// changed.
    ///
    /// The shell resolves this from `physical_key` and hands down a direction,
    /// rather than the view reading a chord, because **Ctrl+Tab and Ctrl+I are
    /// the same logical key on Windows**: Ctrl+I arrives as `NamedKey::Tab`, and
    /// `keymap::ctrl_letter` maps it back to `'i'` so italic works. `View::key`
    /// never sees a `KeyEvent` — on purpose, since one cannot be constructed in
    /// a test — so `physical_key` is only reachable here. This keeps the one
    /// untestable line in the shell and leaves the behaviour a plain method.
    fn cycle_tab(&mut self, _delta: i32) -> bool {
        false
    }

    /// **Do this to the focused editable surface.** Answers whether anything
    /// changed.
    ///
    /// The whole of dictation's seam into a view. The controller
    /// ([`koru_dictation::Dictation`]) decides *what*; a view decides *where*,
    /// by asking [`routing::Blockers`] and then [`routing::EditableTarget`] —
    /// the same two answers the key path uses, so a dictation cannot land
    /// behind a modal or in the document under a prompt.
    ///
    /// **Deliberately not named `begin_composition`.** `Leaf` implements both
    /// [`View`] and `koru_editor::Editing`, and a method name on both resolves
    /// by whichever trait is *imported at the call site* — silently, with every
    /// structural test failing and nothing saying why. `View::dictate` and
    /// `Editing::compose_*`/`*_dictation` are two vocabularies on purpose. The
    /// `false` default follows [`context_menu`](Self::context_menu): a view
    /// with nothing to dictate into says so by not implementing this.
    ///
    /// A `false` answer to [`DictationAction::Begin`] is a real answer and a
    /// common one — a masked credential field, a read-only document, a welcome
    /// screen — and the controller ends the session on it.
    fn dictate(
        &mut self,
        _ws: &mut WorkspaceDoc,
        _action: &koru_dictation::DictationAction,
    ) -> bool {
        false
    }

    /// **What the listening surface should show**, pushed once per pass of the
    /// loop, and `None` the pass a session ends.
    ///
    /// The counterpart to [`dictate`](Self::dictate): that one says what to do
    /// to the *text*, this one says what to draw *about* the gesture — a
    /// stage, the worker's state and a level bucket, and deliberately not a
    /// transcript ([`koru_dictation::Status`]).
    ///
    /// **The `None` is the whole lifetime rule.** A view keeps its indicator
    /// exactly while this is `Some`, so `animating`/`maintenance_deadline`
    /// return to their idle answers the frame the session ends rather than
    /// whenever somebody remembers to clear it. Answers whether anything
    /// changed, so the shell can ask for a redraw and no more often.
    fn dictation_status(&mut self, _status: Option<koru_dictation::Status>) -> bool {
        false
    }

    /// **Whether this press landed on the listening surface** — the one click
    /// that must not discard an open dictation.
    ///
    /// Every other click is an ordinary local event and throws the utterance
    /// away, which is the settled rule. The surface's own microphone button is
    /// the exception, because it *ends* the dictation: discarding there would
    /// throw away the sentence the button exists to keep. In window
    /// coordinates, the same space [`mouse_down`](Self::mouse_down) takes.
    fn dictation_hit(&self, _x: f32, _y: f32) -> bool {
        false
    }

    /// **Say why a dictation will not run**, once.
    ///
    /// A [`koru_dictation::Refusal`] is a code and a flag, never a sentence:
    /// `koru-voice` holds no translated string at all, so the view looks the
    /// code up in `koru-i18n` and says it wherever it says everything else
    /// that has already happened. Answers whether it was shown.
    fn dictation_refused(&mut self, _refusal: koru_dictation::Refusal) -> bool {
        false
    }

    /// **What is installed has changed** — a model finished downloading, or one
    /// was deleted. Answers whether anything redrew.
    ///
    /// Installed state is derived from the verified cache rather than stored
    /// ([`voice_install::installed_models`]), so a surface that shows it holds
    /// a *reading* rather than a fact and has to be told when to take another.
    /// This is that telling, and it is a push rather than a poll because the
    /// alternative is a directory walk on the frame loop.
    fn voice_models_changed(&mut self) -> bool {
        false
    }

    /// Open the **Move to…** picker. Resolved by the shell from the physical
    /// `KeyM` key — on Windows Ctrl+M is the same ASCII byte as Enter (`0x0D`),
    /// so a logical-key chord would steal Ctrl+Enter. Same shape as
    /// [`cycle_tab`](Self::cycle_tab).
    fn open_move_to(&mut self) -> bool {
        false
    }

    /// Step `delta` places through the view's own back/forward history — `-1` is
    /// back, `+1` is forward. Returns whether anything moved.
    ///
    /// A *direction*, resolved by the shell, for the same reason
    /// [`cycle_tab`](Self::cycle_tab) is: the two inputs that mean it are
    /// Alt+Arrow and the mouse's thumb buttons, and `MouseButton` never reaches
    /// a view — `mouse_down` carries only a `ctrl` flag, the way
    /// [`context_menu`](Self::context_menu) explains. A default no-op keeps
    /// every view that has no history from having to say so.
    /// The document is taken because restoring a place means building the view
    /// over it, exactly as [`context_menu`](Self::context_menu) does — and
    /// unlike `take_wants_open`, which has none and is therefore drained later
    /// by a handler that does.
    fn navigate_history(&mut self, _ws: &WorkspaceDoc, _delta: i32) -> bool {
        false
    }
}

/// Something the pane's switcher asked for that only the shell can do.
///
/// A view holds no data directory, no profile document and no `Window`, so
/// every one of these — swapping the open document, picking a folder, starting
/// a process — is asked for rather than performed. The same contract
/// [`WindowRequest`] has.
///
/// **`Clone`, not `Copy`.** [`RevealPath`] carries a path, because the folder
/// worth showing after a failed delete is one no id names any more.
///
/// [`RevealPath`]: PaneRequest::RevealPath
/// What the user asked to do with a file from an attachment pill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileAction {
    Open,
    Reveal,
    Save,
    /// Put a trashed file back where it came from.
    Restore,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ImportSourceSelection {
    /// Open the native multi-file picker for this format — or, for a remote
    /// format like Google Docs, let the importer run its own selection flow
    /// (OAuth + browser Picker) with no dialog at all.
    Picker,
    /// Paths supplied by an OS drop; no second dialog is needed.
    Files(Vec<PathBuf>),
}

/// Where the action on a completed-import notice should take the user.
///
/// Markdown and TextBundle sources become openable Koru documents, while an
/// opaque file belongs in the Files projection. OneNote is distinguished only
/// for the action's user-facing **View Notebook** label.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ImportNoticeProgress {
    Indeterminate,
    Determinate(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportNoticeTarget {
    Notebook(koru_core::DocumentId),
    Document(koru_core::DocumentId),
    File(koru_core::DocumentId),
}

/// Stable navigation identity. Document locations are deliberately absent;
/// the shell resolves them through Store immediately before opening.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavigationTarget {
    Document {
        document: koru_core::DocumentId,
        node: Option<NodeId>,
    },
    Workspace {
        profile: ProfileId,
        workspace: WorkspaceId,
    },
}

/// What a selection amounts to when it leaves its document.
///
/// Captured by the view at request time in koru-core terms, because this
/// crate does not link the editor. `Rows` names the selection's roots (title
/// excluded, none nested — `selection_roots`' contract); `Inline` carries the
/// part of one row that was selected, already sliced to runs.
#[derive(Clone, PartialEq, Debug)]
pub enum MoveSelectionPayload {
    Rows(Vec<koru_core::NodeId>),
    Inline {
        runs: Vec<koru_core::TextRun>,
        attrs: Vec<koru_core::node::Attr>,
    },
}

// `PaneRequest` derives `Eq` and a `TextRun` carries an `Option<f32>` size, so
// the derive is unavailable. Total equality holds for every size the editor
// writes (a font size is never NaN), which is what this asserts.
impl Eq for MoveSelectionPayload {}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConnectedProfileDraft {
    pub owner_display_name: String,
    pub profile_name: String,
    pub workspace_name: String,
    pub template: String,
    pub workspace_icon: Option<String>,
    pub workspace_color: Option<String>,
    pub profile_icon: Option<String>,
    pub profile_color: Option<String>,
    /// Whether the completed local Profile should also be created in the
    /// signed-in Koru account. False is the explicit offline path.
    pub connect_account: bool,
}

/// **What a folder picker for a delegated run's base answered** (ADR-365).
///
/// Two states and not three: a cancelled picker is not an answer, and the
/// shell simply does not call. `NotWritable` is a folder that exists and was
/// chosen and that Koru cannot write into -- said now, while somebody is
/// looking at the picker, rather than in ten minutes when a run refuses to
/// start in a place they no longer remember choosing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DelegateFolder {
    Chosen(std::path::PathBuf),
    NotWritable,
}

/// **What the shell says about a session recording** (SESSIONRECORD).
///
/// The view's lifecycle moves on these and nothing else: the start worker's
/// answer (the first-frame origin, or the code the start failed with) and the
/// finalize worker's (the clip is on disk). The origin is the encoder's
/// first-frame `Instant` — same process, same clock as the view's
/// observations, so `now - origin` stamps a telemetry row without drift.
#[derive(Clone, Debug)]
pub enum RecordingEvent {
    /// The capture is live and producing encoded frames. `origin` is the
    /// encoder's first-frame instant; `dpi_scale_milli` and `size` are the
    /// capture's own geometry, so the view's observations land in the clip's
    /// physical pixels (view-logical × zoom × scale).
    Started {
        origin: std::time::Instant,
        dpi_scale_milli: u32,
        size: (u32, u32),
    },
    /// The start (or, mid-clip, the finalize) failed, with the capture
    /// contract's code. The window is already unlocked and restored.
    Failed { code: &'static str },
    /// The clip is finalized on disk and the window is back the way it was.
    Stopped { clip_path: std::path::PathBuf },
}

impl RecordingEvent {
    /// The [`RecordingEvent::Failed`] code for "another window of this
    /// process holds the one recording" — a refusal, not a capture failure:
    /// the asking window is already unlocked and can try again once the
    /// other finishes. Named so the view can give it a sentence of its own
    /// instead of showing the raw code.
    pub const ALREADY_RECORDING: &'static str = "already-recording";
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PaneRequest {
    /// Send an account operation to Agent; no IPC or browser work runs in a view.
    Account(koru_agent_wire::AccountCommand),
    AccountOwned {
        owner_scope: u64,
        command: koru_agent_wire::AccountCommand,
    },
    AccountTracked {
        submission: account_ui::CommandSubmissionId,
        owner_scope: u64,
        command: koru_agent_wire::AccountCommand,
    },
    /// Create a new local Profile and first Workspace off-loop, then optionally
    /// connect that exact Profile only after hosted and local acknowledgements.
    CreateConnectedProfile {
        owner_scope: u64,
        draft: ConnectedProfileDraft,
    },
    /// Restore a retained local Profile selected from the hosted availability
    /// catalog. Store and profile switching remain shell-owned and off-loop.
    RestoreLocalProfile {
        profile: ProfileId,
    },
    /// Remove one local Profile through Agent/Store, optionally moving all of
    /// its active Workspaces to Trash first.
    RemoveLocalProfile {
        profile: ProfileId,
        delete_workspaces: bool,
    },
    /// Finish the recoverable local cascade only after Agent has durably
    /// acknowledged hosted deletion for this exact Profile.
    FinishHostedProfileDeletion {
        profile: ProfileId,
        discovered_after_offline: bool,
    },
    /// Keep a hosted-deleted local Profile by compare-and-clearing its exact
    /// Koru Identity binding. Content and local visibility remain unchanged.
    ContinueDeletedProfileOffline {
        profile: ProfileId,
        expected_identity_id: String,
    },
    /// Bind one local Profile only after a server-owned exact identity acknowledgement.
    BindLocalProfileAccount {
        profile: ProfileId,
        expected_identity_id: Option<String>,
        new_identity_id: String,
    },
    /// Move the display zoom — what the window menu's `−`, `+` and `↺` ask for.
    ///
    /// **An intent rather than a number.** The shell already owns the step and
    /// the bounds for the Ctrl+= / Ctrl+- / Ctrl+0 chords, and a view that sent
    /// a computed factor would be a second place that knows how big a step is —
    /// with a read-modify-write across the boundary to get it wrong. The row
    /// and the chord now reach the same line.
    Zoom(ZoomIntent),
    /// Open or focus the dedicated global-search window.
    OpenSearch,
    /// Open a fixed app-owned HTTPS or mailto destination through the injected
    /// system URI port. Views emit this typed effect and never perform I/O.
    OpenUri(&'static str),
    /// Configured app-owned web destination, dispatched through the Agent port.
    OpenWebsite(String),
    /// Open the approved Koru help page through the injected system browser port.
    OpenHelp,
    /// Open or reveal the singleton support window — the Help row, the
    /// tray, the floating icon and the reply notice all send this one.
    OpenSupport,
    /// Ask the embedding host's Agent link to open one exact AI chat block.
    /// Strings keep this generic shell independent of the AI domain model.
    OpenChatLink {
        chat: String,
        block: String,
    },
    /// Navigate to stable content. Every document target is resolved through
    /// Store here; views never decide which profile/workspace currently owns it.
    ///
    /// May switch the active workspace when the target lives elsewhere. Tab
    /// selection must not use this — use [`ActivateDocument`](Self::ActivateDocument).
    Navigate {
        target: NavigationTarget,
        /// Authored evidence from the selected result. The destination uses it
        /// only for local caret and transient-flash placement.
        match_text: Option<String>,
        disposition: SearchTargetDisposition,
    },
    /// Show an already-open (or openable) document **without** changing the
    /// active workspace or Explore. What selecting a tab does.
    ActivateDocument {
        document: koru_core::DocumentId,
        /// Home workspace the document room is joined under. When `None`, the
        /// shell opens against the current workspace.
        workspace: Option<WorkspaceId>,
    },
    /// Load `document` into the parked-tabs map and insert a background tab,
    /// without making it the active document.
    OpenBackground {
        document: koru_core::DocumentId,
        node: Option<NodeId>,
    },
    /// Open every listed document as tabs in one new window.
    OpenDocumentsInNewWindow {
        documents: Vec<koru_core::DocumentId>,
    },
    /// Show this workspace in this window (Explore / manifest). Open tabs stay.
    Switch(WorkspaceId),
    /// ...in a window of its own.
    OpenInNewWindow(WorkspaceId),
    /// Make one after the window has collected a non-empty name.
    NewWorkspace(String),
    /// Make one in another Profile, then switch this window to it there.
    /// The profile switcher can show any Profile's workspaces, so its
    /// *New workspace* row may name one that is not open.
    NewWorkspaceIn(ProfileId, String),
    /// Create a document in the open workspace.
    ///
    /// **A request rather than a call**, because a view holds the active
    /// `WorkspaceDoc` and a document belongs to the *workspace* — under the
    /// store, creating one is a round trip that registers it in the manifest.
    /// Inserting a node into the document in hand would nest a page inside
    /// whatever is open, which is a different thing that happens to look the
    /// same in the sidebar until you reopen.
    CreateDocument(String),
    /// Create a **canvas** in the open workspace, and open it.
    ///
    /// [`CreateDocument`](Self::CreateDocument)'s twin, and a request for the
    /// same reason: it registers a document in the manifest — under the kind
    /// `koru_core::explore::KIND_CANVAS`, seeded with its root alone and
    /// placed at the workspace root, because canvases are a flat list of their
    /// own. A variant rather than a kind field on `CreateDocument`, because a
    /// page's placement (beside the page it was made from) and its draft
    /// promotion are not a canvas's, and a field would make every
    /// `CreateDocument` handler ask which one it was.
    CreateCanvas(String),
    /// Create a document from the `@` picker's Add new page row, insert its
    /// routed pill at the original query, then open the new document.
    CreateLinkedDocument(String),
    /// Move the editor's selection into a **new** document and open it.
    ///
    /// The view captured the payload at request time; the shell creates the
    /// document, plants the content, and only then calls the view back
    /// (`complete_selection_move`) to replace the selection with the trail
    /// pill — the target write comes first, so no failure can lose content.
    MoveSelectionToNewDocument {
        title: String,
        payload: MoveSelectionPayload,
        /// Open the created document afterwards (Enter). `false` is
        /// Ctrl+Enter or Shift+Enter: perform the move and stay where the
        /// selection was.
        follow: bool,
        /// Shift+Enter: create the document as a **child** of the one the
        /// selection left, rather than its next sibling.
        child: bool,
    },
    /// Move the editor's selection into an **existing** document, at its top —
    /// or first under `node` when the picker drilled into a heading.
    ///
    /// The document may belong to another workspace; the shell resolves it
    /// through the store and, for one this window holds no replica of, writes
    /// over a store session off the event loop. The trail pill lands in the
    /// source only after the destination write is durable.
    MoveSelectionToDocument {
        document: koru_core::DocumentId,
        node: Option<koru_core::NodeId>,
        title: String,
        payload: MoveSelectionPayload,
    },
    /// Copy a document into a new one, and open the copy.
    ///
    /// **The other way out of the *In Trash* notice**, and a request for the
    /// same reason [`CreateDocument`](PaneRequest::CreateDocument) is: the copy
    /// is a document of the *workspace*, registered in the manifest, and a view
    /// holds no manifest. `title` is the view's, because what a copy is called —
    /// *"Notes (copy)"* — is a piece of the interface's language.
    CopyDocument {
        source: koru_core::DocumentId,
        title: String,
    },
    /// Let go of a document: its last tab has closed.
    ///
    /// **A tab is what keeps a document open.** Under the store an open document
    /// is a `LoroDoc`, a room and a socket thread, and the workspace cannot see
    /// a tab close — so without this every document ever opened stays open for
    /// as long as the window is up. The view sends it; only the shell can act on
    /// it, and it declines for the *active* document until another takes over.
    CloseDocument(koru_core::DocumentId),
    /// Move a document among the workspace's entries: a drag in Explore.
    ///
    /// `parent` is `None` for the top level. `index` is a position among the
    /// entries sharing that parent, which is not a tree index — the manifest
    /// translates.
    MoveDocument {
        id: koru_core::DocumentId,
        parent: Option<koru_core::DocumentId>,
        index: usize,
    },
    /// Move a document (and its explore subtree) to another workspace in this
    /// profile. Same-workspace reparent uses [`MoveDocument`] instead.
    ///
    /// `follow` asks the shell to switch to the target workspace and open the
    /// document after a successful transfer.
    TransferDocument {
        id: koru_core::DocumentId,
        source: WorkspaceId,
        target: WorkspaceId,
        parent: Option<koru_core::DocumentId>,
        index: usize,
        follow: bool,
        path_label: String,
    },
    /// Fetch another workspace's document listing for the Move to… picker
    /// without switching this window's open workspace.
    ListWorkspaceDocuments(WorkspaceId),
    /// Put these documents in the trash — **a move, not a delete**
    /// ([ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it)). They leave the library, are
    /// listed in the bin, and come back whole.
    ///
    /// A request rather than something the view does, for the same reason
    /// `MoveDocument` is one: the manifest belongs to the workspace, and a view
    /// holds none. It is also why the view cannot simply call `ws.trash(node)` —
    /// that is the *node* trash inside the open document, and an Explore row
    /// names a document, so it found nothing and failed in silence.
    TrashDocuments(Vec<koru_core::DocumentId>),
    /// Take these back out of the trash.
    ///
    /// Only their own stamp clears: a document whose *parent* is still binned
    /// stays out of sight, which is what a file manager does and must not read
    /// as a failure.
    RestoreDocuments(Vec<koru_core::DocumentId>),
    /// **Destroy every trashed document.** The one irreversible action in the
    /// product; the confirm is the view's, and this is what it confirms.
    EmptyDocumentTrash,
    /// **Destroy these documents, named.** The bin's own sweep walks the
    /// listing a person sees, which excludes the kinds nobody opens or drags
    /// — a delegated run among them. A run is deleted by the surface that
    /// shows runs, and only after the delegate worker has removed everything
    /// device-local it left behind, so this is the half that unregisters the
    /// Run document once that has answered.
    PurgeDocuments(Vec<koru_core::DocumentId>),
    /// Switch the requesting window to this Store profile in place.
    SwitchProfile(ProfileId),
    /// Switch the requesting window to this Profile **and this workspace in
    /// it**, in place. What the switcher sends when a workspace of another
    /// Profile is chosen; within the open Profile it is a workspace switch.
    SwitchToWorkspaceIn(ProfileId, WorkspaceId),
    /// Choose a folder, create a profile in it, and open it.
    AddProfile,
    /// **Create the Store profile this launch does not have, and sit in it.**
    ///
    /// First run's first entry (S21). Deliberately not [`AddProfile`], which
    /// asks for a *folder* and launches a second process against it — the
    /// transitional folder route, and the wrong answer for a window that is
    /// already open and simply has nobody in it.
    ///
    /// The window seats the new persona in place, over the path a profile switch
    /// already uses. Nothing restarts: the first-run window becomes the window
    /// somebody works in, which is what makes *Create profile* one press rather
    /// than a press and a relaunch.
    ///
    /// [`AddProfile`]: PaneRequest::AddProfile
    CreateStoreProfile,
    /// Choose a folder that **already holds** a profile, and open it. The way
    /// back to one the list has forgotten, without a command line.
    OpenProfile,
    /// **Choose where Koru makes a folder for a delegated run** (ADR-365).
    ///
    /// A view may not open a native dialog -- it blocks -- so the delegation
    /// modal asks and the shell performs it off the press, the way every other
    /// picker in this app is performed. The title arrives already localized:
    /// the language is the container's, and this crate has no business
    /// deciding what a dialog says.
    ///
    /// **Whether the folder can be written to is settled here too**, while the
    /// person is still looking at the picker, and the answer goes back through
    /// [`View::delegate_folder_chosen`]. Cancelling answers nothing at all.
    ChooseDelegateFolder {
        title: String,
        start: Option<std::path::PathBuf>,
    },
    /// **Choose where a session recording is saved** (SESSIONRECORD). The
    /// recording modal asks and the shell performs the save-file picker off
    /// the press, for [`ChooseDelegateFolder`](Self::ChooseDelegateFolder)'s
    /// reason: a view may not open a blocking native dialog. The title arrives
    /// localized for the same reason; `file_name` is the prefilled default
    /// (`koru-<timestamp>.mp4`). A choice goes back through
    /// [`View::recording_path_chosen`]; cancelling answers nothing at all.
    ChooseRecordingPath {
        title: String,
        start: Option<std::path::PathBuf>,
        file_name: String,
    },
    /// **Choose the folder an AI chat's local agent works in** (AGENTUX
    /// UC8). The chat surface asks and the shell performs the folder picker
    /// off the press, for [`ChooseDelegateFolder`](Self::ChooseDelegateFolder)'s
    /// reason. The title arrives localized. A choice goes back through
    /// [`View::ai_chat_folder_chosen`]; cancelling answers nothing at all.
    ChooseAiChatFolder {
        title: String,
        start: Option<std::path::PathBuf>,
    },
    RenameProfile(ProfileId, String),
    SetProfileAppearance {
        id: ProfileId,
        icon: Option<String>,
        color: Option<String>,
    },

    // ------------------------------------------------- the workspace verbs
    //
    // Each needs the profile document, the profile directory, or a process —
    // none of which a view holds. They are the menu's items, one for one.
    /// Give it another name.
    ///
    /// **Two writers, one authority.** The profile entry is the truth — it is
    /// what the switcher lists and what a launch reads — and the workspace
    /// document's own `meta.name` is reconciled from it when that workspace is
    /// opened. Opening a foreign workspace's `Session` purely to rename it
    /// would mean two processes writing one `snapshot.loro`, which is the one
    /// thing not to do.
    RenameWorkspace(WorkspaceId, String),
    /// Its emoji and its palette colour, both at once — they are one decision
    /// and one modal, so they are one write.
    SetWorkspaceIcon {
        id: WorkspaceId,
        icon: Option<String>,
        color: Option<String>,
    },
    /// Import files as documents in the workspace that is already open.
    ImportContent {
        format: &'static str,
        source: ImportSourceSelection,
    },
    /// Add opaque file entries to the workspace's Files projection.
    ImportFiles(ImportSourceSelection),
    /// Ask the active content-import worker to stop at its next safe checkpoint.
    CancelImport,
    /// **The voice consent step was answered.** `true` records the agreement
    /// and starts the download; `false` leaves the record untouched, because
    /// declining is *not now* rather than *never*.
    VoiceConsent(bool),
    /// Ask the running model download to stop at its next chunk boundary.
    CancelVoiceDownload,
    /// **Offer a speech model**, opening the consent step and then the
    /// download. What the Settings section's *Install* asks for, and the same
    /// work the tray row already asks the agent to hand a window.
    SetUpVoice,
    /// **Delete every installed speech model.** Deliberately not a revocation:
    /// removing the bytes says nothing about what somebody agreed to, and
    /// `koru-voice-consent`'s note is explicit that the two are separate facts.
    RemoveVoiceModels,
    /// **Which microphones can this machine see?** Answered by the worker over
    /// the voice wire, on a thread, into `koru_workspace_host::voice_devices` — a view
    /// may not block on a socket, and `cpal` is linked in exactly one process
    /// in this workspace and it is not this one.
    ///
    /// It starts no worker: drawing a picker is not somebody asking to
    /// dictate, so a machine with nothing running answers an empty list.
    AskVoiceDevices,
    /// Record the one-time first-reminder startup answer. The shell relays a
    /// field patch to the settings authority; OS and file work stays off the
    /// event loop and the correlated result is pushed back through `View`.
    AnswerReminderStartup(ReminderStartupRequest),
    /// Hand a notification's validated semantic snooze intent to the view.
    ReminderSnooze(ReminderSnoozeIntent),
    /// Save one workspace file to a user-selected path.
    SaveFile(koru_core::DocumentId),
    /// Save several workspace files into a user-selected folder.
    SaveFiles(Vec<koru_core::DocumentId>),
    /// Save a drawn diagram as a PNG or an SVG at a user-selected path. Served
    /// against the window that asked: its device rasterises the PNG.
    SaveDiagram(SaveDiagram),
    /// Put a picture of a drawn diagram on the clipboard. Served against the
    /// window that asked, like a save: its device rasterises the bitmap.
    CopyDiagram(CopyDiagram),
    /// Materialize one workspace file in Koru's temporary open cache and hand
    /// it to the operating system's associated application.
    OpenFile(koru_core::DocumentId),
    /// Materialize and open every selected workspace file through its OS
    /// association.
    OpenFiles(Vec<koru_core::DocumentId>),
    /// Open the one note attached to a workspace file, creating it when the
    /// file has none. The store is idempotent, so *Add note* and *Open note*
    /// are the same request.
    OpenFileNote(koru_core::DocumentId),
    /// Rename an opaque workspace file. Files have no document title row; the
    /// manifest title is the name Explore shows and Open in app uses.
    RenameFile(koru_core::DocumentId, String),
    /// Fetch one workspace file's bytes from a peer that holds them, so a file
    /// this device has only ever seen listed becomes usable here.
    ///
    /// Nothing is opened or saved: the point is the payload arriving and staying
    /// arrived, which is what turns a placeholder row into an ordinary one.
    DownloadFile(koru_core::DocumentId),
    /// Import an older workspace through preview and the Koru Store API.
    ///
    /// Inspection is read-only. The confirmation keeps the legacy identity
    /// when it is free, or asks the Store to import a fully reminted copy; an
    /// identity that is already present can only take the copy path.
    ImportWorkspace,
    /// The answer to the legacy import preview. `Cancel` performs no write.
    FinishWorkspaceImport(ImportChoice),
    /// Measure every workspace and push the answers back through
    /// [`View::set_workspace_stats`].
    ///
    /// **Asked for, never scheduled.** Each one is a directory walk, so this is
    /// raised when the manager opens rather than on any clock.
    RefreshWorkspaceStats,
    /// Sort it to the top of the switcher, or stop.
    PinWorkspace(WorkspaceId, bool),
    /// Persist the complete profile workspace order after a manager drag.
    ReorderWorkspaces(Vec<WorkspaceId>),
    /// Keep it out of the switcher, or put it back.
    ///
    /// **Never the last one that is not archived.** Archiving everything would
    /// leave `open_profile` with no workspace to choose on the next launch, and
    /// it would silently mint an empty one — so this is refused both by the menu
    /// that offers it and again by whoever performs it.
    ArchiveWorkspace(WorkspaceId, bool),
    /// Show a folder that is not a workspace's — what an error's *Reveal Folder*
    /// asks for, since the folder worth looking at after a failed delete is the
    /// trash, which no `WorkspaceId` names any more.
    RevealPath(PathBuf),
    /// Start a fresh Koru on this profile and end this process. For the one
    /// change a running shell cannot adopt: the Agent came back as a different
    /// device (*Start over as a new device*), and every view here holds the old
    /// Device ID's credential scope.
    Relaunch,
    /// Write a `.zip` of its folder somewhere the user picks.
    ExportWorkspace(WorkspaceId),
    /// Export one or more documents through a registered format (`"markdown"`, …).
    ///
    /// The shell owns the OS dialog (save-file for one, folder for many) and the
    /// last-export directory; the format crate turns each snapshot into files.
    ExportDocuments {
        ids: Vec<koru_core::DocumentId>,
        format: &'static str,
    },
    /// Move its folder to `<profile>/trash/` and drop the entry.
    ///
    /// **Nothing is unlinked.** Deleting content is the one unrecoverable bug,
    /// so this is a move: the bytes stay until somebody empties the trash by
    /// hand. Refused for the workspace this window is showing, and for the last
    /// one — both cases leave the window pointed at nothing.
    DeleteWorkspace(WorkspaceId),
    /// Put a softly deleted Store workspace back in the active workspace list.
    RestoreWorkspace(WorkspaceId),
}

/// Which way a zoom request moves. The three the display-zoom chords take.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ZoomIntent {
    /// One step smaller, down to the floor.
    Out,
    /// One step larger, up to the ceiling.
    In,
    /// Straight back to 100%.
    ActualSize,
}

impl PaneRequest {
    /// System browser effects are distinct from the Koru deep-link activation
    /// port. Shared dispatch keeps configured website URLs on the URI worker.
    pub fn dispatch_system_uri(&self, host: &impl Host) -> bool {
        let uri = match self {
            Self::OpenUri(uri) => *uri,
            Self::OpenWebsite(uri) => uri.as_str(),
            Self::OpenHelp => "https://koru.do/help",
            _ => return false,
        };
        host.open_uri(uri);
        true
    }
    /// What to call this request in a stall line.
    ///
    /// **`maintenance.pane_requests` names a queue, not a job.** Every one of
    /// these is served on the main thread and several open documents over a
    /// socket, so a stall attributed to the drain says only that *something*
    /// expensive was asked for. A `&'static str` per variant costs nothing and
    /// turns the next log into an answer.
    pub fn stall_name(&self) -> &'static str {
        match self {
            Self::Account(..) | Self::AccountOwned { .. } | Self::AccountTracked { .. } => {
                "request.account"
            }
            Self::CreateConnectedProfile { .. } => "request.create_connected_profile",
            Self::RestoreLocalProfile { .. } => "request.restore_local_profile",
            Self::RemoveLocalProfile { .. } => "request.remove_local_profile",
            Self::FinishHostedProfileDeletion { .. } => "request.finish_hosted_profile_deletion",
            Self::ContinueDeletedProfileOffline { .. } => {
                "request.continue_deleted_profile_offline"
            }
            Self::BindLocalProfileAccount { .. } => "request.bind_local_profile_account",
            Self::Zoom(..) => "request.zoom",
            Self::OpenSearch => "request.open_search",
            Self::OpenUri(..) => "request.open_uri",
            Self::OpenWebsite(..) => "request.open_website",
            Self::OpenHelp => "request.open_help",
            Self::OpenSupport => "request.open_support",
            Self::OpenChatLink { .. } => "request.open_chat_link",
            Self::Navigate { .. } => "request.navigate",
            Self::ActivateDocument { .. } => "request.activate_document",
            Self::OpenBackground { .. } => "request.open_background",
            Self::OpenDocumentsInNewWindow { .. } => "request.open_documents_new_window",
            Self::CreateLinkedDocument(_) => "request.create_linked_document",
            Self::MoveSelectionToNewDocument { .. } => "request.move_selection_new",
            Self::MoveSelectionToDocument { .. } => "request.move_selection",
            Self::Switch(..) => "request.switch_workspace",
            Self::OpenInNewWindow(..) => "request.new_window",
            Self::NewWorkspace { .. } => "request.new_workspace",
            Self::NewWorkspaceIn(..) => "request.new_workspace_in",
            Self::CreateDocument { .. } => "request.create_document",
            Self::CreateCanvas(..) => "request.create_canvas",
            Self::CopyDocument { .. } => "request.copy_document",
            Self::CloseDocument(..) => "request.close_document",
            Self::MoveDocument { .. } => "request.move_document",
            Self::TransferDocument { .. } => "request.transfer_document",
            Self::ListWorkspaceDocuments(..) => "request.list_workspace_documents",
            Self::TrashDocuments(..) => "request.trash_documents",
            Self::RestoreDocuments(..) => "request.restore_documents",
            Self::EmptyDocumentTrash => "request.empty_trash",
            Self::PurgeDocuments(..) => "request.purge_documents",
            Self::SwitchProfile(..) => "request.switch_profile",
            Self::SwitchToWorkspaceIn(..) => "request.switch_to_workspace_in",
            Self::RenameProfile(..) => "request.rename_profile",
            Self::SetProfileAppearance { .. } => "request.profile_appearance",
            Self::AddProfile => "request.add_profile",
            Self::CreateStoreProfile => "request.create_store_profile",
            Self::OpenProfile => "request.open_profile",
            Self::ChooseDelegateFolder { .. } => "request.choose_delegate_folder",
            Self::ChooseRecordingPath { .. } => "request.choose_recording_path",
            Self::ChooseAiChatFolder { .. } => "request.choose_ai_chat_folder",
            Self::RenameWorkspace(..) => "request.rename_workspace",
            Self::SetWorkspaceIcon { .. } => "request.workspace_icon",
            Self::ImportContent { .. } => "request.import_content",
            Self::ImportFiles(..) => "request.import_files",
            Self::CancelImport => "request.cancel_import",
            Self::VoiceConsent(..) => "request.voice_consent",
            Self::CancelVoiceDownload => "request.cancel_voice_download",
            Self::SetUpVoice => "request.set_up_voice",
            Self::RemoveVoiceModels => "request.remove_voice_models",
            Self::AskVoiceDevices => "request.ask_voice_devices",
            Self::AnswerReminderStartup(..) => "request.reminder_startup",
            Self::ReminderSnooze(..) => "request.reminder_snooze",
            Self::SaveFile(..) => "request.save_file",
            Self::SaveFiles(..) => "request.save_files",
            Self::SaveDiagram(..) => "request.save_diagram",
            Self::CopyDiagram(..) => "request.copy_diagram",
            Self::OpenFile(..) => "request.open_file",
            Self::OpenFiles(..) => "request.open_files",
            Self::OpenFileNote(..) => "request.open_file_note",
            Self::RenameFile(..) => "request.rename_file",
            Self::DownloadFile(..) => "request.download_file",
            Self::ImportWorkspace => "request.import_workspace",
            // The second half of an import, after the preview has been
            // answered: the conversion and the Store call, which is the
            // expensive one and therefore the one worth naming.
            Self::FinishWorkspaceImport(..) => "request.finish_workspace_import",
            Self::RefreshWorkspaceStats => "request.workspace_stats",
            Self::PinWorkspace { .. } => "request.pin_workspace",
            Self::ArchiveWorkspace { .. } => "request.archive_workspace",
            Self::RevealPath(..) => "request.reveal_path",
            Self::Relaunch => "request.relaunch",
            Self::ExportWorkspace(..) => "request.export_workspace",
            Self::ExportDocuments { .. } => "request.export_documents",
            Self::DeleteWorkspace(..) => "request.delete_workspace",
            Self::RestoreWorkspace(..) => "request.restore_workspace",
            Self::ReorderWorkspaces(..) => "request.reorder_workspaces",
        }
    }
}

/// One flattened row in the legacy import preview.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImportPreviewRow {
    pub depth: usize,
    pub title: String,
    pub nodes: usize,
    pub trashed: bool,
}

/// The read-only information an import confirmation renders.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImportPreview {
    pub workspace_name: String,
    pub document_count: usize,
    pub node_count: usize,
    pub image_count: usize,
    pub warnings: Vec<String>,
    pub rows: Vec<ImportPreviewRow>,
    /// An existing workspace already owns this identity, so only Copy is valid.
    pub identity_conflict: bool,
}

/// The import identity decision made in the preview modal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ImportChoice {
    KeepIdentity,
    Copy,
    Cancel,
}

/// Where a global-search result should open.
///
/// Kept on the stable target request rather than inferred from keyboard state
/// in the shell: mouse and keyboard entry points produce the same navigation
/// object, and the shell remains independent of search-surface key bindings.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SearchTargetDisposition {
    /// Somewhere in this process. The window that asked is offered the landing
    /// first, but a window that already holds the document wins — raising the
    /// copy that is already open beats putting a second one in front of
    /// somebody.
    CurrentWindow,
    /// **The window that asked, and no other.**
    ///
    /// For a navigation that is only *half* of a gesture. The explore row's
    /// *Rename* and its four *Attributes* rows have no `&WorkspaceDoc` at the
    /// click, so they record what they want on the container that raised them
    /// and complete it when the document lands there. Under
    /// [`Self::CurrentWindow`] a second window on the same workspace can take
    /// that landing — it holds the document, so it is preferred — and then the
    /// picker opens over the window nobody right-clicked in while the window
    /// that asked waits for an arrival that has already happened
    /// (Q44 in `docs/features/trackable/DECISIONS.md`).
    ///
    /// A gesture belongs to the window it was made in, so this disposition says
    /// so on the wire rather than leaving it to a preference order that is
    /// right for a search result and wrong for a half-finished gesture.
    SourceWindow,
    NewWindow,
}

/// Something the Koru Agent asked this window to do.
///
/// The shell's own vocabulary: the host converts from `koru-agent-wire`, which
/// this crate deliberately does not link — an agent message is a fact about a
/// protocol, and what a window does about it is a fact about a window.
pub enum AgentAsk {
    /// Open a result chosen in the machine-wide overlay.
    Open(Box<PaneRequest>),
    /// Navigate to the occurrence's entity, then hand the semantic snooze ask
    /// to the same pane without choosing a surface or duration. The Agent sends
    /// the durable Open immediately before this ask; carrying that navigation a
    /// second time here made the picker open between two identical navigations.
    ReminderSnooze {
        intent: ReminderSnoozeIntent,
        /// Agent's device-local one-shot token, acknowledged only after both
        /// requests have entered the event-loop-owned queue.
        delivery: String,
    },
    /// Open the profile-global All Reminders destination.
    OpenReminders,
    /// The notification area asked for Contact support.
    ShowSupport,
    /// Come back from the notification area.
    Restore,
    /// Show a localized, already-sanitized application error through the same
    /// in-window surface used by other post-startup failures.
    ReportError(ErrorReport),
    /// Save and close: the agent is about to replace this installation's files.
    ///
    /// **A close, not a quit.** It runs the same `initiate_close` the ✕ runs, on
    /// every window — which is what puts the unsaved-work dialog in front of
    /// somebody who has one, for free and with no second code path. A window
    /// that survives that pass has refused, and the host says so back to the
    /// agent; nothing here forces the issue, because a person who says *cancel*
    /// to a save prompt has answered the update question too.
    CloseForUpdate,
    /// An install finished. Open the page saying what this build changed.
    ///
    /// **Sent to one window, once.** The agent forgets it the moment a shell has
    /// been told, so this does not arrive again on a reconnect and there is no
    /// state on this side to keep in step. Carries the version the agent
    /// installed; see [`View::open_whats_new`].
    ShowWhatsNew(String),
    /// Open Feedback in a standalone window, asked by the notification area.
    ShowFeedback,
    /// **Set voice input up**, asked by the notification area's row.
    ///
    /// The same beginning the first dictation has: the consent step opens, and
    /// what it says comes from a catalog this window fetches and verifies for
    /// itself. It is an ask rather than a report for the reason
    /// [`AgentAsk::ShowWhatsNew`] is — somebody pressed a row once, and the
    /// answer is a modal rather than something a surface redraws from.
    SetUpVoice,
    /// **Listen with this microphone**, chosen from the notification area's
    /// *Input* submenu. `None` is *follow the operating system*.
    ///
    /// A `String` rather than a `koru_voice::InputChoice` for the whole
    /// journey: `koru-agent-wire` carries names as strings on purpose, and this
    /// is the first place with a voice vocabulary to parse it in.
    ///
    /// **The shell applies it because the shell owns `preferences.conf`.**
    /// `koru_window::user_preferences` is explicit that there is one writer,
    /// so a tray choice is a gesture routed here rather than a second process
    /// editing a file this one rewrites whole.
    SetVoiceInput(Option<String>),
    /// Route Agent's registered Dictation chord into this shell's existing
    /// local controller. The opaque token correlates lifecycle only.
    DictationGesture {
        route: u64,
        gesture: AgentDictationGesture,
    },
    /// The Agent connection that owned any routed local dictation ended.
    /// Carries no transport detail and never applies to a native session.
    AgentDisconnected,
    /// One exact local Profile was deleted from Koru by a trusted peer. The
    /// host must stop presenting it and run the recoverable local cascade.
    ProfileLifecycleDeleted(ProfileId),
    /// **Open this running terminal session's tab** — one of the notification
    /// area's Agents rows (Q-E8 P9). The pane already showing the session is
    /// focused; otherwise a container pane opens a tab attached to it
    /// ([`View::open_session_tab`] — attach, never launch).
    ShowSession(koru_session_protocol::SessionId),
    /// **Start a new agent session** — the Agents menu's New. Focuses a
    /// container and opens its new-tab chooser
    /// ([`View::open_new_session_chooser`]): which agent to start is the
    /// person's choice, and the notification area has nowhere to ask it.
    NewAgentSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentDictationKey {
    D,
    M,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentDictationGesture {
    Press(AgentDictationKey),
    Release(AgentDictationKey),
    Cancel(AgentDictationKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentDictationState {
    Refused,
    Listening(koru_dictation::Mode),
    Finalising,
    Idle,
}

/// A platform global-hotkey activation waiting for the winit shell.
///
/// Carrying the physical trigger is what lets the receiving search pane ignore
/// the press/repeats that caused it to receive focus.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct HotkeyActivation {
    pub trigger: winit::keyboard::KeyCode,
}

/// **This window opened with no profile at all** — S21's first run.
/// What a workspace costs and where it is, for a view that lists them.
///
/// Plain data, pushed in by the shell — a view cannot query the Store. Missing
/// values still render as a dash rather than zero; a failed usage refresh is
/// absence of an answer, not an empty workspace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceStat {
    pub id: WorkspaceId,
    /// Deduplicated content-addressed bytes referenced by live file entries.
    pub live_bytes: u64,
    /// The timestamped copies under `backups/`, counted separately because they
    /// dwarf the document and one total would read as a bug.
    pub backup_bytes: u64,
    /// How many page/document entries the Store currently lists. File entries
    /// contribute to `live_bytes`, not to this column.
    pub docs: Option<i64>,
    /// How many live opaque file entries the Store currently lists.
    pub files: Option<i64>,
    pub sync: SyncState,
    /// When this machine last opened it. `None` means never — which is an
    /// ordinary state for a workspace that arrived in a synced folder.
    pub last_opened_ms: Option<i64>,
    /// When this batch was measured, so a view can say "3 days ago" without
    /// holding a clock of its own.
    pub now_ms: Option<i64>,
    /// Where it lives, for display. **Not identity** — a folder moves.
    pub folder: String,
    /// Machine-local derived search status. `None` while the search service is
    /// unavailable; never inferred from the workspace's cloud folder.
    pub search: Option<WorkspaceSearchStat>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceSearchStat {
    pub state: SearchIndexState,
    pub live_fragments: u64,
    pub trash_fragments: u64,
    pub pending_changes: u64,
    pub last_indexed_ms: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SearchIndexState {
    Queued,
    Indexing,
    Ready,
    Updating,
    NeedsAttention,
}

impl SearchIndexState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Indexing => "Indexing",
            Self::Ready => "Ready",
            Self::Updating => "Updating",
            Self::NeedsAttention => "Needs attention",
        }
    }
}

/// What is known about a workspace's replication.
///
/// Honest by construction: there is no server, so the only thing that can be
/// observed is whether another *instance on this machine* is writing to the
/// workspace's update log. Everything else is `Local`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SyncState {
    /// Nothing but this machine has it, as far as anything here can tell.
    Local,
    /// Another window or process has it open right now.
    Live,
    /// A peer log would not decode. Its content is intact in the snapshot; the
    /// damaged file was quarantined.
    Damaged,
}

impl SyncState {
    pub fn label(self) -> &'static str {
        match self {
            SyncState::Local => "Local only",
            SyncState::Live => "Open elsewhere",
            SyncState::Damaged => "Needs attention",
        }
    }
}

/// Something went wrong that the person at the keyboard has to know about.
///
/// **Not an `eprintln!`.** There is no console under `windows_subsystem`, so
/// everything written that way has been shouted into a void — which is how a
/// failed save became a silent data-loss path. And not an `rfd::MessageDialog`
/// either: that is the right tool in `startup`, which runs *before there is a
/// window*, and the wrong one afterwards, when we can say something specific and
/// offer a way out.
///
/// The shape is the design's: a plain-language title naming the thing, a body
/// that says **what was not changed** as well as what failed, the raw error
/// underneath for anyone who wants it, and buttons that do something.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ErrorReport {
    /// Names the thing, not the operation — *Couldn't create "Research 2026"*.
    pub title: String,
    /// What happened, and what state the world is in now.
    ///
    /// **The second half is the load-bearing one.** After a failed delete or a
    /// half-finished export the first question is always *did it partly
    /// happen*, and a message that does not answer it leaves somebody opening a
    /// file manager to find out. Every raise site has to be able to say this
    /// truthfully, which is a real constraint on how the operations are
    /// ordered — see `move_to_trash`.
    pub body: String,
    /// The underlying error, verbatim, shown under a fold. Never parsed, never
    /// prettified: its value is that it is exactly what the OS said.
    pub details: Option<String>,
    /// Offered left to right after *Cancel*. The last is drawn as the primary.
    pub actions: Vec<ErrorAction>,
}

/// A way out of an error, offered as a button.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ErrorAction {
    /// Show the folder involved, so somebody can look for themselves.
    Reveal(PathBuf),
    /// Do the whole thing again — including whatever picker it began with,
    /// which is what makes this also the "choose somewhere else" button.
    Retry(PaneRequest),
}

impl ErrorAction {
    pub fn label(&self) -> &'static str {
        match self {
            ErrorAction::Reveal(_) => "Reveal Folder",
            ErrorAction::Retry(_) => "Try Again",
        }
    }
}

impl ErrorReport {
    /// The common shape: something failed, nothing changed, here is the error.
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            details: None,
            actions: Vec::new(),
        }
    }

    pub fn with_details(mut self, details: impl std::fmt::Display) -> Self {
        self.details = Some(details.to_string());
        self
    }

    pub fn with_action(mut self, action: ErrorAction) -> Self {
        self.actions.push(action);
        self
    }
}

/// What makes views. Implemented by whatever composes an application out of
/// them — a single-view binary, or a manager that can show either kind.
pub trait Host {
    type V: View;

    /// Override the ordinary Koru application icon for this host's windows.
    /// Auxiliary applications use this to keep the Koru mark while adding a
    /// small distinguishing overlay in the taskbar and app switcher.
    fn window_icon(&self) -> Option<winit::window::Icon> {
        None
    }

    /// Optional machine-local geometry namespace for this host's primary
    /// window. Profile/session state continues to use the replica slot; this
    /// changes only the row owned in `windows.conf`.
    fn window_geometry_slot(&self) -> Option<&'static str> {
        None
    }

    /// Give background host adapters the event-loop wake handle once it exists.
    /// Queuing work without this wake creates a race where `about_to_wait`
    /// drains too early and sleeps past an agent-delivered navigation.
    fn install_wake_proxy(&mut self, _proxy: winit::event_loop::EventLoopProxy<app::Wake>) {}

    /// Installation-scoped credentials acknowledged by the Agent greeting.
    /// Hosts without an Agent return `None`; OAuth then remains session-only.
    fn oauth_credential_context(&self) -> Option<koru_oauth_credentials::CredentialContext> {
        None
    }

    /// Latest content-light account state reported by Agent.
    fn account_state(&self) -> koru_agent_wire::AccountState {
        koru_agent_wire::AccountState::default()
    }

    /// Taken wake flag for account state/completion changes.
    fn take_account_dirty(&self) -> bool {
        false
    }

    /// One targeted operation completion. Unlike broadcast state, this may carry
    /// Profile display metadata and is delivered only to the requesting shell.
    fn take_account_result(&self) -> Option<(u64, koru_agent_wire::AccountOperationResult)> {
        None
    }

    fn take_scoped_account_result(
        &self,
    ) -> Option<(u64, u64, koru_agent_wire::AccountOperationResult)> {
        self.take_account_result()
            .map(|(generation, result)| (0, generation, result))
    }

    /// Send account work to Agent. Views reach this only through PaneRequest.
    fn account_command(&self, _command: koru_agent_wire::AccountCommand) {}
    fn account_command_owned(&self, _owner_scope: u64, command: koru_agent_wire::AccountCommand) {
        self.account_command(command);
    }
    fn account_command_tracked(
        &self,
        _submission: account_ui::CommandSubmissionId,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.account_command(command);
    }
    fn account_command_tracked_owned(
        &self,
        submission: account_ui::CommandSubmissionId,
        _owner_scope: u64,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.account_command_tracked(submission, command);
    }
    fn take_account_command_failure(&self) -> Option<account_ui::CommandFailure> {
        None
    }
    fn take_account_command_admission(&self) -> Option<account_ui::CommandAdmission> {
        None
    }
    fn account_command_deadline(&self) -> Option<std::time::Instant> {
        None
    }
    fn remove_local_profile(&self, _profile: ProfileId) {}

    fn take_local_profile_removal(
        &self,
    ) -> Option<(
        String,
        Result<koru_agent_wire::LocalProfileRemovalOutcome, String>,
    )> {
        None
    }

    /// Atomically compare-and-set one local Profile to the exact hosted Koru identity.
    fn bind_local_profile_account(
        &self,
        _profile: ProfileId,
        _expected_identity_id: Option<String>,
        _new_identity_id: String,
    ) {
    }

    fn take_local_profile_binding(
        &self,
    ) -> Option<(
        String,
        Result<koru_agent_wire::LocalProfileAccountBindingOutcome, String>,
    )> {
        None
    }

    /// The host-owned route that asks supervision for the voice worker.
    ///
    /// `koru-workspace-host` owns the voice client but deliberately does not link the
    /// Agent wire. A desktop host returns a closure over its Agent connection;
    /// an auxiliary host with no Agent returns `None`, and a missing worker is
    /// then refused immediately instead of being polled for ten seconds.
    fn voice_ensure(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        None
    }

    /// Whether anything about updating has changed since this was last asked.
    ///
    /// **The shell no longer owns updating** — `koru-agent` checks, downloads
    /// and installs, and what reaches a window is a report it draws a banner
    /// from. So the one thing the loop still has to do is notice: the report
    /// arrives on a reader thread, and a window that only found out on the next
    /// keystroke would sit on a finished download. Taken by the asking, and
    /// `false` for every host that has no agent.
    fn take_update_redraw(&self) -> bool {
        false
    }

    /// A window survived [`AgentAsk::CloseForUpdate`], so the install cannot
    /// happen. Say so back to the agent.
    ///
    /// **A hook rather than a value the shell sends**, because the sentence is
    /// `UpdateChoice::Later` and this crate does not link `koru-agent-wire` —
    /// the same division `take_agent_asks` already draws, read the other way.
    fn update_close_refused(&self) {}

    /// Raised by a platform hotkey owner. Polled by the shell alongside other
    /// background services so the platform module never reaches into winit.
    fn take_global_search_request(&self) -> Option<HotkeyActivation> {
        None
    }

    fn take_global_search_notice(&self) -> Option<String> {
        None
    }

    /// A correlated failure returned by the host's Agent connection.
    ///
    /// The blocking work and pipe read have already happened on worker
    /// threads; the shell only places this report into the active view.
    fn take_agent_notice(&self) -> Option<ErrorReport> {
        None
    }

    /// A taskbar activation (a Jump List *Search* / *New document* / Recent
    /// click) landed and the window should come forward.
    ///
    /// **Consumed by the shell, which focuses its own window** — the foreground
    /// ask must come from the thread that owns the window, through winit's
    /// `focus_window`, not from a background listener thread guessing an HWND.
    /// This is the same rule the tray's *Restore* path already follows. The
    /// listener only sets a flag and wakes; the shell does the showing.
    fn take_activation_focus(&self) -> bool {
        false
    }

    /// A Jump List *Search* click: open the agent's global search overlay.
    ///
    /// **Performed by the host, not the shell.** The overlay is the agent's
    /// window, and only the host holds the agent link — so the host drains the
    /// flag, grants the agent the foreground, and asks it to show the overlay.
    /// The shell polls this in `about_to_wait` (reliably, on the listener's
    /// wake) rather than relying on a redraw, and does nothing with the result.
    fn take_search_activation(&self) -> bool {
        false
    }

    /// A Jump List *AI Chat* click: open or focus the agent-owned standalone
    /// chat window. As with Search, the host owns the Agent connection and
    /// performs the request when the shell drains this hook.
    fn take_ai_chat_activation(&self) -> bool {
        false
    }

    /// Open one fixed application-owned URI through the desktop's registered
    /// handler. HTTPS goes to the browser; `mailto:` goes to the mail app.
    fn open_uri(&self, _uri: &str) {}

    /// Route an exact AI chat block through the host-owned Agent connection.
    fn open_chat_link(&self, _chat: String, _block: String) {}

    /// Route one URL delivered by the platform application delegate. The Koru
    /// desktop host validates and forwards `koru://` through its Agent link;
    /// other shell hosts have no registered URL scheme and ignore it.
    fn open_external_url(&self, _url: String) -> Result<(), String> {
        Ok(())
    }

    fn take_unpinned_documents(&self) -> Vec<koru_core::DocumentId> {
        Vec::new()
    }

    /// What the Koru Agent has asked this window to do since it was last
    /// asked — a result chosen in the machine-wide overlay, or a request to
    /// come back from the notification area.
    ///
    /// **A `Vec`, not an `Option`.** The agent pushes without being asked, so
    /// two things can arrive between one poll and the next; taking one and
    /// leaving the other would drop a navigation somebody made.
    ///
    /// Already translated into this shell's vocabulary by the host, because the
    /// wire types belong to `koru-agent-wire` and this crate does not link it.
    fn take_agent_asks(&self) -> Vec<AgentAsk> {
        Vec::new()
    }

    /// Confirm that an Agent snooze ask is now retained by the shell event
    /// loop. A disconnected acknowledgement leaves Agent's copy retryable.
    fn accept_reminder_snooze(&self, _delivery: String) {}

    /// Tell the agent what this window is showing, so a result can be routed to
    /// it. Called on the first workspace and on every switch.
    ///
    /// **The agent keys on the connection, never on a pid or a window**, so
    /// this updates one entry rather than adding a second.
    fn agent_showing(
        &self,
        _profile: ProfileId,
        _workspace: WorkspaceId,
        _profile_name: &str,
        _workspace_name: &str,
        _pinned: &[(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)],
    ) {
    }

    /// Whether the agent can bring a hidden Koru process back.
    fn agent_available(&self) -> bool {
        false
    }

    /// This window came to the front. Carries no time — see
    /// `koru_agent_wire::Request::Active`.
    fn agent_active(&self) {}

    /// Return content-free lifecycle state for an Agent-routed local
    /// dictation. Defaulted only for host abstraction compatibility.
    fn agent_dictation_state(&self, _route: u64, _state: AgentDictationState) {}

    /// The host's window focus changed. Most hosts need no copy of this state;
    /// a singleton auxiliary window can expose it to the process that owns a
    /// global toggle without asking the OS to identify foreground windows.
    fn window_focus_changed(&self, _focused: bool) {}

    fn workspace_search_stat(
        &self,
        _profile: ProfileId,
        _workspace: WorkspaceId,
    ) -> Option<WorkspaceSearchStat> {
        None
    }

    /// Protect a workspace whose live in-memory document this process owns.
    /// Called before any view or switch adopts it, so a background disk scanner
    /// can exclude the source before it has a chance to reopen peer logs.
    fn workspace_opened(&mut self, _profile: ProfileId, _workspace: WorkspaceId) {}

    /// Release a workspace after this process has either switched away from it
    /// or failed to finish opening it. Defaulted for hosts with no scanner.
    fn workspace_closed(&mut self, _profile: ProfileId, _workspace: WorkspaceId) {}

    /// The view the first window shows. Takes the whole workspace because a
    /// first view may have to seed an empty one, which is a real edit.
    fn initial(&mut self, workspace: &mut Workspace) -> Self::V;

    /// Another window on the same thing — Ctrl+N.
    fn duplicate(&mut self, ws: &WorkspaceDoc) -> Self::V;

    /// The process switched workspaces.
    ///
    /// Do to the incoming document whatever [`initial`] does before a view is
    /// built over it — give an empty one a line to type on, turn undo back on —
    /// and nothing else. `koru-workspace-host` cannot call the editor's `prepare`
    /// itself; that is the whole reason this hook exists, and it is the same
    /// reason `initial` takes a `&mut Workspace`.
    ///
    /// Defaulted, so a single-view binary that can never switch need not care.
    ///
    /// [`initial`]: Host::initial
    fn adopt_workspace(&mut self, _workspace: &mut Workspace) {}

    /// A window showing one document. `None` if this host has no way to show
    /// one, which is how a single-view binary declines.
    fn open_document(&mut self, ws: &WorkspaceDoc, doc: NodeId) -> Option<Self::V>;

    /// The reusable search surface in a window of its own.
    fn open_search(&mut self, _ws: &WorkspaceDoc) -> Option<Self::V> {
        None
    }

    /// A window whose only job is the Send feedback tab.
    fn open_feedback(&mut self, _ws: &WorkspaceDoc) -> Option<Self::V> {
        None
    }

    /// The singleton support window: the chat, and only the chat. `None`
    /// when support is not offered.
    fn open_support(&mut self, _ws: &WorkspaceDoc) -> Option<Self::V> {
        None
    }

    /// The floating support icon's window. `None` when there is no chat.
    fn open_fab(&mut self, _ws: &WorkspaceDoc) -> Option<Self::V> {
        None
    }

    /// The event loop is about to end; every window still exists. An app
    /// that owns UI-thread objects which must die before their windows —
    /// the support chat's web surface — drops them here.
    fn exiting(&mut self) {}

    /// The last document window is closing and the close preference says
    /// quit. Background work that would otherwise keep the process alive —
    /// the support chat — ends here (D24).
    fn last_window_closing(&mut self) {}
}
