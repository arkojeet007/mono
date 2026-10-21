// SPDX-License-Identifier: MPL-2.0

//! Every shortcut the app offers, in one table.
//!
//! ## What this is for
//!
//! Four layers resolve a key press — the shell, the container, the document
//! view, and the editor — and each used to be the only record of what it bound.
//! Three things follow from that, and all three were real:
//!
//! 1. **Collisions were prose.** `mainview::keys` carries a comment saying
//!    `Ctrl+Shift+N` is not free because it is the document's Narrow mode, and
//!    that claiming it "would not conflict, it would silently remove Narrow".
//!    That reasoning was correct and entirely unenforced — it depended on the
//!    next person reading a comment. It is now
//!    [`no_shortcut_shadows_a_lower_layer`](tests).
//! 2. **Labels drifted.** 77 hardcoded `"Ctrl+…"` strings, every one of them
//!    wrong on macOS.
//! 3. **The user guide could say anything.** 118 chord mentions that nothing
//!    verified.
//!
//! ## What keeps it honest
//!
//! A table that merely *describes* four resolvers is a fifth copy of the truth,
//! and would drift from them within a release. Two things stop that:
//!
//! - **The resolvers are driven from it.** `keymap`'s tests and
//!   `mainview::keys`'s tests take every entry for their layer and assert the
//!   real resolver answers it. A chord that changes in one place fails there.
//! - **The layer ordering is checked**, so an entry cannot quietly shadow one
//!   below it.
//!
//! ## `ShortcutId` is stable storage the moment bindings are customisable
//!
//! Nothing persists these yet. When it does, a user's remapped bindings will be
//! filed under these names — so renaming a variant will orphan their config,
//! exactly as renaming `NodeMeta.colour` would orphan a document
//! (`docs/architecture/DATAMODEL.md`). Say so before renaming one.

use winit::keyboard::NamedKey;

use crate::chord::{Chord, On, named};
use crate::modifiers::{NONE, PRIMARY, PRIMARY_ALT, PRIMARY_SHIFT, SHIFT};

/// Which resolver owns a chord, in the order they are asked.
///
/// **The order is the shadowing order.** The shell sees a key press first and a
/// chord it claims never reaches the container; the container's claims never
/// reach the view; the view's never reach the editor. `Layer as u8` is that
/// precedence, and the collision test reads it.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer {
    /// `koru-workspace-host`'s `app.rs` — needs the `Window` itself.
    Shell = 0,
    /// Standalone Koru Terminal input policy after window-level actions.
    Terminal = 1,
    /// `apps/koru`'s `mainview::keys` — tabs, columns, the workspace manager.
    Container = 2,
    /// `apps/koru-document` — the view modes and the selection bubble.
    View = 3,
    /// `koru-editor`'s `input::keymap` — text, marks, structure.
    Editor = 4,
}

impl Layer {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Shell => "Window",
            Self::Terminal => "Terminal",
            Self::Container => "Tabs and columns",
            Self::View => "Document",
            Self::Editor => "Editing",
        }
    }
}

/// A stable name for one shortcut.
///
/// **Not the chord.** This is what a customised binding will be filed under —
/// see the module docs.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ShortcutId {
    // ------------------------------------------------------------- the shell
    NewWindow,
    PinWindow,
    ToggleUiInspector,
    ToggleInspectorSpacingRays,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NextTab,
    PreviousTab,
    HistoryBack,
    HistoryForward,
    /// **Push to talk.** Held down, it dictates into whatever a typed
    /// character would reach; a preference turns it into a toggle. Resolved at
    /// the shell because the shell is the only layer that sees a key *release*
    /// — see `View::dictate` and `app.rs`'s release seam.
    Dictate,
    // ---------------------------------------------------------- the terminal
    TerminalCopyOrInterrupt,
    TerminalCopySelection,
    TerminalPaste,
    TerminalPasteAlternate,
    /// **Settings, and only Settings.**
    ///
    /// The tab lane wanted three chords and got one, because two of them are
    /// the shell's. `Ctrl+T` is readline's *transpose characters* and `Ctrl+W`
    /// is its *delete previous word*, and a window layer that took either would
    /// be taking it from the PTY — the exact inversion of the rule that lets
    /// this terminal keep swallowing `Ctrl+C`. Every terminal emulator worth
    /// copying reaches for `Ctrl+Shift+…` for its own tabs for the same reason.
    ///
    /// `Ctrl+Shift+W` is free here (the container's *Close all tabs* has it and
    /// the two layers never coexist); `Ctrl+Shift+T` is **not** — the shell's
    /// [`PinWindow`](Self::PinWindow) owns it, and `Layer::Shell` coexists with
    /// everything. So a pair would have had to be split across two idioms, and
    /// neither is shipped until the owner picks one. The `+` is a button and a
    /// tab's ✕ is a box; `Ctrl+,` has no shell meaning at all.
    TerminalSettings,
    /// **The explore pane, inside the Terminal.**
    ///
    /// `Ctrl+Shift+E`, which is the container's chord for the same pane and VS
    /// Code's for its Explorer. A separate id rather than reusing
    /// [`ToggleExplore`](Self::ToggleExplore) because the two live in different
    /// binaries and a chord that is customised in one has no business moving in
    /// the other — and because `of_layer(Layer::Terminal)` is what a Terminal
    /// shortcut sheet prints.
    ///
    /// It resolves at the **window** layer, above `Layer::Terminal`'s own, for
    /// the reason `TerminalSettings` gives: the pane is chrome, and a key the
    /// grid could swallow would be a pane you cannot open while a shell is
    /// running.
    TerminalToggleExplore,
    // ---------------------------------------------------------- the container
    NewTab,
    AddContent,
    CloseTab,
    CloseAllTabs,
    /// Global search in this window.
    Search,
    ToggleExplore,
    /// The explore column's views, `Alt+1` to `Alt+8` — the order the
    /// icons at its head sit in. Plain Alt, not the primary modifier:
    /// `Ctrl+Alt+digit` is a heading and `Ctrl+digit` a tab, and the digits
    /// are free under Alt alone on every layer.
    ShowDocuments,
    ShowFiles,
    ShowTasks,
    /// The Chats view: every chat newest first, with who answered it
    /// (AGENTUX I5). Right of Tasks, so `Alt+4`; Agents and Artifacts moved
    /// up one digit to make room (decision R3).
    ShowChats,
    ShowAgents,
    /// The Artifacts column: what the delegated runs of this workspace
    /// produced, and what they left that is not a result yet (P1.13).
    ShowArtifacts,
    /// The Canvases section: every canvas in this workspace, one flat list.
    /// The seventh icon, so `Alt+7` (CANVAS P1.3).
    ShowCanvases,
    /// The Socials navigator: drafts, channel queues and the publishing
    /// schedule. The eighth icon, so `Alt+8` (after Canvases took `Alt+7`).
    ShowSocials,
    ToggleContext,
    ToggleAiPane,
    Settings,
    ManageWorkspaces,
    /// The workspaces list in the explore header. **The one `Ctrl+Alt+letter`
    /// binding outside the headings**, and it works only because the shell
    /// rebuilds the letter from its physical key — see `alt_letter` there.
    SwitchWorkspace,
    /// The profiles list shared by the Explore header and global search.
    SwitchProfile,
    /// Step the context column to its next section, opening the column if it
    /// is shut. **The only way to reach a section from the keyboard**: the
    /// column's tabs are icons in a row and nothing else focuses them, so a
    /// person who cannot use a pointer could not reach Cloud sync at all.
    NextContextSection,
    /// Put the context column's Cloud sync facts on the clipboard. Bound so
    /// the action a mouse has as a button is not mouse-only, and so a person
    /// can paste what they are looking at into a message without reaching for
    /// the pointer at all — which is the whole point of that tab.
    CopyCloudDetails,
    /// Move keyboard focus from the active content region into window chrome.
    FocusChrome,
    /// **Record this window (internal builds only).** F9 opens the recording
    /// modal — every start asks for a size and a destination — and stops the
    /// active recording. Bound by the container because the surface is the
    /// container's own modal; gated `internal && windows` at the resolver, so
    /// a release build never resolves it.
    ToggleRecording,
    /// **Hand the work over, from inside the delegation modal.**
    ///
    /// The container's, because the modal is the container's: the four
    /// gestures that open it are the tab strip's `+`, the editor's slash menu,
    /// a task's menu and a document selection's menu, and the surface stands
    /// over all of them. Enter inside the objective inserts a newline, the way
    /// it does in every other multi-line field, so a chord is what commits.
    ///
    /// It is the same chord as the editor's [`ToggleDone`](Self::ToggleDone)
    /// and says so rather than hiding it: the modal blocks the editor while it
    /// stands, so the two can never both be listening.
    Delegate,
    /// **The reminders on the item the caret is in** — open the one already
    /// saved, or start a new one when there is none.
    ///
    /// It exists because reminder behaviour that already shipped had no
    /// keyboard route at all: a reminder pill hanging off a row is reached by
    /// pressing it, and `press` is the only caller that ever asked for one
    /// (`koru_editor::editor`'s `wants_reminder_management`). Creating one
    /// has the `/remind` command; *revisiting a saved one* had nothing, which
    /// is the half of A3 the accessibility roadmap gates on. This is that
    /// route, and it performs the same request the press does.
    ///
    /// **The container, not the editor**, for `SaveSnapshot`'s reason: a
    /// reminder is a fact about the *item* rather than about the text under
    /// the caret, and the surface it opens is the container's own popover.
    /// `Ctrl+Shift+R` because the plain form is the editor's Align right and
    /// the shifted form is bound at no layer.
    Reminders,
    // **There is deliberately no `TrashDocument`.** Trashing is offered by the
    // explore pane's context menu and by nothing else; the container binds no
    // chord for it, and `move_to_trash_has_no_keyboard_shortcut_and_fits_its_label`
    // pins that. An id here is a promise that some layer resolves a chord, so an
    // action with no binding does not get one — see the note on `REGISTRY`.
    RenameDocument,
    /// Ask to save a named snapshot of the document showing.
    ///
    /// **The container, not the editor**, because a snapshot is a fact about
    /// the *document in this tab* rather than about the text under the caret —
    /// and because the tabs that are not documents (Welcome, Trash, the
    /// workspace manager) have to be able to answer it with nothing at all.
    /// The plain primary `S` is free at every layer: the editor binds only its
    /// shifted form, which is Strikethrough.
    SaveSnapshot,
    /// Open the **Move to…** picker. Resolved from the **physical** `KeyM` in
    /// the shell: on Windows Ctrl+M is the same ASCII byte as Enter (`0x0D`),
    /// so a logical-key binding would steal Ctrl+Enter.
    MoveDocument,
    // --------------------------------------------------------------- the view
    ViewModePage,
    ViewModeNarrow,
    ViewModeFull,
    SummonBubble,
    /// A standalone document window has no tabs, so the primary `W` closes the
    /// window instead. Never reached inside the container, which claims that
    /// chord for *Close tab* one layer earlier.
    CloseDocumentWindow,
    // ------------------------------------------------------------- the editor
    Bold,
    Italic,
    Underline,
    Strikethrough,
    InlineCode,
    CodeBlock,
    CycleHighlight,
    AlignLeft,
    AlignCentre,
    AlignRight,
    BodyText,
    Heading1,
    Heading2,
    Heading3,
    Heading4,
    BulletedList,
    NumberedList,
    Checklist,
    ClearFormatting,
    Superscript,
    Subscript,
    ToggleDone,
    /// **Tick or untick the box the focused row draws** — the same
    /// `Body::set_done` a press on the box performs, and nothing else.
    ///
    /// It exists because that box had no plain-keyboard route at all. A press
    /// reaches it and — since the accessibility projection published the row's
    /// `#check` — so does an assistive technology, but a person using the
    /// keyboard with no screen reader could not tick a checklist item:
    /// `/completed` is offered only where `allows.task` holds, so a plain
    /// checklist row never gets it. That was the keyboard half of the
    /// accessibility roadmap's A3 gate.
    ///
    /// **`docs/features/tasks/SPEC.md` §11 defers keyboard chords for task actions and
    /// the maintainers overrode it for this one action** (2026-09-11, Q-T2), because
    /// M2's exit is "keyboard *and* each screen reader" and the keyboard half
    /// could not otherwise pass.
    ///
    /// **The box, never the lifecycle.** *Completing* a task outright is the
    /// row menu's explicit Complete, which cascades through everything
    /// checkable below it; this is `Body::set_done`, which on a rich task
    /// moves the box and nothing else (SPEC §5, ADR-345). So it does the same
    /// thing on every row that draws a box — and nothing at all on a row that
    /// draws none.
    ///
    /// `PRIMARY`+Shift+Enter because the unshifted form is the *done* chord
    /// this app already has ([`ToggleDone`](Self::ToggleDone)), and a shifted
    /// sibling is how the rest of this table spells "the other half of that
    /// key" — `Ctrl+S`/`Ctrl+Shift+S`, `Ctrl+V`/`Ctrl+Shift+V`,
    /// `Ctrl+E`/`Ctrl+Shift+E`. Enter under `PRIMARY_SHIFT` is bound at no
    /// layer: the editor's Enter family is plain (split), Shift (split above),
    /// Alt (paragraph above) and primary, and no layer above the editor claims
    /// Enter at all.
    ///
    /// **The Windows trap it inherits**, written down rather than
    /// rediscovered: `Ctrl+M` is the same ASCII byte as Enter there — which is
    /// why [`MoveDocument`](Self::MoveDocument) dispatches off a physical key —
    /// so `Ctrl+Shift+M` reaches this chord too. Nothing binds
    /// `Ctrl+Shift+M`, so it costs nothing today.
    TickCheckbox,
    /// Save the focused diagram's picture as a PNG or an SVG — the menu's
    /// *Save as…* on a drawn `mermaid` block, from the keyboard.
    ///
    /// **Applies only on a drawn diagram**, and the editor's handler enforces
    /// it the way [`TickCheckbox`](Self::TickCheckbox) does: on a paragraph,
    /// on a diagram whose text is open, or on one that does not draw, the
    /// handler answers `false` and the chord falls through unclaimed. The
    /// registry has no notion of a selection scope, so this is the only place
    /// the rule can live; the menu row is the other half, offered only when
    /// there is a drawing to save.
    ///
    /// `PRIMARY`+Shift+X: the shifted sibling of Cut, free on both platforms
    /// — Ctrl/⌘+S and Ctrl/⌘+Shift+S are the document's saves, and the
    /// collision test below keeps it that way. It exists because a document
    /// block's menu opens only from a pointer today; without this a keyboard
    /// user could reach every other step of a diagram's life but not its
    /// file.
    SaveDiagramAs,
    InsertLink,
    EmojiPicker,
    DeleteRows,
    Copy,
    Cut,
    Paste,
    PastePlain,
    Undo,
    Redo,
    SelectAll,
    Indent,
    Outdent,
    MoveRowUp,
    MoveRowDown,
}

/// One row of the table.
pub struct Shortcut {
    pub id: ShortcutId,
    pub layer: Layer,
    /// What a menu row calls it. Sentence case, no chord in it — the chord is
    /// rendered from [`Shortcut::chord`] so the two cannot disagree.
    pub label: &'static str,
    pub chord: Chord,
    /// A chord this deliberately shares with an entry in another layer.
    ///
    /// The only honest way to record the one real overlap in the app: the
    /// container's *Close tab* and a standalone document window's *Close
    /// window* are both the primary modifier and `W`. They never coexist —
    /// a window with tabs has no document-level binding and vice versa — so the
    /// collision test needs to be told, rather than the fact being lost.
    pub shares_chord_with: Option<ShortcutId>,
    /// **A developer-only binding**: it exists in internal builds and is left
    /// out of the generated `SHORTCUTS.md`, which is a user guide — a chord
    /// the reader's build does not have is a promise the doc cannot keep
    /// (SESSIONRECORD: F9, and retroactively the F11/F12 inspector chords).
    pub internal_only: bool,
}

const fn s(id: ShortcutId, layer: Layer, label: &'static str, chord: Chord) -> Shortcut {
    Shortcut {
        id,
        layer,
        label,
        chord,
        shares_chord_with: None,
        internal_only: false,
    }
}

/// An [`internal_only`](Shortcut::internal_only) row: bound for developers,
/// documented nowhere a user reads.
const fn s_internal(id: ShortcutId, layer: Layer, label: &'static str, chord: Chord) -> Shortcut {
    Shortcut {
        id,
        layer,
        label,
        chord,
        shares_chord_with: None,
        internal_only: true,
    }
}

/// **Tab cycling is the literal Control key on every platform.**
///
/// ⌘+Tab is the macOS application switcher and is not ours to take; Ctrl+Tab is
/// what a Mac app uses to cycle its own tabs. This is the one entry that does
/// not follow [`PRIMARY`].
const CTRL: winit::keyboard::ModifiersState = winit::keyboard::ModifiersState::CONTROL;
const CTRL_SHIFT: winit::keyboard::ModifiersState =
    winit::keyboard::ModifiersState::from_bits_truncate(CTRL.bits() | SHIFT.bits());

/// **Back and forward differ by platform, and cannot not.**
///
/// Elsewhere it is `Alt`+←/→, the browser and VS Code convention. On macOS `⌥←`
/// is word-left in every text field on the system, and this is resolved at the
/// shell — before any view sees the key — so claiming it would take word motion
/// away from the editor wholesale. `⌘[` / `⌘]` is what Finder, Safari and Xcode
/// use, and it is free here because the editor binds neither bracket.
const ALT: winit::keyboard::ModifiersState = winit::keyboard::ModifiersState::ALT;
const HISTORY_BACK: Chord = if cfg!(target_os = "macos") {
    Chord::new(On::Letter('['), PRIMARY)
} else {
    Chord::new(named(NamedKey::ArrowLeft), ALT)
};
const HISTORY_FORWARD: Chord = if cfg!(target_os = "macos") {
    Chord::new(On::Letter(']'), PRIMARY)
} else {
    Chord::new(named(NamedKey::ArrowRight), ALT)
};

/// **Push to talk, and why it is not on Space.**
///
/// The obvious chord for dictation is the primary modifier, Shift and Space,
/// and it is wrong twice over.
///
/// It is already taken *here*: `PRIMARY_SHIFT`+Space is the editor's emoji
/// picker, so `no_shortcut_shadows_a_lower_layer` refuses it — which is the
/// collision test doing exactly its job.
///
/// And it is taken *outside* here, by the audience this feature is for. A
/// modifier-plus-Space is what nearly every input method uses to switch itself
/// on and off: `Ctrl+Space` is Microsoft Pinyin's Chinese/English toggle and
/// an offered assignment in the Japanese MS-IME, it is fcitx5's default
/// trigger on Linux, and on macOS `⌃Space` and `⌃⌥Space` are *Select the
/// previous input source* and *Select next source in Input menu*. Somebody
/// typing Chinese presses it reflexively; a hold whose Shift arrives a moment
/// late would switch their language instead of dictating. The registry's
/// collision test sees only Koru's own bindings, so nothing in this file could
/// have caught that — it is written down instead (Q-V58).
///
/// `Ctrl+Alt` was the other candidate and is worse: **AltGr is Ctrl+Alt on
/// Windows**, which is why `Ctrl+Alt+letter` arrives here as
/// `Key::Unidentified` and has to be rebuilt from its physical key. A bare
/// function key is worse again — `F5` is reload everywhere and is the *mic*
/// key on a Mac, so it would collide with the platform's own dictation.
///
/// `PRIMARY`+Shift+`D` is free on both counts. `PRIMARY`+`D` alone is the
/// editor's *Delete line*, which is why this one carries Shift; and Shift+`D`
/// avoids the other trap on Windows, where a **modifier-only** `Ctrl`+`Shift`
/// press is the layout-switch hotkey — pressing a letter in between is what
/// stops that from firing.
const DICTATE: Chord = Chord::new(On::Letter('d'), PRIMARY_SHIFT);

/// Every shortcut, grouped by the layer that resolves it.
pub const REGISTRY: &[Shortcut] = &[
    s_internal(
        ShortcutId::ToggleInspectorSpacingRays,
        Layer::Shell,
        "Inspector spacing rays",
        Chord::new(named(NamedKey::F11), NONE),
    ),
    s_internal(
        ShortcutId::ToggleUiInspector,
        Layer::Shell,
        "UI inspector",
        Chord::new(named(NamedKey::F12), NONE),
    ),
    // ------------------------------------------------------------- the shell
    s(
        ShortcutId::NewWindow,
        Layer::Shell,
        "New window",
        Chord::new(On::Letter('n'), PRIMARY),
    ),
    s(
        ShortcutId::PinWindow,
        Layer::Shell,
        "Keep window on top",
        Chord::new(On::Letter('t'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ZoomIn,
        Layer::Shell,
        "Zoom in",
        Chord::new(On::Letter('='), PRIMARY),
    ),
    s(
        ShortcutId::ZoomOut,
        Layer::Shell,
        "Zoom out",
        Chord::new(On::Letter('-'), PRIMARY),
    ),
    s(
        ShortcutId::ZoomReset,
        Layer::Shell,
        "Actual size",
        Chord::new(On::Letter('0'), PRIMARY),
    ),
    s(
        ShortcutId::NextTab,
        Layer::Shell,
        "Next tab",
        Chord::new(named(NamedKey::Tab), CTRL),
    ),
    s(
        ShortcutId::PreviousTab,
        Layer::Shell,
        "Previous tab",
        Chord::new(named(NamedKey::Tab), CTRL_SHIFT),
    ),
    s(ShortcutId::HistoryBack, Layer::Shell, "Back", HISTORY_BACK),
    s(
        ShortcutId::HistoryForward,
        Layer::Shell,
        "Forward",
        HISTORY_FORWARD,
    ),
    s(ShortcutId::Dictate, Layer::Shell, "Dictate", DICTATE),
    // ---------------------------------------------------------- the terminal
    s(
        ShortcutId::TerminalCopyOrInterrupt,
        Layer::Terminal,
        "Copy or interrupt",
        Chord::new(On::Letter('c'), PRIMARY),
    ),
    s(
        ShortcutId::TerminalCopySelection,
        Layer::Terminal,
        "Copy selection",
        Chord::new(On::Letter('c'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::TerminalPaste,
        Layer::Terminal,
        "Paste",
        Chord::new(On::Letter('v'), PRIMARY),
    ),
    s(
        ShortcutId::TerminalPasteAlternate,
        Layer::Terminal,
        "Paste",
        Chord::new(On::Letter('v'), PRIMARY_SHIFT),
    ),
    // The one the tab lane brought with it (SP-10). It shares its chord with
    // the container's *Settings* on purpose — `layers_can_coexist` already
    // knows the Terminal and the container are different binaries, so neither
    // shadows the other — and it is a row of its own rather than a reuse
    // because a shortcut sheet in a terminal has to print the terminal's rows.
    s(
        ShortcutId::TerminalSettings,
        Layer::Terminal,
        "Settings",
        Chord::new(On::Letter(','), PRIMARY),
    ),
    // **The same chord as the container's `ToggleExplore`, on purpose.** It is
    // the same pane wearing the same name in a second binary, and
    // `layers_can_coexist` knows the two never run in one window — so this is a
    // row of its own rather than a reuse, because a shortcut sheet printed
    // inside the Terminal has to print the Terminal's rows.
    s(
        ShortcutId::TerminalToggleExplore,
        Layer::Terminal,
        "Explore",
        Chord::new(On::Letter('e'), PRIMARY_SHIFT),
    ),
    // ---------------------------------------------------------- the container
    s(
        ShortcutId::NewTab,
        Layer::Container,
        "New tab",
        Chord::new(On::Letter('t'), PRIMARY),
    ),
    s(
        ShortcutId::AddContent,
        Layer::Container,
        "Add…",
        Chord::new(On::Letter('a'), PRIMARY_SHIFT),
    ),
    Shortcut {
        shares_chord_with: Some(ShortcutId::CloseDocumentWindow),
        ..s(
            ShortcutId::CloseTab,
            Layer::Container,
            "Close tab",
            Chord::new(On::Letter('w'), PRIMARY),
        )
    },
    s(
        ShortcutId::CloseAllTabs,
        Layer::Container,
        "Close all tabs",
        Chord::new(On::Letter('w'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::Search,
        Layer::Container,
        "Search",
        Chord::new(On::Letter('p'), PRIMARY),
    ),
    s(
        ShortcutId::ToggleExplore,
        Layer::Container,
        "Explore",
        Chord::new(On::Letter('e'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ShowDocuments,
        Layer::Container,
        "Documents",
        Chord::new(On::Letter('1'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowFiles,
        Layer::Container,
        "Files",
        Chord::new(On::Letter('2'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowTasks,
        Layer::Container,
        "Tasks",
        Chord::new(On::Letter('3'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowChats,
        Layer::Container,
        "Chats",
        Chord::new(On::Letter('4'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowAgents,
        Layer::Container,
        "Agents",
        Chord::new(On::Letter('5'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowArtifacts,
        Layer::Container,
        "Artifacts",
        Chord::new(On::Letter('6'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowCanvases,
        Layer::Container,
        "Canvases",
        Chord::new(On::Letter('7'), crate::VIEW),
    ),
    s(
        ShortcutId::ShowSocials,
        Layer::Container,
        "Socials",
        Chord::new(On::Letter('8'), crate::VIEW),
    ),
    s(
        ShortcutId::ToggleContext,
        Layer::Container,
        "Context",
        Chord::new(On::Letter('j'), PRIMARY),
    ),
    s(
        ShortcutId::ToggleAiPane,
        Layer::Container,
        "Koru AI pane",
        Chord::new(On::Letter('j'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::Settings,
        Layer::Container,
        "Settings",
        Chord::new(On::Letter(','), PRIMARY),
    ),
    s(
        ShortcutId::ManageWorkspaces,
        Layer::Container,
        "Manage workspaces",
        Chord::new(On::Letter('o'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::SwitchWorkspace,
        Layer::Container,
        "Switch workspace",
        Chord::new(On::Letter('w'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::SwitchProfile,
        Layer::Container,
        "Switch profile",
        Chord::new(On::Letter('p'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::NextContextSection,
        Layer::Container,
        "Next context section",
        Chord::new(On::Letter('j'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::CopyCloudDetails,
        Layer::Container,
        "Copy Cloud sync details",
        Chord::new(On::Letter('k'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::RenameDocument,
        Layer::Container,
        "Rename",
        Chord::new(named(NamedKey::F2), NONE),
    ),
    s(
        ShortcutId::FocusChrome,
        Layer::Container,
        "Focus window controls",
        Chord::new(named(NamedKey::F6), NONE),
    ),
    s_internal(
        ShortcutId::ToggleRecording,
        Layer::Container,
        "Record this window",
        Chord::new(named(NamedKey::F9), NONE),
    ),
    s(
        ShortcutId::Reminders,
        Layer::Container,
        "Reminders",
        Chord::new(On::Letter('r'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::SaveSnapshot,
        Layer::Container,
        "Save or rename a snapshot",
        Chord::new(On::Letter('s'), PRIMARY),
    ),
    s(
        ShortcutId::MoveDocument,
        Layer::Container,
        "Move to\u{2026}",
        // Label only: dispatch is physical `KeyM` in the shell (see the id).
        Chord::new(On::Letter('m'), PRIMARY),
    ),
    Shortcut {
        // The editor's *Mark done* is the same chord. They never coexist:
        // the delegation modal is a blocker, so while it stands nothing
        // reaches the editor underneath it.
        shares_chord_with: Some(ShortcutId::ToggleDone),
        ..s(
            ShortcutId::Delegate,
            Layer::Container,
            "Delegate the work",
            Chord::new(named(NamedKey::Enter), PRIMARY),
        )
    },
    // --------------------------------------------------------------- the view
    s(
        ShortcutId::ViewModePage,
        Layer::View,
        "Page width",
        Chord::new(On::Letter('p'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ViewModeNarrow,
        Layer::View,
        "Narrow width",
        Chord::new(On::Letter('n'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ViewModeFull,
        Layer::View,
        "Full width",
        Chord::new(On::Letter('g'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::SummonBubble,
        Layer::View,
        "Formatting bubble",
        Chord::new(On::Letter('.'), PRIMARY),
    ),
    Shortcut {
        shares_chord_with: Some(ShortcutId::CloseTab),
        ..s(
            ShortcutId::CloseDocumentWindow,
            Layer::View,
            "Close window",
            Chord::new(On::Letter('w'), PRIMARY),
        )
    },
    // ------------------------------------------------------------- the editor
    s(
        ShortcutId::Bold,
        Layer::Editor,
        "Bold",
        Chord::new(On::Letter('b'), PRIMARY),
    ),
    s(
        ShortcutId::Italic,
        Layer::Editor,
        "Italic",
        Chord::new(On::Letter('i'), PRIMARY),
    ),
    s(
        ShortcutId::Underline,
        Layer::Editor,
        "Underline",
        Chord::new(On::Letter('u'), PRIMARY),
    ),
    s(
        ShortcutId::Strikethrough,
        Layer::Editor,
        "Strikethrough",
        Chord::new(On::Letter('s'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::InlineCode,
        Layer::Editor,
        "Code",
        Chord::new(On::Letter('c'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::CodeBlock,
        Layer::Editor,
        "Code block",
        Chord::new(On::Letter('`'), PRIMARY),
    ),
    s(
        ShortcutId::CycleHighlight,
        Layer::Editor,
        "Highlight",
        Chord::new(On::Letter('h'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::AlignLeft,
        Layer::Editor,
        "Align left",
        Chord::new(On::Letter('l'), PRIMARY),
    ),
    s(
        ShortcutId::AlignCentre,
        Layer::Editor,
        "Align centre",
        Chord::new(On::Letter('e'), PRIMARY),
    ),
    s(
        ShortcutId::AlignRight,
        Layer::Editor,
        "Align right",
        Chord::new(On::Letter('r'), PRIMARY),
    ),
    s(
        ShortcutId::BodyText,
        Layer::Editor,
        "Body text",
        Chord::new(On::Letter('0'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::Heading1,
        Layer::Editor,
        "Heading 1",
        Chord::new(On::Letter('1'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::Heading2,
        Layer::Editor,
        "Heading 2",
        Chord::new(On::Letter('2'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::Heading3,
        Layer::Editor,
        "Heading 3",
        Chord::new(On::Letter('3'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::Heading4,
        Layer::Editor,
        "Heading 4",
        Chord::new(On::Letter('4'), PRIMARY_ALT),
    ),
    s(
        ShortcutId::BulletedList,
        Layer::Editor,
        "Bulleted list",
        Chord::new(On::Letter('8'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::NumberedList,
        Layer::Editor,
        "Numbered list",
        Chord::new(On::Letter('7'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::Checklist,
        Layer::Editor,
        "Checklist",
        Chord::new(On::Letter('9'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ClearFormatting,
        Layer::Editor,
        "Clear formatting",
        Chord::new(On::Letter('\\'), PRIMARY),
    ),
    s(
        ShortcutId::Superscript,
        Layer::Editor,
        "Superscript",
        Chord::new(On::Letter('='), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::Subscript,
        Layer::Editor,
        "Subscript",
        Chord::new(On::Letter('-'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::ToggleDone,
        Layer::Editor,
        "Mark done",
        Chord::new(named(NamedKey::Enter), PRIMARY),
    ),
    s(
        ShortcutId::TickCheckbox,
        Layer::Editor,
        "Tick checkbox",
        Chord::new(named(NamedKey::Enter), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::SaveDiagramAs,
        Layer::Editor,
        "Save diagram as…",
        Chord::new(On::Letter('x'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::InsertLink,
        Layer::Editor,
        "Link",
        Chord::new(On::Letter('k'), PRIMARY),
    ),
    s(
        ShortcutId::EmojiPicker,
        Layer::Editor,
        "Emoji",
        Chord::new(named(NamedKey::Space), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::DeleteRows,
        Layer::Editor,
        "Delete line (children outdent)",
        Chord::new(On::Letter('d'), PRIMARY),
    ),
    s(
        ShortcutId::Copy,
        Layer::Editor,
        "Copy",
        Chord::new(On::Letter('c'), PRIMARY),
    ),
    s(
        ShortcutId::Cut,
        Layer::Editor,
        "Cut",
        Chord::new(On::Letter('x'), PRIMARY),
    ),
    s(
        ShortcutId::Paste,
        Layer::Editor,
        "Paste",
        Chord::new(On::Letter('v'), PRIMARY),
    ),
    // The document's right-click menu offers this beside Paste, and a row that
    // said `Ctrl+Shift+V` on a Mac would be the exact bug this registry exists
    // to stop — so it is an entry rather than a literal at the call site.
    s(
        ShortcutId::PastePlain,
        Layer::Editor,
        "Paste as plain text",
        Chord::new(On::Letter('v'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::Undo,
        Layer::Editor,
        "Undo",
        Chord::new(On::Letter('z'), PRIMARY),
    ),
    s(
        ShortcutId::Redo,
        Layer::Editor,
        "Redo",
        Chord::new(On::Letter('z'), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::SelectAll,
        Layer::Editor,
        "Select all",
        Chord::new(On::Letter('a'), PRIMARY),
    ),
    s(
        ShortcutId::Indent,
        Layer::Editor,
        "Indent",
        Chord::new(named(NamedKey::Tab), NONE),
    ),
    s(
        ShortcutId::Outdent,
        Layer::Editor,
        "Outdent",
        Chord::new(named(NamedKey::Tab), SHIFT),
    ),
    s(
        ShortcutId::MoveRowUp,
        Layer::Editor,
        "Move line up",
        Chord::new(named(NamedKey::ArrowUp), PRIMARY_SHIFT),
    ),
    s(
        ShortcutId::MoveRowDown,
        Layer::Editor,
        "Move line down",
        Chord::new(named(NamedKey::ArrowDown), PRIMARY_SHIFT),
    ),
];

impl ShortcutId {
    /// Whether this build shows the surface the chord belongs to
    /// (`koru_features`, guide/contributing/FEATURE-FLAGS.md).
    ///
    /// **The row stays in [`REGISTRY`]** — it is still the chord's one home,
    /// `SHORTCUTS.md` is still generated from the whole table, and
    /// `no_shortcut_shadows_a_lower_layer` still sees it. What a hidden row
    /// loses is being *advertised*: [`crate::show_hint`] answers `None` for
    /// it. Each resolver refuses to resolve it beside its own match (the
    /// container's `mainview::keys::resolve`).
    pub fn shown_in_product(self) -> bool {
        match self {
            Self::ShowTasks => koru_features::tasks(),
            Self::ShowChats | Self::ToggleAiPane | Self::Delegate => koru_features::ai(),
            Self::ShowAgents | Self::ShowArtifacts => koru_features::agent_views(),
            Self::CopyCloudDetails => koru_features::cloud_sync(),
            Self::Reminders => koru_features::reminders(),
            Self::ShowCanvases => koru_features::canvas(),
            Self::ShowSocials => koru_features::socials(),
            _ => true,
        }
    }
}

/// The entry for `id`. Panics if the table has no such row, which can only be a
/// missing arm rather than a runtime condition.
pub fn get(id: ShortcutId) -> &'static Shortcut {
    REGISTRY
        .iter()
        .find(|s| s.id == id)
        .expect("every ShortcutId is in REGISTRY — pinned by a test")
}

/// How `id` is written on this platform. What a menu row shows.
pub fn label_of(id: ShortcutId) -> String {
    crate::format::label(&get(id).chord)
}

/// Every entry a given layer owns, in table order.
pub fn of_layer(layer: Layer) -> impl Iterator<Item = &'static Shortcut> {
    REGISTRY.iter().filter(move |s| s.layer == layer)
}

/// The whole table as Markdown, one section per layer.
///
/// Used to generate `reference/SHORTCUTS.md`, so the documented chord and the bound
/// chord are the same value rather than two claims that agree today.
pub fn markdown() -> String {
    let mut out = String::from(
        "# Keyboard shortcuts\n\n\
         <!-- Generated from `crates/koru-keys/src/registry.rs`. Do not edit by hand:\n\
              `cargo test -p koru-keys` fails when this file and the table disagree,\n\
              and `KORU_UPDATE_SHORTCUTS=1 cargo test -p koru-keys` rewrites it. -->\n\n\
         Chords are shown as this platform writes them.\n",
    );
    for layer in [
        Layer::Shell,
        Layer::Terminal,
        Layer::Container,
        Layer::View,
        Layer::Editor,
    ] {
        out.push_str(&format!("\n## {}\n\n| | |\n| --- | --- |\n", layer.name()));
        for entry in of_layer(layer) {
            // A user guide documents the build a user runs: developer chords
            // (F9, F11, F12) are bound for internal builds and omitted here.
            if entry.internal_only {
                continue;
            }
            out.push_str(&format!(
                "| `{}` | {} |\n",
                crate::format::label(&entry.chord),
                entry.label
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests;
