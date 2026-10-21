// SPDX-License-Identifier: MPL-2.0

//! The built-in document: prose over a flat body, with a formatting toolbar.
//!
//! It **owns** an [`Editor`] — the whole component, embedded — where the task
//! outline instead holds `body` and `view` itself. That asymmetry is the point:
//! a document is heading for block kinds an outline will never have, so it takes
//! the batteries-included piece and grows around it.
//!
//! Everything shared — typing, selection, marks, pills, undo — reaches it
//! through [`Editing`]. What is here is the furniture, and the two keystrokes
//! that mean something different in prose than they do in an outline.

use std::time::Instant;

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use koru_editor::Body;
use koru_editor::{Editing, Editor, OpenWhere};

pub mod blockmenu;
pub mod bubble;
pub mod caption;
pub mod context_menu;
pub mod diagrams;
pub mod docview;
pub mod draw;
pub mod menu;
pub mod page_menu;
pub mod search;
pub mod spelling;
mod structure;
mod view;
pub mod window_menu;

#[cfg(test)]
mod tests;

use caption::Hit as CaptionHit;
use koru_draw::geometry::Rect;
use koru_draw::theme;
use koru_draw::theme::ViewMode;
use koru_draw::tween::Tween;
use koru_editor::ViewState;
use koru_editor::input::action::{Action, Scope};
use koru_editor::input::keymap;
use koru_ui::caption::Action as CaptionAction;
use koru_window::{WindowRequest, snap};
use koru_workspace_host::DocViewPrefs;
use koru_workspace_host::Settings;
use koru_workspace_host::routing::{Blockers, EditableTarget, FieldId, ModalKind, TabId};

pub struct BuiltInDocument {
    pub editor: Editor,
    /// Whether this document *is* the window, and so draws its own title bar.
    ///
    /// False inside a container tab, where the caption above belongs to the
    /// container and a second one would be both wrong and unusable — the tab
    /// does not reach the window's edge. Set by [`owns_the_window`], which the
    /// single-view binary calls and the container does not.
    ///
    /// [`owns_the_window`]: Self::owns_the_window
    window_chrome: bool,
    /// Whether the window fills the screen, pushed by the shell. The maximise
    /// button has to become a restore button, and a view has no `Window` to ask.
    maximized: bool,
    /// Whether the window is pinned above the others, pushed by the shell. The
    /// caption's pin button draws pressed (in the accent colour) when it is.
    pinned: bool,
    /// Whether this window is reaching the store, and what it is owed if not.
    ///
    /// **Pushed every tick, like the container's**, because part of what it
    /// reports moves without anything happening. A popped-out document shares
    /// the container's workspace, so this is the same fact arriving at a second
    /// caption rather than a second fact.
    store_trouble: Option<koru_workspace::StoreTrouble>,
    /// Whether the Offline chip's popover is showing.
    offline_open: bool,
    /// How far through turning from a ⋮ button into an ✕ the caption's window
    /// menu button is: 0 a ⋮ button, 1 an ✕.
    ///
    /// Follows whether the window menu is open, but *lagging* it — the menu
    /// appears at once and the glyph catches up. Kept beside the window state
    /// rather than on the menu because it has to keep turning after the menu is
    /// gone.
    menu_morph: Tween,
    /// The document's name, for the title bar — **cached**, not read per frame.
    ///
    /// A standalone window turns `Body::title_row` off, so the name is no longer
    /// a row this view can look at; it has to come from the document. Asking
    /// `ws.read` once a frame is the shape of cost the responsiveness rules
    /// exist to keep out of a draw, so it is refreshed on the events a rename
    /// can ride in on instead: `adopt` for a peer's, and [`key`](Self::key) or
    /// [`mouse_down`](Self::mouse_down) for one made here.
    title: String,
    /// How many times [`refresh_title`](Self::refresh_title) has been called.
    ///
    /// The third of the three pieces of work this view's `adopt` arranges — with
    /// the search re-scan and the spelling observation — and all three are
    /// idempotent, so running one twice for a single peer edit is invisible in
    /// the state it produces. Q-B4's fix reconciled the container's adopt path
    /// with this one and had to leave all three counts alone; that claim is
    /// asserted from these counters rather than read out of the code.
    title_refreshes: u64,
    /// A window-level action the caption asked for, drained by the shell inside
    /// the same press — `drag_window` only works while the button that asked for
    /// it is still down.
    window_request: Option<WindowRequest>,
    /// Where the bubble is, and the selection it was placed for.
    ///
    /// **Pinned once it appears.** Its rect is measured from the selection's
    /// glyphs, and pressing Bold re-shapes them — so a bubble that re-measured
    /// every frame slid sideways out from under the pointer that had just
    /// pressed it. The selection has not changed, so it keeps the place it was
    /// given; a *different* range gets a fresh one.
    ///
    /// Recomputed once per frame by [`place_bubble`](Self::place_bubble), which
    /// is what lets [`bubble_panel`](Self::bubble_panel) stay `&self` and be
    /// asked by the click path and the cursor alike.
    bubble: Option<(NodeId, usize, usize, Rect)>,
    /// The row `Ctrl+.` summoned the bubble on, if it was summoned.
    ///
    /// The bubble is otherwise a consequence of selecting text, which leaves
    /// the block dropdown and the three alignments — none of which act on a
    /// *range* — reachable only with a mouse. The chord makes them reachable
    /// without one, and every inline button still works: `Paragraph::toggle_mark`
    /// arms an empty range for the next keystroke rather than refusing it, so
    /// pressing Bold here does exactly what `Ctrl+B` at the same caret does.
    ///
    /// It holds the *row* rather than a flag so moving the caret elsewhere
    /// retires it, which is the same rule the selection follows.
    bubble_pinned: Option<NodeId>,
    /// The bubble button under the pointer, and when it got there — its
    /// tooltip's clock, matching the `+`'s. A bubble button is not a `Hover`,
    /// so it cannot share `ViewState::hover_since`.
    bubble_hover: Option<(bubble::Button, Instant)>,
    /// The one menu that may be open: the page settings behind the title's
    /// `…`, or one of the two the selection bubble opens.
    ///
    /// One field rather than three `Option`s, for the reason `ViewState` keeps
    /// a single `Overlay`: at most one may ever be showing, and three fields
    /// make seven states of which four are impossible.
    menu: Option<OpenMenu>,
    /// Per-tab local find/replace state, including the retained query/options.
    search: search::LocalSearch,
    /// Document pill the open context menu was requested on, if any.
    context_document_pill: Option<(usize, koru_core::AttrId)>,
    /// Attribute chip the open context menu was requested on: its row, and the
    /// hit itself.
    ///
    /// **The whole hit, resolved once**, for the reason the attachment pill
    /// keeps five fields: the menu's two rows both act on this chip, and
    /// re-hit-testing when one is chosen would ask about wherever the pointer
    /// has drifted to since. `ChipHit` carries the spec, which is what says
    /// *which* attribute, and the rectangle the picker hangs off.
    context_attribute_chip: Option<(usize, koru_editor::chips::ChipHit)>,
    /// Row, attr, file id, trashed, dangling — resolved when the menu opens so
    /// actions do not re-resolve and miss. `dangling` means permanently gone.
    context_attachment_pill: Option<(usize, koru_core::AttrId, koru_core::DocumentId, bool, bool)>,
    /// Exact object under the right-click that opened the context menu.
    /// Reminder creation is object-scoped, so it must not fall back to the
    /// focus end of a preserved multi-row selection.
    context_reminder_node: Option<NodeId>,
    /// Exact inline reminder pill under the right-click, if there was one.
    /// Kept separately from `context_reminder_node`: the latter owns the
    /// ordinary row-level Add reminder action.
    context_reminder: Option<(NodeId, koru_editor::reminders::InlineChip)>,
    /// The workspace tag table as last pushed by the shell, kept for the
    /// cheap same-or-not compare in `set_tag_table`.
    tag_table: Vec<koru_core::system::TagEntry>,
    /// The workspace list table, kept for the same compare in
    /// `set_list_table`. A document reads it for one thing: a task's due
    /// chip wears its list's colour, so the accent has to resolve here.
    list_table: Vec<koru_core::system::ListEntry>,
    /// The **Remove from Tasks** question, and the node it is about.
    ///
    /// The node is carried rather than re-derived when the answer comes back:
    /// the confirm is modal, but a peer's edit is not blocked by it, and a
    /// focus resolved twice can be two different rows.
    ///
    /// One `Option` and not a general modal stack: a document view has exactly
    /// one question it can ask, and a second would need a subject enum before
    /// it needed a stack (the container's `ConfirmSubject` is that shape).
    confirm: Option<(koru_ui::Confirm, Confirming)>,
    /// **Where a live dictation is going, decided once.**
    ///
    /// Recorded when the composition opened and never re-derived. Re-running
    /// the routing per partial would let a focus change mid-utterance redirect
    /// the sentence into a surface it did not start in — which is the one way
    /// dictated words end up somewhere nobody was looking.
    dictation: Option<(u64, DictationInto)>,
    /// Document-local spell checking: the issue store, its debounce, and the
    /// line to the shell's spell worker. Rebuilt with the view on `reseat`,
    /// which is what re-subscribes its change tracker to the new document.
    spelling: spelling::SpellCheck,
    /// What the open context menu says about the clicked misspelling, frozen
    /// at open (D13) so the menu never relabels under the pointer. Assigned —
    /// to a snapshot or to `None` — on every context-menu open.
    context_spelling: Option<spelling::SpellSnapshot>,
    /// The line to the shell's diagram worker: the editor's layout requests
    /// go out and the drawings come back through it, once per `maintain`.
    diagrams: diagrams::Diagrams,
}

/// The three pieces of work this view's `adopt` arranges, counted.
///
/// A window-level `adopt` re-titles the caption, re-scans the find bar and
/// shows the document to the spelling controller; a **container** arranges all
/// three for itself, elsewhere and on its own schedule. Q-B4 reconciled the two
/// hosts' handling of an open picker across a peer edit, and the rule it had to
/// respect was that neither host started doing any of these a second time.
///
/// All three are idempotent, so a duplicate leaves no trace in the state they
/// produce and can only be caught by counting. Read with
/// [`BuiltInDocument::adopt_work`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptWork {
    /// Calls to `refresh_title`.
    pub titles: u64,
    /// Calls to `LocalSearch::refresh`.
    pub searches: u64,
    /// Calls to `SpellCheck::observe`.
    pub spellings: u64,
}

/// **What an open confirmation is about.**
///
/// The slot used to hold a bare `NodeId`, because there was one question this
/// view could ask. There are two now — *Remove from Tasks* takes the marker off
/// and leaves the writing, *Delete row* takes the writing — and they arrive
/// through the same modal, so the answer has to say which was asked. A second
/// `Option` field beside the first would have allowed both to be set at once,
/// which is a state the dialog cannot be in.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Confirming {
    /// Stop tracking this node as a task. The paragraph stays.
    RemoveFromTasks(NodeId),
    /// Delete the paragraph. Rows nested under it move up a level.
    DeleteRow(NodeId),
}

/// Which of a document's three text surfaces a dictation is composing into.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DictationInto {
    /// The rows: the body, the title row included.
    Body,
    /// The find bar, the replace bar, or a prompt inside the document.
    Field(FieldId),
}

/// A menu the document has open, and what its rows mean.
#[derive(Clone, Copy, PartialEq, Debug)]
enum OpenMenu {
    /// The page settings, from the `…` in the title's gutter.
    Page(menu::Menu),
    /// What the focused line is, from the bubble's left-hand button.
    Block(menu::Menu),
    /// Alignment and super/subscript, from the bubble's `⋯`.
    More(menu::Menu),
    /// Bulleted, numbered, and checklist styles from the bubble's list button.
    List(menu::Menu),
    /// Pin, minimise, maximise and close, from the caption's window-menu button.
    Window(menu::Menu),
    /// Cut/copy/paste below a right-click, with the formatting bubble above it.
    Context(menu::Menu),
    /// The block-type submenu opened from a right-click toolbar. The toolbar
    /// rect travels with the submenu so replacing the clipboard menu does not
    /// make the surface that opened it disappear.
    ContextBlock { menu: menu::Menu, toolbar: Rect },
    /// The alignment/script submenu opened from a right-click toolbar.
    ContextMore { menu: menu::Menu, toolbar: Rect },
    /// The list-style submenu opened from a right-click toolbar.
    ContextList { menu: menu::Menu, toolbar: Rect },
}

impl OpenMenu {
    fn menu(&self) -> &menu::Menu {
        match self {
            Self::Page(m)
            | Self::Block(m)
            | Self::List(m)
            | Self::More(m)
            | Self::Window(m)
            | Self::Context(m) => m,
            Self::ContextBlock { menu, .. }
            | Self::ContextMore { menu, .. }
            | Self::ContextList { menu, .. } => menu,
        }
    }

    fn menu_mut(&mut self) -> &mut menu::Menu {
        match self {
            Self::Page(m)
            | Self::Block(m)
            | Self::List(m)
            | Self::More(m)
            | Self::Window(m)
            | Self::Context(m) => m,
            Self::ContextBlock { menu, .. }
            | Self::ContextMore { menu, .. }
            | Self::ContextList { menu, .. } => menu,
        }
    }

    fn is_context_surface(self) -> bool {
        matches!(
            self,
            Self::Context(_)
                | Self::ContextBlock { .. }
                | Self::ContextMore { .. }
                | Self::ContextList { .. }
        )
    }
}

impl Editing for BuiltInDocument {
    fn parts(&mut self) -> (&mut Body, &mut ViewState) {
        self.editor.parts()
    }
    fn body(&self) -> &Body {
        &self.editor.body
    }
    fn body_mut(&mut self) -> &mut Body {
        &mut self.editor.body
    }
    fn view(&self) -> &ViewState {
        &self.editor.view
    }
    fn view_mut(&mut self) -> &mut ViewState {
        &mut self.editor.view
    }
}

impl BuiltInDocument {
    /// Reopen on the first document in `ws`'s library, or on nothing.
    ///
    /// What a **workspace switch** leaves this view able to do. The node it was
    /// showing lived in the document that has just been closed, so there is no
    /// id to reopen at — a `NodeId` is only meaningful inside the document that
    /// minted it. Landing on the library's first document is the same thing a
    /// cold start does.
    pub fn first(ws: &WorkspaceDoc) -> Self {
        match ws
            .children(koru_core::schema::Parent::Root)
            .unwrap_or_default()
            .into_iter()
            .find(|&id| {
                ws.read(id)
                    .is_ok_and(|n| n.kind == koru_core::Kind::Document)
            }) {
            Some(doc) => Self::open(ws, doc),
            // An empty library still has to produce a view. It shows nothing,
            // which is honest: there is nothing.
            None => {
                let mut editor = Editor::over(ws);
                editor.body.flat = true;
                editor.view.insert_button = true;
                editor.view.images_enabled = true;
                editor.view.dividers_enabled = true;
                editor.view.quotes_enabled = true;
                editor.view.headings_enabled = true;
                editor.view.task_lists_enabled = true;
                editor.view.code_enabled = true;
                editor.view.tables_enabled = true;
                apply_default_view_prefs(&mut editor);
                without_footer(&mut editor);
                let search = search::LocalSearch::new(&mut editor.view.engine);
                Self {
                    editor,
                    window_chrome: false,
                    maximized: false,
                    pinned: false,
                    store_trouble: None,
                    offline_open: false,
                    menu_morph: Tween::new(theme::GLYPH_MORPH_MS, false),
                    title: String::new(),
                    title_refreshes: 0,
                    window_request: None,
                    bubble: None,
                    bubble_pinned: None,
                    bubble_hover: None,
                    menu: None,
                    search,
                    context_document_pill: None,
                    context_attribute_chip: None,
                    context_attachment_pill: None,
                    context_reminder_node: None,
                    context_reminder: None,
                    tag_table: Vec::new(),
                    list_table: Vec::new(),
                    confirm: None,
                    dictation: None,
                    spelling: spelling::SpellCheck::new(),
                    context_spelling: None,
                    diagrams: diagrams::Diagrams::new(),
                }
            }
        }
    }

    /// Open `doc`: its body, flat, under a toolbar.
    pub fn open(ws: &WorkspaceDoc, doc: NodeId) -> Self {
        // `rooted_at` rather than `over` + `zoom_to`: the latter built a body
        // over the whole outline realm and laid every row of it out through
        // parley, then threw all of it away on the zoom. Once per tab, on the
        // startup path. Prose does not nest, so depth is not drawn (ADR-23) —
        // that is the `flat`.
        let mut editor = Editor::rooted_at(ws, doc, true);
        // The document node's own text is its title, and it is the first row —
        // editable in place rather than a label in a strip you double-click.
        editor.body.title_row = true;
        editor.body.rebuild(ws);
        // The caret opens in the body, not in the name. Opening a document is
        // "carry on writing", and landing in the title would make the first
        // keystroke rename it.
        editor.body.focus = editor.body.first_body_row();
        // A hovered row offers a `+` beside its grip: the block menu, for a
        // block that does not exist yet.
        editor.view.insert_button = true;
        // Images live inside documents; the `/image` command is offered here only.
        editor.view.images_enabled = true;
        // Dividers are prose furniture, so they are offered in documents only.
        editor.view.dividers_enabled = true;
        editor.view.quotes_enabled = true;
        // Prose gets its structure from headings rather than from depth — the
        // other half of ADR-23: prose gets its structure from headings, where a
        // view that nests gets it from depth.
        //
        // A view with no levels leaves `headings_enabled` off, and the chord
        // does nothing there rather than quietly relabelling a row.
        editor.view.headings_enabled = true;
        editor.view.task_lists_enabled = true;
        without_footer(&mut editor);
        // Code blocks belong in prose: the fence and `/code` are offered here.
        editor.view.code_enabled = true;
        editor.view.tables_enabled = true;
        // A fresh document opens in the global default mode (set on the welcome
        // page). A reopened one is then overridden by `View::restore` if it had
        // a per-document choice; `open` runs first, so this is the fallback.
        apply_default_view_prefs(&mut editor);
        let search = search::LocalSearch::new(&mut editor.view.engine);
        Self {
            editor,
            window_chrome: false,
            maximized: false,
            pinned: false,
            store_trouble: None,
            offline_open: false,
            menu_morph: Tween::new(theme::GLYPH_MORPH_MS, false),
            title: String::new(),
            title_refreshes: 0,
            window_request: None,
            bubble: None,
            bubble_pinned: None,
            bubble_hover: None,
            menu: None,
            search,
            context_document_pill: None,
            context_attribute_chip: None,
            context_attachment_pill: None,
            context_reminder_node: None,
            context_reminder: None,
            tag_table: Vec::new(),
            list_table: Vec::new(),
            confirm: None,
            dictation: None,
            spelling: spelling::SpellCheck::new(),
            context_spelling: None,
            diagrams: diagrams::Diagrams::new(),
        }
    }

    /// Open a `Kind::ListBlock` as a non-flat view: its item children nest, the
    /// way an outline does. The same editor and toolbar as a document; only
    /// `flat` differs.
    pub fn open_list(ws: &WorkspaceDoc, node: NodeId) -> Self {
        let mut editor = Editor::over(ws);
        editor.body.flat = false;
        editor.body.zoom_to(ws, Some(node));
        editor.view.images_enabled = true;
        editor.view.headings_enabled = true;
        editor.view.task_lists_enabled = true;
        without_footer(&mut editor);
        let search = search::LocalSearch::new(&mut editor.view.engine);
        Self {
            editor,
            window_chrome: false,
            maximized: false,
            pinned: false,
            store_trouble: None,
            offline_open: false,
            menu_morph: Tween::new(theme::GLYPH_MORPH_MS, false),
            title: String::new(),
            title_refreshes: 0,
            window_request: None,
            bubble: None,
            bubble_pinned: None,
            bubble_hover: None,
            menu: None,
            search,
            context_document_pill: None,
            context_attribute_chip: None,
            context_attachment_pill: None,
            context_reminder_node: None,
            context_reminder: None,
            tag_table: Vec::new(),
            list_table: Vec::new(),
            confirm: None,
            dictation: None,
            spelling: spelling::SpellCheck::new(),
            context_spelling: None,
            diagrams: diagrams::Diagrams::new(),
        }
    }

    /// This document *is* the window: draw a title bar above the toolbar.
    ///
    /// `koru-workspace-host` opens every window undecorated ([ADR-25]) on the
    /// understanding that the view draws its own caption. The container does;
    /// a single-view binary did not, so `cargo run -p koru-document` had no
    /// minimise, maximise, close or drag at all, and Alt+F4 was the only way
    /// out. Opt-in rather than the default, because the same view is a *tab* in
    /// the container, where the caption above is already the container's.
    ///
    /// [ADR-25]: ../../../docs/decisions/README.md
    pub fn owns_the_window(mut self, ws: &WorkspaceDoc) -> Self {
        self.window_chrome = true;
        self.editor.view.chrome_h = self.chrome_h();
        // **The title moves to the title bar.** As a tab the document's name is
        // row 0 of its own body ([ADR-50-ondemand]) — an ordinary row you can
        // type in, which is the whole reason it is a row rather than a second
        // single-line editor. A window already has somewhere for a name to go,
        // and printing it twice, once as a heading and once in the bar above it,
        // is the sort of thing you only notice because it looks like a bug.
        //
        // `Body::title_row` is the flag that already answers this question, so
        // this is that switch rather than a new concept. The cost
        // is that the name is not editable *here*: `F2` and the explore column
        // are how it is renamed from a standalone window.
        self.editor.body.title_row = false;
        // **And no canvas gap above the sheet.** That gap separates the page
        // from a toolbar; the bar here is the page's own colour, so it
        // separated the page from nothing and just started the document late.
        // (The footer is already gone — `without_footer` runs in every
        // constructor, because no presentation of a document draws one.)
        self.editor.view.page_gap_top = 0.0;
        self.editor.body.rebuild(ws);
        self.refresh_title(ws);
        self.search
            .set_standalone_title(self.editor.body.root.map(|root| (root, self.title.clone())));
        self
    }

    /// How often the three pieces of work `adopt` arranges have actually run.
    /// See [`AdoptWork`].
    pub fn adopt_work(&self) -> AdoptWork {
        AdoptWork {
            titles: self.title_refreshes,
            searches: self.search.refreshes(),
            spellings: self.spelling.observations(),
        }
    }

    /// Where the selection bubble sits this frame, or `None` when there is none.
    ///
    /// A plain read of what [`place_bubble`](Self::place_bubble) decided, so the
    /// click path and the cursor can ask without a `&mut`.
    pub fn bubble_panel(&self) -> Option<Rect> {
        self.bubble.map(|(.., rect)| rect)
    }

    /// The formatting toolbar paired with a document context menu.
    ///
    /// It is the same bubble and command vocabulary used for selections, but
    /// its position belongs to the right-click stack rather than to glyph
    /// geometry. Titles, code and non-text blocks refuse the same formatting
    /// commands here that they refuse everywhere else.
    fn context_toolbar_panel(&self) -> Option<Rect> {
        let menu = match self.menu? {
            OpenMenu::Context(menu) => Some(menu),
            OpenMenu::ContextBlock { toolbar, .. }
            | OpenMenu::ContextMore { toolbar, .. }
            | OpenMenu::ContextList { toolbar, .. } => {
                return Some(toolbar);
            }
            _ => None,
        };
        let menu = menu?;
        // **The second gate, and the one that was missed.** A pill that owns its
        // menu has no toolbar over it — `with_toolbar` decides where the menu
        // is placed and this decides whether the bubble is painted, so the two
        // asking different questions is how a chip's menu came to be drawn
        // under a formatting bar it had already declined.
        if self.context_pill_kind().owns_the_menu() {
            return None;
        }
        // A corrections-only menu (right-click on a word, no selection) hangs
        // from the click with no toolbar — the click was about the word, not
        // about formatting.
        if self.context_spelling.as_ref().is_some_and(|snap| snap.only) {
            return None;
        }
        let focused = self.editor.body.focused()?;
        if !self.editor.view.active
            || self.editor.view.overlay.is_open()
            || self.editor.body.focus_is_title()
            || focused.kind == koru_core::Kind::Code
            || focused.kind == koru_core::Kind::Table
            || focused.kind.shape() != koru_core::node::Shape::Text
            || focused.selection_is_emoji_only()
        {
            return None;
        }
        // Geometry only: the toolbar is placed against the menu's *panel*, and
        // an enabled flag changes no row's height — but the spelling block
        // changes the row *count*, so it participates.
        let rows = context_menu::rows(
            true,
            true,
            self.editor.attribute_selection_is_mixed(),
            self.focused_task_context(),
            self.context_reminder_node.is_some(),
            None,
            None,
            self.context_pill_kind(),
            self.context_spell().as_ref(),
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
        );
        Some(context_menu::toolbar_panel(
            &menu,
            &rows,
            self.editor.view.viewport,
            self.chrome_h(),
        ))
    }

    /// What the row the menu is anchored to is **as a task**.
    ///
    /// Read from the row cache rather than the document, and that is a
    /// requirement rather than a shortcut: `menu_shape` is handed no
    /// `WorkspaceDoc`, and `every_menu_lays_out_the_same_with_and_without_the_document`
    /// fails the moment a row's shape depends on something only `menu_rows` can
    /// see. `Body::rebuild` is what keeps the cache honest.
    fn focused_task_context(&self) -> context_menu::TaskContext {
        context_menu::TaskContext::of(self.editor.body.focused().and_then(|row| row.task))
    }

    fn focused_table_context(&self) -> Option<context_menu::TableContext> {
        let focused = self.editor.body.focused()?;
        if focused.kind != koru_core::Kind::Table {
            return None;
        }
        let visual = focused.table.as_ref()?;
        let selected = self
            .editor
            .view
            .table_cell
            .filter(|(id, _, _)| *id == focused.id)
            .map(|(_, row, column)| (row, column));
        let cell = selected
            .or_else(|| {
                visual
                    .grid
                    .as_ref()
                    .map(|grid| &grid.parsed)
                    .map(|parsed| (1.min(parsed.rows.len()), 0))
            })
            .unwrap_or((0, 0));
        Some(context_menu::TableContext {
            cell,
            source_visible: self.editor.view.table_source == Some(focused.id)
                || visual.grid.is_none(),
        })
    }

    /// The focused row as a diagram, for the menu's diagram group: `None` on
    /// every row that is not a `mermaid` code block. `drawn` reads this
    /// view's entry, which is where the drawing lives, so *Save as…* is
    /// offered exactly when there is a picture to save.
    fn focused_diagram_context(&self) -> Option<context_menu::DiagramContext> {
        let focused = self.editor.body.focused()?;
        if !koru_editor::diagram::is_diagram_row(focused) {
            return None;
        }
        let drawn = matches!(
            self.editor
                .view
                .diagrams
                .get(&focused.id)
                .map(|entry| &entry.state),
            Some(koru_editor::DiagramState::Drawn(_))
        );
        Some(context_menu::DiagramContext {
            node: focused.id,
            drawn,
        })
    }

    // ------------------------------------------------------------- diagrams
    //
    // The host's side of a drawn `mermaid` block, passed straight through to
    // the editor (`koru_editor::diagram`): the editor asks for every layout and
    // never computes one, and the shell answers from a worker it owns.

    /// The layouts the editor is waiting for, taken off its queue. Drained by
    /// the shell's maintenance, handed to its layout worker.
    pub fn take_diagram_requests(&mut self) -> Vec<koru_editor::DiagramRequest> {
        self.editor.take_diagram_requests()
    }

    /// The worker's answer for one request: the prepared drawing, or the
    /// status that says why there is none. `true` when the view changed and a
    /// redraw is owed — the shell returns it from `maintain`.
    pub fn diagram_result(
        &mut self,
        node: NodeId,
        text_hash: u64,
        result: Result<
            std::sync::Arc<koru_draw::diagram::PreparedDiagram>,
            koru_editor::diagram::Status,
        >,
    ) -> bool {
        self.editor.diagram_result(node, text_hash, result)
    }

    /// *Save as…* asked on a drawn diagram, if it was — the way
    /// `take_wants_file_action` surfaces a file action. The shell owns the
    /// save dialog, the raster and the write; this view only names the block.
    /// `View::take_wants_save_diagram_as` drains it, pairing the node with
    /// its drawing and the page's title for the shell.
    pub fn take_wants_save_diagram_as(&mut self) -> Option<koru_editor::SaveDiagramAs> {
        self.editor.view.wants_save_diagram_as.take()
    }

    /// The drawing `node` shows right now, if it is a diagram in its `Drawn`
    /// state — what *Save as…* saves. `None` for a placeholder, a block that
    /// does not draw, or any other row: the menu offers the row only when
    /// this answers, and a request that races a keystroke is simply dropped.
    pub fn drawn_diagram(
        &self,
        node: NodeId,
    ) -> Option<std::sync::Arc<koru_draw::diagram::PreparedDiagram>> {
        match self
            .editor
            .view
            .diagrams
            .get(&node)
            .map(|entry| &entry.state)
        {
            Some(koru_editor::DiagramState::Drawn(prepared)) => Some(prepared.clone()),
            _ => None,
        }
    }

    /// Decide whether there is a bubble this frame and, if there is, **pin it**.
    ///
    /// Called once from `View::draw`, before anything asks. The rect is measured
    /// only when the *selection* changes: it comes from the selection's glyphs,
    /// and re-measuring every frame slid the bar sideways whenever a mark
    /// changed the width of the words under it — which is every time somebody
    /// presses Bold, out from under the pointer that pressed it.
    fn place_bubble(&mut self) {
        // A summoned bubble belongs to the row it was summoned on. The caret
        // moving elsewhere retires it, which is what the selection would have
        // done — otherwise it would hang over a row it no longer describes.
        if self.bubble_pinned.is_some() && self.bubble_pinned != self.editor.body.focused_id() {
            self.bubble_pinned = None;
        }
        let showing = self.editor.view.active
            // **Never over a page that cannot change.** Every row in the bubble
            // is a mark or a TURN INTO, so on a read-only page it is a panel of
            // buttons that all do nothing — and a person who pressed Bold and
            // saw nothing happen would be right to file it. Selecting and
            // copying still work; only the offer goes.
            && !self.editor.view.read_only
            && !self.editor.view.overlay.is_open()
            // Not while the selection is still being made. A drag is a
            // different selection every frame — one of them crossing into a
            // second block, where there is no bubble at all — so a bar that
            // appeared as soon as a range was non-empty flickered and moved
            // under the pointer that was drawing it. The release is the moment
            // the selection means something.
            && !self.editor.view.dragging
            // **Never over a title, code, or a table grid.** A document's name
            // is a name; code is coloured by the syntax engine, and a table's
            // inline formatting comes from its Markdown source. All three
            // refuse marks in `Editing::toggle_mark` and
            // friends — this is why nothing offers them in the first place.
            && !self.editor.body.focus_is_title()
            && self
                .editor
                .body
                .focused()
                .is_some_and(|r| {
                    !matches!(r.kind, koru_core::Kind::Code | koru_core::Kind::Table)
                })
            // Either the selection asked for it, or `Ctrl+.` did. Every veto
            // above still applies to both: summoning it over a title or inside
            // a code block shows nothing, because those two refuse marks.
            && (Editing::has_text_selection(&self.editor) || self.bubble_pinned.is_some())
            // Pure-emoji selections cannot take Bold / TURN INTO — selecting
            // 👋 is fine, but the bubble has nothing useful to offer.
            && !self
                .editor
                .body
                .focused()
                .is_some_and(|r| r.selection_is_emoji_only());
        let range = showing
            .then(|| self.editor.body.focused())
            .flatten()
            .map(|row| {
                let r = row.range();
                (row.id, r.start, r.end)
            });
        let Some((id, from, to)) = range else {
            self.bubble = None;
            // The same hover clock drives the toolbar paired with a context
            // menu. Do not erase it merely because that toolbar has no text
            // selection to anchor to.
            if !self.menu.is_some_and(OpenMenu::is_context_surface) {
                self.bubble_hover = None;
            }
            return;
        };
        // A summoned bubble is pinned to its *row*, not to the caret offset:
        // typing under it must not drag it along the line. A selected one is
        // pinned to the exact range, so a different range gets a fresh place.
        let same = if self.bubble_pinned == Some(id) {
            self.bubble.is_some_and(|(bid, ..)| bid == id)
        } else {
            self.bubble
                .is_some_and(|(bid, bf, bt, _)| (bid, bf, bt) == (id, from, to))
        };
        if same {
            return;
        }
        // The selection's first line, or — when it was summoned at a bare
        // caret — the caret itself. A collapsed selection covers no glyphs, so
        // `selection_rects` has nothing to report and the caret's own box is
        // the only thing to hang from.
        let anchor = Editing::selection_rects(&self.editor)
            .into_iter()
            .next()
            .or_else(|| Editing::caret_screen_rect(&self.editor));
        let Some(first) = anchor else {
            self.bubble = None;
            return;
        };
        let rect = bubble::panel(first, self.editor.view.viewport, self.chrome_h());
        self.bubble = Some((id, from, to, rect));
    }

    /// How far into its appearance the bubble's tooltip is, on the same clock
    /// as the `+`'s: nothing for `theme::motion::tooltip().hover_delay`, then a short fade.
    pub fn bubble_tooltip_progress(&self) -> f32 {
        let Some((_, since)) = self.bubble_hover else {
            return 0.0;
        };
        let waited = since.elapsed().as_millis() as f32
            - theme::motion::tooltip().hover_delay.as_millis() as f32;
        (waited / theme::motion::tooltip().enter_duration.as_millis() as f32).clamp(0.0, 1.0)
    }

    /// Whether that tooltip is mid-wait or mid-fade, so the shell keeps waking
    /// this view. Returns to `false` at the end of the fade (ADR-26).
    pub fn bubble_tooltip_animating(&self) -> bool {
        self.bubble_hover.is_some() && self.bubble_tooltip_progress() < 1.0
    }

    /// Everything this view draws above its rows: its own caption row when it
    /// *is* the window, and nothing at all inside a container tab.
    ///
    /// It was `CAPTION_H + TOOLBAR_H`. The toolbar is gone, and with it the one
    /// band of chrome a document wore permanently — a tab now opens straight
    /// onto the page.
    fn chrome_h(&self) -> f32 {
        if self.window_chrome {
            theme::CAPTION_H
        } else {
            0.0
        }
    }

    /// What the renderer needs to know about this window's title bar.
    fn window_chrome(&self) -> Option<draw::WindowChrome> {
        self.window_chrome.then_some(draw::WindowChrome {
            maximized: self.maximized,
            pinned: self.pinned,
            menu_morph: self.menu_morph.progress(),
            menu_open: self.window_menu_open(),
            title: self.title.clone(),
            search: self.editor.body.root.and_then(|root| {
                self.editor
                    .view
                    .search_highlights
                    .iter()
                    .find(|highlight| highlight.node == root)
                    .cloned()
            }),
            offline: self
                .store_trouble
                .as_ref()
                .map(|t| (caption::store_detail(caption::lang(), t), self.offline_open)),
        })
    }

    /// The find panel's rect while it is open, in this view's coordinates.
    ///
    /// The panel floats over the top-right of the content, which is exactly
    /// where the title row parks its page-furniture button. The container asks
    /// this through `Leaf::title_action_rect` so the two cannot draw over each
    /// other or fight for the same click.
    pub fn search_panel(&self) -> Option<koru_draw::geometry::Rect> {
        self.search.panel(&self.editor)
    }

    /// The **Offline** chip, when this view is drawing its own caption.
    ///
    /// `None` inside a container tab, where the caption above is the container's
    /// and its chip is already there — two chips in one window would be the
    /// same fact said twice.
    fn offline_chip(&self) -> Option<koru_draw::geometry::Rect> {
        if !self.window_chrome {
            return None;
        }
        caption::offline_chip(self.editor.view.viewport.0, self.store_trouble.is_some())
    }

    /// Resolve the chord first, so this view can claim what is its own and
    /// refuse the structural ones.
    ///
    /// Two things happen here and nowhere else. Tab is remapped: the table says
    /// `Indent`, because that is what Tab means in an outline, and prose wants a
    /// literal tab instead. And every other structural action is *swallowed* —
    /// indenting and reordering are not things prose has, and letting them fall
    /// through to the shared editor would perform them on it.
    /// Route a key, then let the title bar catch up.
    ///
    /// **Read-only is enforced by an allowlist, and the direction matters.**
    ///
    /// **The refresh lives here, not in the `View` impl.** `View::key` looked
    /// like the right place until `manager_view` turned out to call the
    /// inherent method directly for a popped-out window — the very window whose
    /// bar this is — and an inherent method wins over a trait one at every call
    /// site that has it in scope. Putting it on the inside means there is no
    /// route that misses it.
    pub fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.editor.view.read_only && !reading_key(key, text, mods) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] document key refused=read-only"
            ));
            return false;
        }
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] document key active={} search={} overlay={} blockers={:?}",
            self.editor.view.active,
            self.search.is_open(),
            self.editor.view.overlay.is_open(),
            self.blockers(),
        ));
        let handled = self.key_inner(ws, key, text, mods);
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] document key handled={handled} active={}",
            self.editor.view.active,
        ));
        // A *local* rename does not come back round through `adopt` the way a
        // peer's does — the same asymmetry that makes the container re-title a
        // tab as it routes the key.
        self.refresh_title(ws);
        handled
    }

    /// What owns this view's keyboard outright.
    ///
    /// A document has one such surface — a menu, of any of its nine kinds —
    /// and it is grouped with the container's modals because `menu_key`
    /// answers **true whatever the key was**. Everything else this view routes
    /// decides per key: the find bar takes the keys it recognises and lets the
    /// rest through, and the chords above it are chords.
    ///
    /// The vocabulary is [`koru_workspace_host::routing`]'s so that a container hosting
    /// this view in a tab and a `koru-document` window hosting it alone give
    /// the same answer in the same words.
    pub fn blockers(&self) -> Blockers {
        if self.menu.is_some() {
            Blockers::Modal(ModalKind::ContextMenu)
        } else {
            Blockers::None
        }
    }

    /// Where a typed character would land in this document.
    ///
    /// The order is `key`'s own, and each step is a fact about it:
    ///
    /// - **Read-only first.** `key` refuses everything outside an allowlist of
    ///   reading keys, and a printable character is not on it — so a read-only
    ///   document takes no text *anywhere*, the find bar included.
    /// - **Then the find bar**, because `key_inner` asks it before anything
    ///   else and a character always ends in whichever of its two fields has
    ///   focus.
    /// - **Then an open menu**, which answers every key and types nothing.
    /// - **Then a prompt in the document.** Only the two overlays that *are*
    ///   text fields — the name prompt and the link prompt — count; the
    ///   pickers filter themselves from typed characters and are reported as
    ///   `None`, because a dictation into a filter list is not a thing anyone
    ///   asked for and a wrong `None` only declines.
    /// - Otherwise the body.
    pub fn editable_target(&self, tab: TabId) -> EditableTarget {
        if self.editor.view.read_only {
            return EditableTarget::None;
        }
        if let Some(field) = self.search.focused_field() {
            return EditableTarget::Field(field);
        }
        if self.blockers().is_blocking() {
            return EditableTarget::None;
        }
        match self.editor.view.overlay {
            koru_editor::components::Overlay::Name(_)
            | koru_editor::components::Overlay::Link(_) => {
                EditableTarget::Field(FieldId::DocumentPrompt)
            }
            koru_editor::components::Overlay::None => EditableTarget::Tab(tab),
            _ => EditableTarget::None,
        }
    }

    /// **Route a dictation into whichever of this document's surfaces a typed
    /// character would reach.** Answers whether anything changed.
    ///
    /// The routing decision is made **once**, at
    /// [`Begin`](koru_dictation::DictationAction::Begin), through the same
    /// [`editable_target`](Self::editable_target) the key path's tail already
    /// computes — so a dictation and a keystroke cannot disagree about where
    /// text goes. Every later action is delivered to what was recorded, and an
    /// action whose generation is not the open composition's is dropped: a
    /// `Final` from a session that was cancelled a moment ago must commit
    /// nothing.
    pub fn dictate(
        &mut self,
        ws: &mut WorkspaceDoc,
        action: &koru_dictation::DictationAction,
    ) -> bool {
        use koru_dictation::DictationAction as A;
        if let A::Begin { generation } = action {
            if self.dictation.is_some() {
                return false;
            }
            let into = match self.editable_target(TabId::STANDALONE) {
                EditableTarget::Tab(_) => DictationInto::Body,
                EditableTarget::Field(field) => DictationInto::Field(field),
                EditableTarget::None => return false,
            };
            let opened = match into {
                DictationInto::Body => self.editor.begin_dictation(ws),
                DictationInto::Field(field) => {
                    self.compose_into(field).is_some_and(|mut c| c.begin())
                }
            };
            if opened {
                self.dictation = Some((*generation, into));
            }
            return opened;
        }
        let Some((generation, into)) = self.dictation else {
            return false;
        };
        if action.generation() != generation {
            return false;
        }
        if action.closes() {
            self.dictation = None;
        }
        match into {
            DictationInto::Body => match action {
                A::Begin { .. } => false,
                A::Update { text, .. } => {
                    self.editor.update_dictation(ws, text.as_str());
                    true
                }
                A::Commit { text, .. } => {
                    self.editor.commit_dictation(ws, text.as_str());
                    self.refresh_title(ws);
                    true
                }
                A::Cancel { .. } => {
                    self.editor.cancel_dictation(ws);
                    true
                }
            },
            DictationInto::Field(field) => {
                let Some(mut compose) = self.compose_into(field) else {
                    return false;
                };
                match action {
                    A::Begin { .. } => false,
                    A::Update { text, .. } => {
                        compose.update(text.as_str());
                        true
                    }
                    A::Commit { text, .. } => {
                        compose.commit(text.as_str());
                        true
                    }
                    A::Cancel { .. } => {
                        compose.cancel();
                        true
                    }
                }
            }
        }
    }

    /// **Where a listening surface hangs from**, in this view's **own space**
    /// — the same coordinates [`Editing::selection_rects`] reports, content
    /// column and scroll already applied. A container converts.
    ///
    /// `None` when this document is not the one dictating, which is what keeps
    /// the indicator off every other tab.
    ///
    /// The body's answer is the plan's rule spelled out: the composition's own
    /// rectangle, and the caret's box when there is no composition to have one
    /// — a collapsed caret covers no glyphs, and a transcript that is still
    /// empty covers none either, so between the press and the first partial
    /// the caret is the only thing there is to hang from.
    ///
    /// A find or replace bar answers with the **panel** rather than with the
    /// box's laid-out line: `TextField::composition_rect` is measured along
    /// that line, before the field's horizontal scroll and before whatever
    /// rectangle a host drew around it, and the panel is the rectangle this
    /// view actually owns. A prompt answers with the caret it was opened from,
    /// which is where it is placed. See Q-V62.
    pub fn dictation_anchor(&self) -> Option<Rect> {
        let (_, into) = self.dictation?;
        let caret = || Editing::caret_screen_rect(&self.editor);
        match into {
            DictationInto::Body => {
                koru_ui::dictation::DictationTarget::composition_rect(&self.editor).or_else(caret)
            }
            DictationInto::Field(FieldId::Find | FieldId::Replace) => {
                self.search.panel(&self.editor).or_else(caret)
            }
            DictationInto::Field(_) => caret(),
        }
    }

    /// The one-line surface `field` names, with the engine that lays it out.
    ///
    /// Both halves are fields of this view, borrowed disjointly — the find
    /// bar's boxes and the prompts alike are laid out by
    /// `ViewState::engine`, which is the editor's and not a second one.
    fn compose_into(&mut self, field: FieldId) -> Option<koru_ui::dictation::ComposeInto<'_>> {
        use koru_editor::components::Overlay;
        use koru_ui::dictation::ComposeInto;
        match field {
            FieldId::Find | FieldId::Replace => {
                let input = self.search.field_mut(field)?;
                Some(ComposeInto::Input(input, &mut self.editor.view.engine))
            }
            FieldId::DocumentPrompt => match &mut self.editor.view.overlay {
                Overlay::Name(prompt) => Some(ComposeInto::Field(
                    &mut prompt.input,
                    &mut self.editor.view.engine,
                )),
                // A link prompt is two boxes; the dictation goes to whichever
                // has the caret, which is the one a typed character reaches.
                Overlay::Link(prompt) => {
                    let field = match prompt.field {
                        koru_editor::components::link_prompt::Field::Name => &mut prompt.name,
                        koru_editor::components::link_prompt::Field::Url => &mut prompt.url,
                    };
                    Some(ComposeInto::Field(field, &mut self.editor.view.engine))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn key_inner(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // **A modal question is answered before anything else is asked.** It
        // swallows every key it does not understand for `Confirm`'s reason: a
        // dialog that let keystrokes through would take dictation during a
        // warning. Enter is *Cancel* here until the focus is moved — the whole
        // point of that type, and the confirm recipe in `docs/features/design/DESIGN.md`.
        if let Some(out) = self.confirm.as_mut().map(|(c, _)| c.key(key)) {
            self.settle_confirm(ws, out);
            return true;
        }
        self.search.set_standalone_title(
            self.window_chrome
                .then(|| self.editor.body.root.map(|root| (root, self.title.clone())))
                .flatten(),
        );
        if self.search.key(ws, &mut self.editor, key, text, mods) {
            self.menu = None;
            self.bubble = None;
            self.bubble_pinned = None;
            return true;
        }
        // The layout chords come before the menu, so the page settings can be
        // left open while you step through the three and watch the page change.
        if let Some(mode) = (koru_keys::has_primary(mods)
            && !koru_keys::has_foreign_primary(mods)
            && mods.shift_key()
            && !mods.alt_key())
        .then(|| keymap::ctrl_letter(key, text))
        .flatten()
        .and_then(page_menu::mode_for_key)
        {
            Editing::set_view_mode(self, mode);
            self.settle();
            return true;
        }
        // **Ctrl+W closes the window here**, because here there are no tabs for
        // it to close one of. In the container this chord never arrives —
        // `mainview::keys` claims `w` and closes a tab — so this arm is reached
        // only by a window that *is* one document: a popped-out tab, or
        // `koru-document`. Alt+F4 already did this; Ctrl+W not doing it was the
        // odd one out.
        //
        // Ctrl+Shift+W is deliberately not taken: it means "close every tab",
        // which a window with none of them has no answer for.
        if koru_keys::primary_only(mods) && keymap::ctrl_letter(key, text) == Some('w') {
            self.window_request = Some(WindowRequest::Close);
            return true;
        }
        // **F2 renames the document, here too.** It was the container's chord
        // alone — `mainview` claims it and calls `start_rename` on the active
        // tab — so a standalone window had no way to reach the prompt at all.
        // That went unnoticed while the name was the first line of the page and
        // could simply be typed over; turning `title_row` off for a window took
        // the last route away and left the name uneditable from the window
        // showing it.
        //
        // Before the menu guard, so it works with the window menu open — the
        // menu is about the window, and this is about the document in it.
        if matches!(key, Key::Named(NamedKey::F2)) {
            return Editing::start_rename(self, ws);
        }
        // **Derived from `blockers`, not re-decided here.** An open menu
        // answers every key it is given, which is the same thing the
        // container's dialogs do and the reason both say so through one type.
        if matches!(self.blockers(), Blockers::Modal(ModalKind::ContextMenu)) {
            return self.menu_key(ws, key);
        }
        // `Ctrl+.` summons the bubble at the caret. A period rather than a
        // letter because it survives the platform: `Ctrl+Alt+letter` is AltGr on
        // Windows and never arrives, and every plain `Ctrl+letter` this view
        // could want is already spoken for. `ctrl_letter` passes punctuation
        // through, so it reaches here as '.'.
        if koru_keys::primary_only(mods) && keymap::ctrl_letter(key, text) == Some('.') {
            self.bubble_pinned = self.editor.body.focused_id();
            return true;
        }
        // Escape retires a summoned bubble. Only a summoned one: a bubble over
        // a selection goes when the selection does, and Escape there means
        // whatever it means to the editor.
        if matches!(key, Key::Named(NamedKey::Escape)) && self.bubble_pinned.is_some() {
            self.bubble_pinned = None;
            return true;
        }
        // F7 cycles the view mode. Bound here rather than in the shared editor
        // keymap because it is a pure presentation toggle, so it fires with or
        // without the caret and over an open rename prompt. The three
        // `Ctrl+Alt` chords above name a mode outright, for the same reason a
        // heading has both `Ctrl+Alt+2` and a menu row.
        if let Key::Named(NamedKey::F7) = key {
            self.cycle_view_mode();
            return true;
        }
        // F4 hides ticked rows. **Above the `view.active` check on purpose**:
        // hiding is not line-specific, so it is one of the two things that still
        // work with the caret dropped — `Action::works_without_a_caret` is where
        // that is written down. It filters on `meta.done`, so in prose it is a
        // toggle with nothing to hide.
        if !self.editor.view.overlay.is_open()
            && matches!(
                keymap::resolve(key, text, mods),
                Some(Action::ToggleHideCompleted)
            )
        {
            self.editor.body.hide_completed = !self.editor.body.hide_completed;
            self.editor.body.rebuild(ws);
            self.editor.view.overlay = koru_editor::components::Overlay::None;
            self.reflow();
            return true;
        }
        if !self.editor.view.overlay.is_open() && self.editor.view.active {
            match keymap::resolve(key, text, mods) {
                Some(Action::Indent(dir)) => {
                    // A name is not indented and does not take a literal tab.
                    if self.editor.body.focus_is_title() {
                        return true;
                    }
                    // A list item nests with Tab the way the outline did —
                    // bounded by the document root so it cannot escape. Prose
                    // still gets a literal tab, and Shift+Tab in prose does
                    // nothing.
                    let in_list = self
                        .editor
                        .body
                        .focused()
                        .is_some_and(|r| r.list_style.is_some());
                    if in_list || self.nests() {
                        self.indent_structure(ws, dir);
                        self.settle();
                    } else if dir > 0 {
                        Editing::act(self, ws, Action::InsertTab);
                        self.settle();
                    }
                    return true;
                }
                // A selected document pill claims Ctrl+Enter before ToggleDone
                // can insert a paragraph. Same destination as Ctrl+click.
                Some(Action::ToggleDone) if self.editor.view.selected_pill.is_some() => {
                    if let Some((row, attr)) = self.editor.view.selected_pill {
                        self.follow_document_pill(ws, row, attr, OpenWhere::BackgroundTab);
                    }
                    return true;
                }
                // Ctrl+Enter is ToggleDone, a structural action, but a document
                // gives it block-specific meanings: a code block means "leave
                // the block", a table and prose create a paragraph below, and a
                // task row ticks it. Routed before the structural-scope filter
                // below eats it.
                Some(Action::ToggleDone)
                    if self.editor.body.focused().is_some_and(|r| {
                        matches!(
                            r.kind,
                            koru_core::node::Kind::Code
                                | koru_core::node::Kind::Table
                                | koru_core::node::Kind::Paragraph
                        )
                    }) =>
                {
                    Editing::act(self, ws, Action::ToggleDone);
                    self.settle();
                    return true;
                }
                Some(Action::ToggleDone) => {
                    self.toggle_done(ws);
                    self.settle();
                    return true;
                }
                // Ctrl+Shift+↑/↓ reorders blocks among their siblings — the same
                // chord a nesting view uses, and the same op the drag grip
                // already performs here. Claimed before the scope filter so it
                // is not swallowed as a no-op.
                Some(Action::Reorder(delta)) => {
                    self.reorder(ws, delta);
                    self.settle();
                    return true;
                }
                Some(a) if a.scope() == Scope::Structural => return true,
                _ => {}
            }
        }
        // A bubble belongs to a selection or an explicitly summoned caret. As
        // soon as text input begins, it is no longer describing the editing
        // state and must not hover over the words being entered.
        if text.is_some_and(|input| !input.is_empty())
            && !koru_keys::has_primary(mods)
            && !mods.alt_key()
        {
            self.bubble = None;
            self.bubble_pinned = None;
            self.bubble_hover = None;
        }
        Editing::key(self, ws, key, text, mods)
    }

    /// Move the selection among its siblings — Ctrl+Shift+↑/↓.
    ///
    /// The title row is the document itself and never moves. Multi-row
    /// selections walk top-first when moving up and bottom-first when moving
    /// down, so a block does not step over one that has not moved yet.
    /// A completely selected list is promoted to its invisible ListBlock first:
    /// paragraphs and lists can then move as sibling blocks without flattening
    /// the list or leaving it behind in a different structural parent.
    fn reorder(&mut self, ws: &mut WorkspaceDoc, delta: i32) {
        if self.editor.body.focus_is_title() {
            return;
        }
        let anchor = self.editor.body.anchor;
        let focused = self.editor.body.focused_id();
        let mut ids = self.editor.body.selection_roots(ws);
        ids.retain(|id| {
            self.editor
                .body
                .index_of(*id)
                .is_some_and(|i| !self.editor.body.is_title_row(i))
        });
        Self::promote_complete_list_blocks(ws, &mut ids);
        if ids.is_empty() {
            return;
        }
        // Preflight the complete selection before the first move. `shift` is a
        // one-node operation; calling it sequentially when the trailing root is
        // already at the end lets that root refuse while earlier roots still
        // move, which rearranges the selection instead of stopping it. A group
        // move is all-or-nothing at either boundary.
        let can_move = ids.iter().all(|id| {
            let Ok(parent) = ws.parent(*id) else {
                return false;
            };
            let Ok(siblings) = ws.children(parent) else {
                return false;
            };
            let Ok(from) = ws.sibling_index(*id) else {
                return false;
            };
            from.checked_add_signed(delta as isize)
                .is_some_and(|to| to < siblings.len())
        });
        if !can_move {
            return;
        }
        if delta >= 0 {
            ids.reverse();
        }
        ws.checkpoint();
        let mut moved = false;
        for id in ids {
            moved |= ws.shift(id, delta).unwrap_or(false);
        }
        if !moved {
            return;
        }
        self.editor.body.rebuild(ws);
        self.editor.body.anchor = anchor;
        if let Some(id) = focused {
            self.editor.body.focus_on(id);
        }
        self.editor.body.mark_selection();
    }

    /// Whether Tab means "nest" here rather than "type a tab".
    ///
    /// A row under a list block nests, and so does a `Kind::Task` — the second
    /// is what the task outline's rows were, and it is what a task written
    /// straight into a document is. Prose does not: indenting a paragraph under
    /// the one above it, and carrying that into every line after it, is worse
    /// than a literal tab.
    fn nests(&self) -> bool {
        self.editor
            .body
            .focused()
            .is_some_and(|r| r.list_style.is_some() || r.kind.is_task_like())
    }

    /// Indent or outdent the selection — Tab, Shift+Tab.
    ///
    /// The same shape as [`reorder`](Self::reorder), and for the same reason:
    /// only the *tops* of the selection move, because a row whose parent is
    /// also selected travels with its parent and moving it as well moves it
    /// twice. Indenting walks top-first and outdenting bottom-first, so a row
    /// does not step over one that has not moved yet.
    ///
    /// Outdent is bounded by the view's root, so a row cannot escape the
    /// document it is in.
    fn reparent(&mut self, ws: &mut WorkspaceDoc, dir: i32) {
        let anchor = self.editor.body.anchor;
        let focused = self.editor.body.focused_id();
        let mut ids = self.editor.body.selection_roots(ws);
        ids.retain(|id| {
            self.editor
                .body
                .index_of(*id)
                .is_some_and(|i| !self.editor.body.is_title_row(i))
        });
        if ids.is_empty() {
            return;
        }
        if dir <= 0 {
            ids.reverse();
        }
        ws.checkpoint();
        let root = self.editor.body.root;
        let mut moved = false;
        for id in ids {
            moved |= if dir > 0 {
                ws.indent(id).unwrap_or(false)
            } else {
                root.and_then(|r| ws.outdent_within(id, r).ok())
                    .unwrap_or(false)
            };
        }
        if !moved {
            return;
        }
        self.editor.body.rebuild(ws);
        self.editor.body.anchor = anchor;
        if let Some(id) = focused {
            self.editor.body.focus_on(id);
        }
        self.editor.body.mark_selection();
    }

    /// Tick or untick the focused row, leaving it exactly where it is.
    fn toggle_done(&mut self, ws: &mut WorkspaceDoc) {
        let Some(row) = self.editor.body.focused().filter(|r| r.is_task()) else {
            return;
        };
        let (id, done) = (row.id, row.done);
        self.editor.body.set_done(ws, id, !done);
    }

    /// Complete the focused task outright, or reopen it — the context menu's
    /// Complete/Reopen row.
    ///
    /// **Not [`toggle_done`](Self::toggle_done)**, and that is the whole point
    /// of the row: `set_done` is the checkbox, which on a task with structural
    /// children moves the box and nothing else (SPEC §5). This runs the
    /// canonical command, so it checks the box, cascades through everything
    /// checkable below, and completes nested tasks in one undo step.
    ///
    /// It reads the **canonical** lifecycle off the row rather than `done`, for
    /// the same reason: the two are allowed to disagree here, and the row's
    /// label was written from the canonical half.
    ///
    /// **One task, the anchor's** — unlike the attribute rows beside it, which
    /// apply to the whole block selection. Right-clicking moves the caret to the
    /// row under the pointer unless the click landed inside an existing
    /// selection, so the anchor is the row whose label was drawn; and completing
    /// already cascades *downward*, so "all of them" would need a second,
    /// sideways meaning for one gesture. Multi-select completion is a decision
    /// nobody has taken, not an omission from this one.
    fn complete_focused_task(&mut self, ws: &mut WorkspaceDoc) {
        let Some(row) = self.editor.body.focused() else {
            return;
        };
        let (Some(task), id) = (row.task, row.id) else {
            return;
        };
        self.editor.body.set_task_completed(ws, id, !task.completed);
    }

    // ------------------------------------------------- remove from tasks
    //
    // The one question this view asks, and everything about it is here: what
    // it says, how it is answered, and what Remove runs.

    /// How much of a row's own text the question quotes.
    ///
    /// A row is a whole line and a line can be a paragraph; the panel wraps,
    /// but a question that is four lines of somebody's prose has stopped being
    /// a question. Long titles are cut at a character boundary and elided.
    const QUOTED_TITLE: usize = 72;

    /// **Ask before taking a row out of Tasks** — SPEC §6, drawn as the
    /// design's E-3 card.
    ///
    /// The warning is about the **due date**, because that is the part of the
    /// removal nothing on screen shows going: the marker and the lifecycle are
    /// invisible anyway, but a row's due date is why it is in a Today list, and
    /// clearing it silently takes the row out of somebody's day. Priority and
    /// the checkbox are deliberately unmentioned — both survive, and naming a
    /// thing that is not happening is how a warning stops being read.
    ///
    /// A task with **no** due date drops that sentence rather than saying "no
    /// due date": there is nothing to warn about, and the copy that is left is
    /// the promise the design ends on — the row itself stays.
    ///
    /// Asked over the **focused** row, like Complete beside it, and for the
    /// same reason: right-clicking moved the caret to the row whose label was
    /// drawn.
    fn ask_remove_from_tasks(&mut self, ws: &WorkspaceDoc) {
        self.ask_remove_from_tasks_on(ws, chrono::Local::now().date_naive());
    }

    /// [`ask_remove_from_tasks`] against an explicit `today` — the same seam
    /// `chips::chip_boxes_on` keeps, and for the same reason: "Today" and
    /// "Aug 30" are the same value on different days, so a test that did not
    /// pin the clock would assert a different sentence in September.
    ///
    /// [`ask_remove_from_tasks`]: Self::ask_remove_from_tasks
    fn ask_remove_from_tasks_on(&mut self, ws: &WorkspaceDoc, today: chrono::NaiveDate) {
        let Some(row) = self.editor.body.focused() else {
            return;
        };
        if row.task.is_none() {
            return;
        }
        let node = row.id;
        let title = Self::quoted_row_title(row);
        let due = Self::row_due_label(row, today);
        let document = self.document_name(ws);
        let body = match due {
            Some(due) => format!(
                "\u{201C}{title}\u{201D} is due {due}. Removing it clears the due date on the \
                 row in {document}; the row itself stays."
            ),
            None => format!(
                "Removing \u{201C}{title}\u{201D} stops tracking it as a task in {document}; \
                 the row itself stays."
            ),
        };
        self.confirm = Some((
            koru_ui::Confirm::new("Remove this task?", body, "Remove"),
            Confirming::RemoveFromTasks(node),
        ));
    }

    /// **Ask before deleting the row itself.**
    ///
    /// *Remove from Tasks* beside it takes the marker off and leaves the
    /// writing; this takes the writing. A task is a marked node (ADR-285), so
    /// those are the only two answers there are, and which one somebody wants
    /// is not something a menu can work out — hence two rows, and hence this
    /// second question. It confirms for the plainer reason too: the milder of
    /// the pair already does, and a harsher action that did not would read as
    /// the safer one.
    fn ask_delete_row(&mut self, ws: &WorkspaceDoc) {
        // The chip's own row first, the focused one second — the same rule
        // *Go to task* applies, so the question is about the chip that was
        // right-clicked even when the caret sits elsewhere.
        let Some(row) = self
            .context_attribute_chip
            .as_ref()
            .and_then(|(row, _)| self.editor.body.rows.get(*row))
            .or_else(|| self.editor.body.focused())
        else {
            return;
        };
        let node = row.id;
        let title = Self::quoted_row_title(row);
        let document = self.document_name(ws);
        let body = format!(
            "\u{201C}{title}\u{201D} and everything it says are deleted from {document}. Rows \
             nested under it move up a level rather than going with it."
        );
        self.confirm = Some((
            koru_ui::Confirm::new("Delete this row?", body, "Delete"),
            Confirming::DeleteRow(node),
        ));
    }

    /// Delete the row, through the editor's own structural seam.
    ///
    /// [`Editing::delete_rows`] with `promote`, which is the same call
    /// `Ctrl+Shift+K` makes — so the menu row and the chord do one thing, and
    /// rows nested under this one survive by moving up rather than being taken
    /// with it. One undo step; the command takes its own checkpoint.
    fn delete_row(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        // The caret was put on this row by the right-click that opened the
        // menu, but a confirmation is a modal with a person in front of it —
        // so the row is named again here rather than trusted to still be
        // focused. **And the selection is dropped first**: a right-click
        // inside a row selection keeps it on purpose, `delete_rows` acts on
        // the selection, and `focus_on` moves the caret without clearing it —
        // so without this line a question naming one row deleted every
        // selected one.
        self.editor.body.clear_selection();
        self.editor.body.focus_on(node);
        if Editing::delete_rows(&mut self.editor, ws, true) {
            self.settle();
        }
    }

    /// The row's own text, trimmed and cut to [`QUOTED_TITLE`] characters.
    ///
    /// [`QUOTED_TITLE`]: Self::QUOTED_TITLE
    fn quoted_row_title(row: &koru_editor::Paragraph) -> String {
        let text = row.text.trim().to_string();
        if text.is_empty() {
            // An empty row is still a task — `/task` on a blank line makes one.
            // "this row" is what the question can honestly call it.
            return "this row".to_string();
        }
        if text.chars().count() <= Self::QUOTED_TITLE {
            return text;
        }
        let cut: String = text.chars().take(Self::QUOTED_TITLE).collect();
        format!("{}\u{2026}", cut.trim_end())
    }

    /// The row's due date as the chip beside it prints it — the same function,
    /// so the question and the chip can never say two different dates.
    fn row_due_label(row: &koru_editor::Paragraph, today: chrono::NaiveDate) -> Option<String> {
        // `Due(None)` is E-4's **dash** chip — a task with no due date, which
        // is exactly the case that drops the warning sentence. Matching on
        // `Some` here rather than on the chip's presence is what keeps the two
        // apart.
        row.chips.iter().find_map(|chip| match chip {
            koru_editor::chips::ChipSpec::Due(Some(due)) => {
                Some(koru_editor::chips::due_label(due, today))
            }
            _ => None,
        })
    }

    /// This document's name, for a sentence that says where the row is.
    ///
    /// Read here rather than taken from [`title`](Self::title): that cache is
    /// only kept when this view owns the window, and the question is asked
    /// far more often from a tab.
    fn document_name(&self, ws: &WorkspaceDoc) -> String {
        let name = self
            .editor
            .body
            .root
            .and_then(|root| ws.read(root).ok())
            .and_then(|n| n.content.as_text().map(str::to_string))
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            "Untitled".to_string()
        } else {
            name
        }
    }

    /// Answer the question, and put it away.
    ///
    /// `true` when something changed on screen — which `Keep` never does, and
    /// the other two always do, because the dialog itself goes.
    fn settle_confirm(&mut self, ws: &mut WorkspaceDoc, out: koru_ui::confirm::Outcome) -> bool {
        match out {
            // Two answers here; the third belongs to a dialog that declared
            // one (`Confirm::with_alternative`) and never reaches this.
            koru_ui::confirm::Outcome::Keep | koru_ui::confirm::Outcome::Alternative => false,
            koru_ui::confirm::Outcome::Cancel => {
                self.confirm = None;
                true
            }
            koru_ui::confirm::Outcome::Confirm => {
                match self.confirm.take().map(|(_, asked)| asked) {
                    Some(Confirming::RemoveFromTasks(node)) => self.remove_from_tasks(ws, node),
                    Some(Confirming::DeleteRow(node)) => self.delete_row(ws, node),
                    None => {}
                }
                true
            }
        }
    }

    /// Run the canonical command, through the editor's seam.
    ///
    /// **One undo step**, and not because this function arranges one:
    /// `koru_core::tasks::remove_from_tasks` takes its own checkpoint and
    /// writes inside one atomic group, so the marker, the lifecycle and the due
    /// come back together on a single Ctrl+Z. Priority and the checkbox are
    /// never touched — the command guarantees it, and the view test says so at
    /// this level as well.
    ///
    /// It goes through [`koru_editor::tasks::run`] rather than calling the
    /// command directly so the activity the removal owes lands in the sink the
    /// host drains, exactly as the checkbox's does.
    fn remove_from_tasks(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        let outcome = koru_editor::tasks::run(&mut self.editor.body.task_activity, ws, |ws| {
            koru_core::tasks::remove_from_tasks(ws, node).map(Some)
        });
        if outcome.is_err() {
            return;
        }
        self.editor.body.rebuild(ws);
        self.settle();
    }

    /// The question, over the scrim, above everything this view draws.
    ///
    /// Called from the `View` impl's `draw` tail rather than inside the frame:
    /// a modal is not document chrome, and drawing it there would put it under
    /// an open menu — which cannot happen, because opening it closes the menu,
    /// but only by accident of ordering.
    pub fn draw_confirm(&mut self, scene: &mut vello::Scene, root: vello::kurbo::Affine) {
        let Some((confirm, _)) = &self.confirm else {
            return;
        };
        let viewport = self.editor.view.viewport;
        confirm.draw(scene, root, &mut self.editor.view.engine, viewport);
    }

    /// Whether a modal question is up. Everything else is unanswerable while
    /// it is.
    pub fn confirm_open(&self) -> bool {
        self.confirm.is_some()
    }

    /// The pointer over the modal — never an I-beam, whatever is under it.
    pub fn confirm_cursor(&self) -> Option<winit::window::CursorIcon> {
        self.confirm.as_ref().map(|(c, _)| c.cursor())
    }

    /// Replace every complete set of selected ListBlock children with the
    /// invisible container that owns them. Selection itself stays on the
    /// visible rows; this list is only the set of structural nodes to move.
    fn promote_complete_list_blocks(ws: &WorkspaceDoc, ids: &mut Vec<NodeId>) {
        loop {
            let mut blocks = Vec::new();
            for id in ids.iter().copied() {
                let Ok(koru_core::schema::Parent::Node(parent)) = ws.parent(id) else {
                    continue;
                };
                if ws
                    .read(parent)
                    .is_ok_and(|node| node.kind == koru_core::Kind::ListBlock)
                    && !blocks.contains(&parent)
                {
                    blocks.push(parent);
                }
            }

            let mut promoted = false;
            for block in blocks {
                let Ok(children) = ws.children(koru_core::schema::Parent::Node(block)) else {
                    continue;
                };
                if children.is_empty() || !children.iter().all(|child| ids.contains(child)) {
                    continue;
                }
                let Some(at) = ids.iter().position(|id| children.contains(id)) else {
                    continue;
                };
                ids.retain(|id| !children.contains(id));
                ids.insert(at, block);
                promoted = true;
            }
            if !promoted {
                break;
            }
        }
    }

    // ------------------------------------------------------------------ menus
    //
    // At most one is open, and while it is, it owns the keyboard and the
    // pointer. Everything below is the same three questions for all four --
    // what rows does it have, what does row `i` mean, and where does it hang --
    // which is what the shared `menu::Menu` is for.

    /// The rows of whichever menu is open.
    ///
    /// Rebuilt on demand rather than stored: they are a pure function of the
    /// caret, and a stored copy is one that can go stale between the frame that
    /// drew it and the click that hits it.
    /// The document's declared prose language, when it names a dictionary.
    fn prose_language(
        &self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::spelling::SpellLang> {
        self.editor
            .body
            .root
            .and_then(|root| ws.read(root).ok())
            .and_then(|node| node.meta.prose_language)
            .as_deref()
            .and_then(koru_workspace_host::spelling::SpellLang::from_locale_tag)
    }

    fn menu_rows(&self, ws: &WorkspaceDoc) -> Vec<menu::Row> {
        match self.menu {
            Some(OpenMenu::Page(_)) => page_menu::shape(
                self.editor.view.view_mode,
                canvas_index(self.editor.view.canvas_bg),
                self.prose_language(ws),
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
            ),
            Some(OpenMenu::Block(_) | OpenMenu::ContextBlock { .. }) => {
                blockmenu::block_rows(&self.command_state(ws))
            }
            Some(OpenMenu::List(_) | OpenMenu::ContextList { .. }) => {
                blockmenu::list_rows(&self.command_state(ws))
            }
            Some(OpenMenu::More(_) | OpenMenu::ContextMore { .. }) => {
                blockmenu::more_rows(&self.command_state(ws))
            }
            Some(OpenMenu::Window(_)) => window_menu::shape(self.pinned, self.maximized),
            Some(OpenMenu::Context(_)) => context_menu::rows(
                self.editor.body.focused().is_some(),
                self.move_selection_offered(),
                self.editor.attribute_selection_is_mixed(),
                self.focused_task_context(),
                self.context_reminder_node.is_some(),
                self.focused_table_context(),
                self.focused_diagram_context(),
                self.context_pill_kind(),
                self.context_spell().as_ref(),
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
            ),
            None => Vec::new(),
        }
    }

    /// The open menu's view of the frozen spelling snapshot, if any.
    fn context_spell(&self) -> Option<context_menu::SpellContext<'_>> {
        self.context_spelling
            .as_ref()
            .map(|snap| context_menu::SpellContext {
                suggestions: snap.suggestions.as_deref(),
                only: snap.only,
            })
    }

    /// Pour freshly arrived suggestions into the open context menu's
    /// placeholder — the reopen-to-see-them experience was rejected at
    /// acceptance. The panel may grow (a suggestion can be wider than the
    /// placeholder) but never shrinks under the pointer.
    fn fill_context_suggestions(&mut self) -> bool {
        if !matches!(self.menu, Some(OpenMenu::Context(_))) {
            return false;
        }
        let Some(snap) = self.context_spelling.as_mut() else {
            return false;
        };
        if snap.suggestions.is_some() {
            return false;
        }
        let Some(list) = self.spelling.cached_suggestions(&snap.word) else {
            return false;
        };
        snap.suggestions = Some(list.to_vec());
        let rows = self.menu_shape();
        let width = menu::measure_width(&mut self.editor.view.engine, &rows);
        if let Some(OpenMenu::Context(menu)) = self.menu.as_mut() {
            menu.width = menu.width.max(width);
            // The first suggestion is the natural target now that it exists.
            if rows.first().is_some_and(menu::Row::selectable) {
                menu.hover = Some(0);
            }
        }
        true
    }

    fn context_pill_kind(&self) -> context_menu::PillContext {
        if let Some((_, reminder)) = self.context_reminder.as_ref() {
            context_menu::PillContext::Reminder {
                paused: reminder.paused,
                handled: reminder.handled,
            }
        } else if let Some((row, attr, _, trashed, dangling)) = self.context_attachment_pill {
            let _ = (row, attr);
            if dangling {
                context_menu::PillContext::AttachmentMissing
            } else if trashed {
                context_menu::PillContext::AttachmentTrashed
            } else {
                context_menu::PillContext::Attachment
            }
        } else if self.context_document_pill.is_some() {
            context_menu::PillContext::Document
        } else if let Some((_, hit)) = &self.context_attribute_chip {
            use koru_editor::chips::ChipSpec;
            context_menu::PillContext::Attribute(match hit.spec {
                ChipSpec::Tag(_) => context_menu::ChipMenu::Tag,
                ChipSpec::Color(_) => context_menu::ChipMenu::Color,
                ChipSpec::Due(_) => context_menu::ChipMenu::Due,
                ChipSpec::Priority(_) => context_menu::ChipMenu::Priority,
            })
        } else {
            context_menu::PillContext::None
        }
    }

    /// Whether *Move to document…* can honestly be offered: there is a
    /// selection, and this view is hosted where the request will be drained.
    /// The standalone document window has no container behind it, so nothing
    /// there answers a `wants_move_selection` — offering the row would be a
    /// button that does nothing.
    fn move_selection_offered(&self) -> bool {
        !self.window_chrome && self.editor.has_move_selection()
    }

    /// The open menu's rows *without* reading the document.
    ///
    /// A pointer move has no `&WorkspaceDoc` to hand — `View::mouse_move` does
    /// not take one — but hit-testing does not need the document either: what
    /// the caret says only ever reaches a row's [`Trailing`](menu::Trailing),
    /// and a tick at the right-hand end changes neither a row's height nor
    /// whether it can be clicked. Pinned by
    /// `every_menu_lays_out_the_same_with_and_without_the_document`, so a row
    /// that ever *did* change shape with the caret would fail rather than
    /// making the hover and the click disagree by one.
    fn menu_shape(&self) -> Vec<menu::Row> {
        match self.menu {
            // `None`/`En` deliberately: neither value changes a row's height
            // or count, and shape has no document to read (see the doc note).
            Some(OpenMenu::Page(_)) => page_menu::shape(
                self.editor.view.view_mode,
                canvas_index(self.editor.view.canvas_bg),
                None,
                koru_i18n::Lang::En,
            ),
            Some(OpenMenu::Block(_) | OpenMenu::ContextBlock { .. }) => {
                blockmenu::block_rows(&docview::State::default())
            }
            Some(OpenMenu::List(_) | OpenMenu::ContextList { .. }) => {
                blockmenu::list_rows(&docview::State::default())
            }
            Some(OpenMenu::More(_) | OpenMenu::ContextMore { .. }) => {
                blockmenu::more_rows(&docview::State::default())
            }
            // The window's own state, not the caret's — so this needs no
            // document either, and shape and rows are the same list.
            Some(OpenMenu::Window(_)) => window_menu::shape(self.pinned, self.maximized),
            Some(OpenMenu::Context(_)) => context_menu::rows(
                self.editor.body.focused().is_some(),
                self.move_selection_offered(),
                self.editor.attribute_selection_is_mixed(),
                self.focused_task_context(),
                self.context_reminder_node.is_some(),
                self.focused_table_context(),
                self.focused_diagram_context(),
                self.context_pill_kind(),
                self.context_spell().as_ref(),
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
            ),
            None => Vec::new(),
        }
    }

    /// What the caret says about the commands: which draw pressed, which are
    /// dead. Read once per frame, and once per click.
    pub fn command_state(&self, ws: &WorkspaceDoc) -> docview::State {
        docview::State::read(ws, self.body(), self.body().armed())
    }

    fn menu_key(&mut self, ws: &mut WorkspaceDoc, key: &Key) -> bool {
        let rows = self.menu_rows(ws);
        match key {
            Key::Named(NamedKey::Escape) => {
                self.menu = None;
                self.bubble_hover = None;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(open) = self.menu.as_mut() {
                    open.menu_mut().step(&rows, 1);
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(open) = self.menu.as_mut() {
                    open.menu_mut().step(&rows, -1);
                }
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(i) = self.menu.and_then(|m| m.menu().hover) {
                    self.choose_menu(ws, i, None);
                }
            }

            _ => {}
        }
        true
    }

    /// Apply the choice on row `i` of the open menu, and close it.
    ///
    /// `swatch` is the index of the colour circle a press landed on, when it
    /// landed on one: the page-colour row is the single place a click means
    /// something finer than "this row".
    fn choose_menu(&mut self, ws: &mut WorkspaceDoc, i: usize, swatch: Option<usize>) {
        let open = self.menu;
        let context_pill = self.context_pill_kind();
        let reminder_node = self.context_reminder_node.take();
        let reminder = self.context_reminder.take();
        // Closed *before* acting, so an arm that opens a submenu (List) leaves
        // its own menu shut rather than racing it. The page settings put
        // themselves back: every row there is a setting whose result you are
        // looking at, so it stays up until it is dismissed.
        self.menu = None;
        match open {
            Some(OpenMenu::Page(_)) => {
                self.menu = open;
                let mode = self.editor.view.view_mode;
                let choice = match swatch {
                    Some(n) => Some(page_menu::Choice::Canvas(n)),
                    None => page_menu::choice_at(mode, i),
                };
                let _ = i;
                match choice {
                    Some(page_menu::Choice::Mode(next)) => {
                        Editing::set_view_mode(self, next);
                        self.settle();
                    }
                    Some(page_menu::Choice::Canvas(n)) => {
                        self.editor.view.canvas_bg =
                            docview::canvas_bg_presets().get(n).copied().flatten();
                    }
                    Some(page_menu::Choice::CycleCanvas) => {
                        self.editor.view.canvas_bg =
                            docview::cycle_canvas_bg(self.editor.view.canvas_bg);
                    }
                    Some(page_menu::Choice::CycleFont(role)) => {
                        // Cycles the role's family global. The shell's
                        // `invalidate_everywhere` relays the change to every
                        // window after the click returns.
                        use page_menu::FontRole;
                        match role {
                            FontRole::Body => theme::set_body_font(theme::body_font().next()),
                            FontRole::Headings => {
                                theme::set_headings_font(theme::headings_font().next())
                            }
                            FontRole::Code => theme::set_code_font(theme::code_font().next()),
                        }
                    }
                    Some(page_menu::Choice::CycleProseLanguage) => {
                        // The one page-menu choice that writes to the document
                        // (D4): the prose language is a shared fact, and the
                        // spelling controller notices the root's meta change
                        // through its tracker and rechecks in the new tongue.
                        if let Some(root) = self.editor.body.root {
                            let next = page_menu::next_prose_language(self.prose_language(ws));
                            let _ = ws.set_prose_language(root, next.map(|l| l.tag()));
                        }
                    }
                    None => {}
                }
            }
            Some(OpenMenu::Block(_) | OpenMenu::ContextBlock { .. }) => {
                match blockmenu::block_choice_at(i) {
                    Some(blockmenu::BlockChoice::Text) => {
                        Editing::act(self, ws, Action::SetHeading(None));
                        self.settle();
                    }
                    Some(blockmenu::BlockChoice::Heading(h)) => {
                        Editing::act(self, ws, Action::SetHeading(Some(h)));
                        self.settle();
                    }
                    None => {}
                }
            }
            Some(OpenMenu::List(_) | OpenMenu::ContextList { .. }) => {
                if let Some(cmd) = blockmenu::list_choice_at(i) {
                    Editing::act(self, ws, cmd.action());
                    self.settle();
                }
            }
            Some(OpenMenu::More(_) | OpenMenu::ContextMore { .. }) => {
                if let Some(cmd) = blockmenu::more_choice_at(i) {
                    Editing::act(self, ws, cmd.action());
                    self.settle();
                }
            }
            // Straight out to the shell — the same four requests the caption's
            // buttons raised, which is what keeps the menu and the buttons the
            // same feature rather than two.
            Some(OpenMenu::Window(_)) => {
                if let Some(choice) = window_menu::choice_at(self.pinned, self.maximized, i) {
                    self.window_request = Some(match choice {
                        window_menu::Choice::TogglePin => WindowRequest::TogglePin,
                        window_menu::Choice::Minimise => WindowRequest::Minimise,
                        window_menu::Choice::ToggleMaximise => WindowRequest::ToggleMaximise,
                        window_menu::Choice::SnapLeft => {
                            WindowRequest::Place(snap::Placement::Half(snap::Side::Left))
                        }
                        window_menu::Choice::SnapRight => {
                            WindowRequest::Place(snap::Placement::Half(snap::Side::Right))
                        }
                        window_menu::Choice::Close => WindowRequest::Close,
                    });
                }
            }
            Some(OpenMenu::Context(_)) => {
                let on_doc = self.context_document_pill;
                let on_file = self.context_attachment_pill;
                let pill_kind = context_pill;
                let choice = context_menu::choice_at(
                    i,
                    pill_kind,
                    self.context_spell().as_ref(),
                    self.focused_diagram_context(),
                );
                koru_workspace_host::attach_debug!(
                    "document choose_menu i={i} pill_kind={pill_kind:?} choice={choice:?} on_file={:?}",
                    on_file.map(|(row, attr, file, trashed, dangling)| {
                        (row, attr, file.to_storage(), trashed, dangling)
                    })
                );
                let action = match choice {
                    Some(context_menu::Choice::SpellingMenu) => {
                        self.open_spelling_submenu(open, i);
                        None
                    }
                    Some(context_menu::Choice::Spelling(n)) => {
                        if let Some(snap) = self.context_spelling.clone()
                            && let Some(replacement) =
                                snap.suggestions.as_ref().and_then(|s| s.get(n)).cloned()
                        {
                            self.apply_spelling(ws, &snap, &replacement);
                        }
                        None
                    }
                    Some(context_menu::Choice::SpellingIgnore) => {
                        if let Some(snap) = self.context_spelling.take() {
                            let Self {
                                spelling, editor, ..
                            } = self;
                            spelling.ignore_once(editor, &snap.word);
                        }
                        None
                    }
                    Some(context_menu::Choice::SpellingIgnoreDocument) => {
                        if let Some(snap) = self.context_spelling.take() {
                            let Self {
                                spelling, editor, ..
                            } = self;
                            spelling.ignore_in_document(editor, &snap.word);
                        }
                        None
                    }
                    Some(context_menu::Choice::SpellingAddWord) => {
                        if let Some(snap) = self.context_spelling.take() {
                            let Self {
                                spelling, editor, ..
                            } = self;
                            spelling.add_to_dictionary(editor, &snap.word);
                        }
                        None
                    }
                    Some(context_menu::Choice::SpellingAddWorkspaceWord) => {
                        if let Some(snap) = self.context_spelling.take() {
                            let Self {
                                spelling, editor, ..
                            } = self;
                            spelling.add_to_workspace(editor, &snap.word);
                        }
                        None
                    }
                    Some(context_menu::Choice::Open) => {
                        if let Some((row, attr)) = on_doc {
                            self.follow_document_pill(ws, row, attr, OpenWhere::SameTab);
                        }
                        None
                    }
                    Some(context_menu::Choice::OpenInNewTab) => {
                        if let Some((row, attr)) = on_doc {
                            self.follow_document_pill(ws, row, attr, OpenWhere::BackgroundTab);
                        }
                        None
                    }
                    Some(context_menu::Choice::OpenInNewWindow) => {
                        if let Some((row, attr)) = on_doc {
                            self.follow_document_pill(ws, row, attr, OpenWhere::NewWindow);
                        }
                        None
                    }
                    Some(context_menu::Choice::OpenFile) => {
                        if let Some((_, _, file, _, dangling)) = on_file {
                            if dangling {
                                koru_workspace_host::attach_debug!(
                                    "document OpenFile ignored — file missing"
                                );
                            } else {
                                koru_workspace_host::attach_debug!(
                                    "document sets wants_file_action Open file={}",
                                    file.to_storage()
                                );
                                self.editor.view.wants_file_action =
                                    Some((file, koru_editor::FileAction::Open));
                            }
                        } else {
                            koru_workspace_host::attach_debug!(
                                "document OpenFile but on_file is None — no-op"
                            );
                        }
                        None
                    }
                    Some(context_menu::Choice::RevealFile) => {
                        if let Some((_, _, file, _, dangling)) = on_file {
                            if dangling {
                                koru_workspace_host::attach_debug!(
                                    "document RevealFile ignored — file missing"
                                );
                            } else {
                                koru_workspace_host::attach_debug!(
                                    "document sets wants_file_action Reveal file={}",
                                    file.to_storage()
                                );
                                self.editor.view.wants_file_action =
                                    Some((file, koru_editor::FileAction::Reveal));
                            }
                        } else {
                            koru_workspace_host::attach_debug!(
                                "document RevealFile but on_file is None — no-op"
                            );
                        }
                        None
                    }
                    Some(context_menu::Choice::SaveFileAs) => {
                        if let Some((_, _, file, _, dangling)) = on_file {
                            if dangling {
                                koru_workspace_host::attach_debug!(
                                    "document SaveFileAs ignored — file missing"
                                );
                            } else {
                                koru_workspace_host::attach_debug!(
                                    "document sets wants_file_action Save file={}",
                                    file.to_storage()
                                );
                                self.editor.view.wants_file_action =
                                    Some((file, koru_editor::FileAction::Save));
                            }
                        } else {
                            koru_workspace_host::attach_debug!(
                                "document SaveFileAs but on_file is None — no-op"
                            );
                        }
                        None
                    }
                    Some(context_menu::Choice::RestoreFile) => {
                        if let Some((_, _, file, _, dangling)) = on_file {
                            if dangling {
                                koru_workspace_host::attach_debug!(
                                    "document RestoreFile ignored — file missing"
                                );
                            } else {
                                koru_workspace_host::attach_debug!(
                                    "document sets wants_file_action Restore file={}",
                                    file.to_storage()
                                );
                                self.editor.view.wants_file_action =
                                    Some((file, koru_editor::FileAction::Restore));
                            }
                        } else {
                            koru_workspace_host::attach_debug!(
                                "document RestoreFile but on_file is None — no-op"
                            );
                        }
                        None
                    }
                    Some(context_menu::Choice::RemoveAttachment) => {
                        if let Some((row, attr, _, _, _)) = on_file {
                            let ok =
                                Editing::remove_attachment_pill(&mut self.editor, ws, row, attr);
                            koru_workspace_host::attach_debug!(
                                "document RemoveAttachment row={row} attr={attr} ok={ok}"
                            );
                        } else {
                            koru_workspace_host::attach_debug!(
                                "document RemoveAttachment but on_file is None — no-op"
                            );
                        }
                        None
                    }
                    Some(context_menu::Choice::Cut) => Some(Action::Cut),
                    Some(context_menu::Choice::Copy) => Some(Action::Copy),
                    Some(context_menu::Choice::Paste) => Some(Action::Paste),
                    Some(context_menu::Choice::PastePlain) => Some(Action::PastePlain),
                    Some(context_menu::Choice::MoveToDocument) => {
                        self.editor.open_move_selection_picker(ws);
                        None
                    }
                    // The view only asks. Starting an agent needs the gateway,
                    // a device and a store, none of which is a view's, so the
                    // shell drains this and opens the one delegation modal —
                    // the same surface `/delegate` reaches.
                    Some(context_menu::Choice::Delegate) => {
                        let node = self.context_reminder_node.or_else(|| {
                            koru_editor::Editing::body(&self.editor)
                                .focused()
                                .map(|row| row.id)
                        });
                        // **What the row says becomes what was asked for.**
                        // Somebody who right-clicks a line and chooses
                        // *Delegate* has written the objective already; the
                        // modal opening empty asks them to write it twice.
                        // Read here because this is where the document is.
                        // **`koru_editor`'s reading, not a second copy.**
                        // `/delegate` reads the same row the same way (see
                        // `koru_editor::objective`), and two answers to "what
                        // does this line say" is exactly the pair that drifts.
                        let objective = node
                            .map(|node| koru_editor::row_words(ws, node))
                            .unwrap_or_default();
                        koru_editor::Editing::request_delegation(&mut self.editor, node, objective);
                        None
                    }
                    Some(context_menu::Choice::Tags) => {
                        self.open_selection_attr_picker(ws, koru_editor::AttrPicker::Tags);
                        None
                    }
                    Some(context_menu::Choice::Color) => {
                        self.open_selection_attr_picker(ws, koru_editor::AttrPicker::Color);
                        None
                    }
                    Some(context_menu::Choice::Due) => {
                        self.open_selection_attr_picker(ws, koru_editor::AttrPicker::Due);
                        None
                    }
                    Some(context_menu::Choice::Priority) => {
                        self.open_selection_attr_picker(ws, koru_editor::AttrPicker::Priority);
                        None
                    }
                    // **Both go through the editor's own chip calls**, so the
                    // menu cannot drift from the gestures it describes.
                    // `edit_chip` is the left-click for a tag, a colour and a
                    // priority, and the door the date grid kept when the due
                    // chip's own click became *go to the task*; `clear_chip` is
                    // every chip's `x` lane except the due one, whose `x` now
                    // removes the task instead — so *Clear due date* has to
                    // name the clearing call rather than the `x`.
                    Some(context_menu::Choice::EditChip | context_menu::Choice::ClearChip) => {
                        if let Some((row, hit)) = self.context_attribute_chip.clone() {
                            if matches!(choice, Some(context_menu::Choice::ClearChip)) {
                                Editing::clear_chip(&mut self.editor, ws, row, hit);
                            } else {
                                Editing::edit_chip(&mut self.editor, ws, row, hit);
                            }
                        }
                        None
                    }
                    // **The node under the pointer, whichever door was used.**
                    // A chip's menu names its own row; the row's menu names the
                    // focused one. Both are the same node in practice, and
                    // reading it from the chip first is what keeps the chip
                    // menu about the chip.
                    Some(context_menu::Choice::GoToTask) => {
                        let node = self
                            .context_attribute_chip
                            .as_ref()
                            .and_then(|(row, _)| self.editor.body.rows.get(*row))
                            .or_else(|| self.editor.body.focused())
                            .map(|row| row.id);
                        self.editor.view.wants_task_view = node;
                        None
                    }
                    Some(context_menu::Choice::CompleteTask) => {
                        self.complete_focused_task(ws);
                        self.settle();
                        None
                    }
                    // **Both ask; neither acts.** What *Remove from Tasks*
                    // takes is not on screen (the marker, the lifecycle and the
                    // due), and what *Delete row* takes is the writing — see
                    // `ask_remove_from_tasks` and `ask_delete_row`.
                    Some(context_menu::Choice::RemoveFromTasks) => {
                        self.ask_remove_from_tasks(ws);
                        None
                    }
                    Some(context_menu::Choice::DeleteRow) => {
                        self.ask_delete_row(ws);
                        None
                    }
                    Some(context_menu::Choice::Remind) => {
                        if let Some(node) = reminder_node {
                            self.editor.request_reminder(ws, node);
                        }
                        None
                    }
                    Some(context_menu::Choice::ReminderEdit)
                    | Some(context_menu::Choice::ReminderPause)
                    | Some(context_menu::Choice::ReminderResume)
                    | Some(context_menu::Choice::ReminderDelete) => {
                        if let Some((node, reminder)) = reminder {
                            let action = match choice.expect("matched above") {
                                context_menu::Choice::ReminderEdit => {
                                    koru_editor::reminders::ContextAction::Edit
                                }
                                context_menu::Choice::ReminderPause => {
                                    koru_editor::reminders::ContextAction::Pause
                                }
                                context_menu::Choice::ReminderResume => {
                                    koru_editor::reminders::ContextAction::Resume
                                }
                                context_menu::Choice::ReminderDelete => {
                                    koru_editor::reminders::ContextAction::Delete
                                }
                                _ => unreachable!(),
                            };
                            self.editor.view.wants_reminder_context =
                                Some(koru_editor::reminders::ContextRequest {
                                    node,
                                    reminder_id: reminder.reminder_id,
                                    action,
                                });
                        }
                        None
                    }
                    Some(context_menu::Choice::Table(edit)) => {
                        self.editor.edit_table(ws, edit);
                        None
                    }
                    // The same thing Enter on the selected drawing does; on a
                    // block that does not draw, the caret lands on the line
                    // its reason names.
                    Some(context_menu::Choice::DiagramEditText) => {
                        if let Some(diagram) = self.focused_diagram_context() {
                            self.editor.open_diagram_text(diagram.node);
                        }
                        None
                    }
                    // The view only asks: the save dialog, the raster and the
                    // file are the shell's. Drained by
                    // `View::take_wants_save_diagram_as` on the next
                    // maintenance pass, with the drawing and the page's title.
                    Some(context_menu::Choice::SaveDiagramAs) => {
                        if let Some(diagram) = self.focused_diagram_context().filter(|d| d.drawn) {
                            self.editor.view.wants_save_diagram_as =
                                Some(koru_editor::SaveDiagramAs { node: diagram.node });
                        }
                        None
                    }
                    None => None,
                };
                if let Some(action) = action {
                    Editing::act(self, ws, action);
                    self.settle();
                }
                self.bubble_hover = None;
            }
            None => {}
        }
    }

    /// Open one of the attribute pickers over the block selection, or the
    /// focused row — the context menu's Tags…/Color…/Due…/Priority… rows.
    /// Multi-select applies to every selected root in one undo step.
    ///
    /// A selection spanning the title *and* body rows opens nothing: the rows
    /// that would have opened it are dimmed with a reason, and this is the
    /// second half of that refusal, so a keyboard route to the same row cannot
    /// get past the drawing (ADR-187).
    fn open_selection_attr_picker(&mut self, ws: &WorkspaceDoc, which: koru_editor::AttrPicker) {
        let Some(anchor) = self.editor.body.focused_id() else {
            return;
        };
        let Some(nodes) = self.editor.attribute_targets(anchor) else {
            return;
        };
        self.editor.open_attr_picker_over(ws, nodes, which);
    }

    /// The explore rows' Attributes submenu, once the document is the active
    /// one: open the picker over its **root node** — a document-level
    /// attribute lives on the title row's node (docs/features/trackable/DECISIONS.md Q2).
    pub fn open_document_attr_picker(
        &mut self,
        ws: &WorkspaceDoc,
        which: koru_editor::AttrPicker,
    ) -> bool {
        let Some(root) = self.editor.body.root else {
            return false;
        };
        self.editor.open_attr_picker_over(ws, vec![root], which)
    }

    /// Re-read the document's name into [`title`](Self::title).
    ///
    /// Called where a rename lands — construction and `adopt` — rather than per
    /// frame. An unnamed document says *Untitled*, matching the placeholder the
    /// title row shows in a tab.
    fn refresh_title(&mut self, ws: &WorkspaceDoc) {
        // Before the guard: the count is of *asks*, so it reads the same whether
        // this view draws its own caption or sits in a container's tab.
        self.title_refreshes = self.title_refreshes.saturating_add(1);
        if !self.window_chrome {
            return;
        }
        let name = self.page_name(ws);
        self.title = if name.is_empty() {
            "Untitled".to_string()
        } else {
            name
        };
    }

    /// The page's name: the root's text, trimmed. Empty when the page has
    /// none — the caption says *Untitled* for that, and a saved diagram's
    /// default file name says *Diagram*.
    pub fn page_name(&self, ws: &WorkspaceDoc) -> String {
        self.editor
            .body
            .root
            .and_then(|root| ws.read(root).ok())
            .and_then(|n| n.content.as_text().map(str::to_string))
            .map(|t| t.trim().to_string())
            .unwrap_or_default()
    }

    /// Whether the caption's window-menu button is currently showing its menu.
    ///
    /// Asked by drawing (which glyph, and whether the hover is red) and by
    /// `mouse_down` (whether this press is a close rather than an open).
    pub fn window_menu_open(&self) -> bool {
        matches!(self.menu, Some(OpenMenu::Window(_)))
    }

    /// Step Page → Narrow → Full → Page.
    fn cycle_view_mode(&mut self) {
        let next = match self.editor.view.view_mode {
            ViewMode::Page => ViewMode::Narrow,
            ViewMode::Narrow => ViewMode::Full,
            ViewMode::Full => ViewMode::Page,
        };
        Editing::set_view_mode(self, next);
    }

    /// The chrome first, then the page, then the shared editor.
    ///
    /// The order is the whole of the rule: everything this view draws over the
    /// rows has to claim a press before the editor sees it, or a click on a
    /// bubble button would place a caret in whatever line is scrolled under it.
    /// ...and the same for a press: clicking away from the name prompt commits
    /// it. See [`key`](Self::key) for why the refresh is on the inherent method.
    /// Replace a misspelled word with a chosen suggestion — one ordinary
    /// editor transaction (the emoji-commit shape): one undo step, the caret
    /// after the replacement, Ctrl+Z landing back on the word, and normal
    /// Loro sync. No special CRDT semantics (spec §18).
    fn apply_spelling(
        &mut self,
        ws: &mut WorkspaceDoc,
        snap: &spelling::SpellSnapshot,
        replacement: &str,
    ) {
        // The menu froze a moment ago; apply only if the word is still
        // exactly where it said. A peer edit in between makes this a no-op,
        // never a wrong-place replacement.
        let still_there = self
            .editor
            .body
            .row(snap.node)
            .and_then(|row| row.text.get(snap.range.clone()))
            .is_some_and(|text| text == snap.word);
        if !still_there {
            return;
        }
        ws.checkpoint();
        let _ = ws.set_next_undo_caret(snap.node, snap.range.start);
        let Ok(caret) = ws.replace_text(snap.node, snap.range.clone(), replacement) else {
            return;
        };
        ws.checkpoint();
        self.editor.body.focus_on(snap.node);
        self.editor.body.refresh(ws, snap.node);
        self.editor.place_caret(caret);
        self.editor.view.active = true;
        self.editor.view.desired_x = None;
        self.editor.settle();
    }

    /// Drill from the full menu's *Spelling…* row into the corrections — the
    /// same `OpenMenu::Context` machinery, reopened beside the clicked row as
    /// the corrections-only list (`SpellContext::only`).
    fn open_spelling_submenu(&mut self, from: Option<OpenMenu>, i: usize) {
        let Some(from) = from else { return };
        if self.context_spelling.is_none() {
            return;
        }
        let vp = self.editor.view.viewport;
        let lang =
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
        let shape = |view: &Self, spell: Option<&context_menu::SpellContext<'_>>| {
            context_menu::rows(
                view.editor.body.focused().is_some(),
                view.move_selection_offered(),
                view.editor.attribute_selection_is_mixed(),
                view.focused_task_context(),
                view.context_reminder_node.is_some(),
                view.focused_table_context(),
                view.focused_diagram_context(),
                view.context_pill_kind(),
                spell,
                lang,
            )
        };
        // The clicked row's band, measured against the menu as it was.
        let at = {
            let spell = self.context_spell();
            let rows = shape(self, spell.as_ref());
            let rect = from.menu().row_rect(&rows, vp, i);
            (rect.x + 24.0, rect.y)
        };
        if let Some(snap) = self.context_spelling.as_mut() {
            snap.only = true;
        }
        let spell = self.context_spell();
        let rows = shape(self, spell.as_ref());
        let width = menu::measure_width(&mut self.editor.view.engine, &rows);
        let mut sub = context_menu::open(at, vp.0, self.chrome_h(), width, false);
        sub.step(&rows, 1);
        self.menu = Some(OpenMenu::Context(sub));
    }

    pub fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        self.editor.view.pointer = (x, y);
        if self.editor.view.overlay.is_open() || y < self.chrome_h() {
            let changed = self.menu.take().is_some();
            self.bubble_hover = None;
            self.context_document_pill = None;
            self.context_attachment_pill = None;
            self.context_attribute_chip = None;
            self.context_reminder_node = None;
            self.context_reminder = None;
            return changed;
        }

        // Rows live in the centred content column (Page/Narrow). The same
        // mapping mouse_down uses — without it, a right-click on a pill in Page
        // mode misses the pill and opens the plain formatting menu instead.
        let cx = x - self.editor.view.content_x;
        let reminder = Editing::reminder_at(&mut self.editor, x, y);
        // Run and schedule pills both belong to container-owned product
        // surfaces rather than this document's own menu.
        if reminder.is_none()
            && let Some(schedule) = Editing::schedule_pill_at(&self.editor, ws, cx, y)
        {
            self.menu = None;
            self.bubble_hover = None;
            self.context_document_pill = None;
            self.context_attachment_pill = None;
            self.context_attribute_chip = None;
            self.context_reminder_node = None;
            self.context_reminder = None;
            self.editor.view.wants_schedule_menu = Some(schedule);
            return true;
        }
        // **A run pill's menu is the container's.** It lists what the run
        // produced, and this view can name none of that: an artifact is not a
        // node. So the ask travels and nothing opens here — the same shape
        // the left button already has, where the pill asks for the run and
        // the container puts it on screen.
        if reminder.is_none()
            && let Some(run) = Editing::run_pill_at(&self.editor, ws, cx, y)
        {
            // A menu that was up is dismissed, and none opens here.
            self.menu = None;
            self.bubble_hover = None;
            self.context_document_pill = None;
            self.context_attachment_pill = None;
            self.context_attribute_chip = None;
            self.context_reminder_node = None;
            self.context_reminder = None;
            self.editor.view.wants_run_menu = Some(run);
            return true;
        }
        let document_pill = reminder
            .is_none()
            .then(|| self.document_pill_at(ws, cx, y))
            .flatten();
        let attachment_pill = if reminder.is_none() && document_pill.is_none() {
            Editing::attachment_pill_at(&self.editor, ws, cx, y).and_then(|(row, attr)| {
                let file = Editing::file_behind(&self.editor, ws, row, attr)?;
                let info = self
                    .editor
                    .body
                    .rows
                    .get(row)
                    .and_then(|r| r.pills.get(&attr));
                let trashed = info.is_some_and(|info| info.trashed);
                let dangling = info.is_some_and(|info| info.dangling);
                Some((row, attr, file, trashed, dangling))
            })
        } else {
            None
        };
        koru_workspace_host::attach_debug!(
            "document context_menu at=({x:.1},{y:.1}) cx={cx:.1} doc_pill={:?} attach_pill={:?}",
            document_pill.map(|(r, a)| (r, a)),
            attachment_pill.map(|(r, a, f, t, d)| (r, a, f.to_storage(), t, d))
        );
        self.context_document_pill = document_pill;
        self.context_attachment_pill = attachment_pill;
        // **After the pills, because a pill is not a chip and they do not
        // overlap.** Asked through the same `chips::chip_at` the press and the
        // hover go through, which is what stops the menu and the picker
        // disagreeing about where a chip is.
        self.context_attribute_chip =
            (document_pill.is_none() && attachment_pill.is_none() && reminder.is_none())
                .then(|| Editing::attribute_chip_at(&mut self.editor, cx, y))
                .flatten()
                .and_then(|hit| {
                    let row = self.editor.body.row_at_y(y + self.editor.view.scroll)?;
                    Some((row, hit))
                });
        self.context_reminder = reminder;

        let doc_y = y + self.editor.view.scroll;
        let row_at_point = self.editor.body.row_at_y(doc_y);
        self.context_reminder_node = row_at_point
            .and_then(|index| self.editor.body.rows.get(index))
            .map(|row| row.id);
        let content_x = cx;
        let table_cell_at_point = row_at_point.and_then(|i| {
            let row = self.editor.body.rows.get(i)?;
            let visual = row.table.as_ref()?;
            if row.kind != koru_core::Kind::Table
                || self.editor.view.table_source == Some(row.id)
                || visual.grid.is_none()
            {
                return None;
            }
            let scroll_x = self
                .editor
                .view
                .table_scroll
                .get(&row.id)
                .copied()
                .unwrap_or(0.0);
            koru_editor::table::context_cell_at(row, visual, content_x, doc_y, scroll_x)
                .map(|(table_row, column)| (row.id, table_row, column))
        });
        let over_text_selection = Editing::selection_rects(&self.editor)
            .into_iter()
            .any(|rect| rect.contains(x, y));
        let over_row_selection = row_at_point
            .and_then(|i| self.editor.body.rows.get(i))
            .is_some_and(|row| row.selected);

        // A right-click inside a selection acts on that selection. Elsewhere it
        // moves only the caret: no drag starts and a pill under the pointer does
        // not open its picker merely because its context menu was requested.
        if !over_text_selection
            && !over_row_selection
            && let Some(i) = row_at_point
        {
            self.editor.body.clear_selection();
            self.editor.body.focus = i;
            self.editor.view.active = true;
            self.editor.view.desired_x = None;
            self.editor.view.dragging = false;
            Editing::disarm(&mut self.editor);

            if self.context_reminder.is_some() {
                // Reminder metadata is not text. Preserve the caret and block
                // selection exactly as left-click does.
            } else if let Some((row, attr)) = document_pill {
                self.editor.view.selected_pill = Some((row, attr));
            } else if let Some((row, attr, _, _, _)) = attachment_pill {
                self.editor.view.selected_pill = Some((row, attr));
            } else if let Some((id, table_row, column)) = table_cell_at_point {
                self.editor.focus_table_cell(id, table_row, column);
            } else {
                let row = &mut self.editor.body.rows[i];
                if row.image.is_none() && row.kind.shape() == koru_core::node::Shape::Text {
                    row.place_caret_from_point(content_x - row.text_x(), doc_y - row.y);
                }
            }
        }

        self.bubble = None;
        self.bubble_pinned = None;
        self.bubble_hover = None;
        // Did the click land on a misspelled word? Over a selection the caret
        // deliberately did not move, so the layout is asked read-only which
        // byte the press hit — a double-clicked word still gets its
        // suggestions. The word and range freeze now; a cold suggestion cache
        // shows its placeholder, filled in place when the worker answers.
        let spell_hit =
            (document_pill.is_none() && attachment_pill.is_none() && table_cell_at_point.is_none())
                .then(|| {
                    let row = self.editor.body.rows.get(row_at_point?)?;
                    let at = if over_text_selection || over_row_selection {
                        row.byte_at_point(content_x - row.text_x(), doc_y - row.y)
                    } else {
                        row.caret()
                    };
                    let range = self.spelling.issue_at(row.id, at)?;
                    let word = row.text.get(range.clone())?.to_string();
                    Some((row.id, range, word))
                })
                .flatten();
        // Without a selection the click is *about* the word: the menu is the
        // corrections alone, cursor-style. Over a selection the usual menu
        // keeps its toolbar and the corrections lead it.
        let spell_only = spell_hit.is_some() && !over_text_selection && !over_row_selection;
        self.context_spelling = spell_hit.map(|(node, range, word)| {
            let mut snap = self.spelling.snapshot(node, range, &word);
            snap.only = spell_only;
            snap
        });
        let pill = self.context_pill_kind();
        let lang =
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
        let spell = self.context_spell();
        let rows = context_menu::rows(
            self.editor.body.focused().is_some(),
            self.move_selection_offered(),
            self.editor.attribute_selection_is_mixed(),
            self.focused_task_context(),
            self.context_reminder_node.is_some(),
            self.focused_table_context(),
            self.focused_diagram_context(),
            pill,
            spell.as_ref(),
            lang,
        );
        // Measure once at open and freeze: rows recompute each frame but the
        // panel must not resize under the pointer (koru-ui rule). The one
        // exception grows it: suggestions landing in an open placeholder
        // (`fill_context_suggestions`).
        let width = menu::measure_width(&mut self.editor.view.engine, &rows);
        // **The toolbar is for text, and a pill is not text.** `spell_only` is
        // the same withholding for a third reason: a spelling menu is about one
        // word, not about a run's formatting.
        let with_toolbar = !spell_only && !pill.owns_the_menu();
        let mut open = context_menu::open(
            (x, y),
            self.editor.view.viewport.0,
            self.chrome_h(),
            width,
            with_toolbar,
        );
        open.step(&rows, 1);
        self.menu = Some(OpenMenu::Context(open));
        true
    }

    pub fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        let handled = self.mouse_down_inner(ws, x, y, ctrl);
        self.drain_remove_from_tasks_request(ws);
        self.refresh_title(ws);
        handled
    }

    /// **Raise the question the due chip's `×` asked**, if it was pressed.
    ///
    /// The editor cannot put a dialog up — it owns no modals — so the `×` sets
    /// [`wants_remove_from_tasks`] and this turns it into the same
    /// confirmation the menu's *Remove from Tasks* row raises. One question,
    /// one wording, whichever door was used.
    ///
    /// Drained here rather than deeper in, because a press is the only thing
    /// that can set it: `mouse_down_inner` returns from a dozen places and a
    /// drain inside would have to be repeated at every one of them.
    ///
    /// [`wants_remove_from_tasks`]: koru_editor::ViewState::wants_remove_from_tasks
    fn drain_remove_from_tasks_request(&mut self, ws: &WorkspaceDoc) {
        let Some(node) = self.editor.view.wants_remove_from_tasks.take() else {
            return;
        };
        // The `×` does not move the caret, and `ask_remove_from_tasks_on` reads
        // the focused row — so the row the chip belongs to is put in focus
        // first, or the question would be about wherever the keyboard was.
        self.editor.body.focus_on(node);
        self.ask_remove_from_tasks(ws);
    }

    fn mouse_down_inner(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        // The modal outranks the caption too: a press outside it does *not*
        // dismiss (`Confirm::mouse_down` answers `Keep`), so a stray click can
        // never answer a question about somebody's work in either direction.
        let viewport = self.editor.view.viewport;
        let answered = match &mut self.confirm {
            Some((confirm, _)) => {
                Some(confirm.mouse_down(&mut self.editor.view.engine, viewport, x, y))
            }
            None => None,
        };
        if let Some(out) = answered {
            self.settle_confirm(ws, out);
            return true;
        }
        let search_was_open = self.search.is_open();
        if self.search.mouse_down(ws, &mut self.editor, x, y) {
            return true;
        }
        if search_was_open {
            // Dismiss and keep routing this same press to what is underneath.
            self.search.close(ws, &mut self.editor, true);
        }
        // **Before the caption, or the chip drags the window.** It sits inside
        // the caption band, and the bare row's answer is *drag* — so a press
        // that reached the shared hit test first would move the window instead
        // of answering the question the chip is there to be asked.
        if self.offline_chip().is_some_and(|c| c.contains(x, y)) {
            self.offline_open = !self.offline_open;
            return true;
        }
        // Any other press puts it away, wherever it lands — a popover that
        // outlives a click in the page is one somebody has to hunt for.
        self.offline_open = false;
        // The caption outranks everything, including an open picker: a window
        // you cannot close because a menu is showing is worse than a menu that
        // dismisses itself.
        if let Some(which) = self.caption_hit(x, y) {
            // **The ⋮ button has two meanings, one click apart.** Shut, it
            // opens the window menu. Open, it is drawn as an ✕ and hovers red —
            // and an ✕ in that corner closes the window, which is the meaning
            // the red promises. The same rule the container follows.
            if which == CaptionHit::Control(CaptionAction::WindowMenu) {
                if self.window_menu_open() {
                    self.menu = None;
                    self.window_request = Some(WindowRequest::Close);
                } else {
                    let button = caption::window_menu_button(self.editor.view.viewport.0);
                    self.menu = button.map(|r| OpenMenu::Window(window_menu::open(r)));
                }
                return true;
            }
            self.window_request = Some(match which {
                CaptionHit::Control(CaptionAction::Minimise) => WindowRequest::Minimise,
                CaptionHit::Control(CaptionAction::ToggleMaximise) => WindowRequest::ToggleMaximise,
                CaptionHit::Control(CaptionAction::Close) => WindowRequest::Close,
                // The bare row. Dragging is the default for a title bar, and it
                // has to be asked for inside this press — see `WindowRequest`.
                _ => WindowRequest::Drag,
            });
            return true;
        }

        // A menu, while open: a click inside chooses; a click anywhere else
        // closes it and falls through to the document, the editor-overlay
        // idiom rather than the tab menu's modal-eat.
        if let Some(open) = self.menu {
            if open.is_context_surface()
                && let Some(panel) = self.context_toolbar_panel()
            {
                if let Some(button) = bubble::hit(panel, x, y) {
                    self.bubble_hover = None;
                    match button {
                        bubble::Button::BlockType => {
                            if let Some(anchor) = bubble::button_rect(panel, button) {
                                self.menu = Some(OpenMenu::ContextBlock {
                                    menu: blockmenu::open(anchor),
                                    toolbar: panel,
                                });
                            }
                        }
                        bubble::Button::List => {
                            if let Some(anchor) = bubble::button_rect(panel, button) {
                                self.menu = Some(OpenMenu::ContextList {
                                    menu: blockmenu::open_list(anchor),
                                    toolbar: panel,
                                });
                            }
                        }
                        bubble::Button::More => {
                            if let Some(anchor) = bubble::button_rect(panel, button) {
                                self.menu = Some(OpenMenu::ContextMore {
                                    menu: blockmenu::open(anchor),
                                    toolbar: panel,
                                });
                            }
                        }
                        bubble::Button::Cmd(_) => {
                            // A direct command finishes the context gesture;
                            // only a submenu needs the toolbar to remain.
                            self.menu = None;
                            self.press_bubble(ws, panel, button);
                        }
                    }
                    return true;
                }
                if panel.contains(x, y) {
                    return true;
                }
            }
            let vp = self.editor.view.viewport;
            let rows = self.menu_rows(ws);
            let m = open.menu();
            if let Some(i) = m.item_at(&rows, vp, x, y) {
                // The page-colour row is the one place a press means something
                // finer than "this row": a swatch under the pointer is picked
                // directly rather than cycled to.
                let swatch = matches!(open, OpenMenu::Page(_))
                    .then(|| {
                        page_menu::is_color_row(self.editor.view.view_mode, i)
                            .then(|| {
                                m.swatch_at(&rows, vp, i, docview::canvas_bg_presets().len(), x)
                            })
                            .flatten()
                    })
                    .flatten();
                self.choose_menu(ws, i, swatch);
                return true;
            }
            self.menu = None;
            // A press on the button that opened it toggles it shut rather than
            // reopening it on the way back up.
            if self.opener_at(x, y).is_some() {
                return true;
            }
        }

        if !self.editor.view.overlay.is_open() {
            // The selection bubble, and the two menus it opens.
            if let Some(panel) = self.bubble_panel() {
                if let Some(button) = bubble::hit(panel, x, y) {
                    self.press_bubble(ws, panel, button);
                    return true;
                }
                if panel.contains(x, y) {
                    // The gaps between its buttons are still the bubble's: a
                    // press there must not reach through and drop the selection
                    // the bubble exists to act on.
                    return true;
                }
            }
            // Anything else is a press outside it, which retires a summoned
            // one. A selected one needs no help: the press moves the caret and
            // the selection goes with it.
            self.bubble_pinned = None;
            // The title's `…`, and a row's `+`. Both live in the affordance
            // lane, which scrolls with the content, so they are asked in
            // document space the way the grip is.
            let doc_y = y + self.editor.view.scroll;
            let cx = x - self.editor.view.content_x;
            if let Some(r) =
                Editing::title_menu_rect(&self.editor).filter(|r| r.contains(cx, doc_y))
            {
                let anchor = Rect::new(
                    r.x + self.editor.view.content_x,
                    r.y - self.editor.view.scroll,
                    r.w,
                    r.h,
                );
                self.menu = Some(OpenMenu::Page(page_menu::open(anchor)));
                return true;
            }
            if let Some(i) = self.editor.body.row_at_y(doc_y).filter(|&i| {
                Editing::plus_rect(&self.editor, i).is_some_and(|r| r.contains(cx, doc_y))
            }) {
                // Alt-click adds above, plain click below. Nothing is inserted
                // here: the menu carries the target and the direction, and the
                // block is made only if a row is chosen.
                let above = self.editor.view.mods.alt_key();
                Editing::open_insert_menu(&mut self.editor, i, above);
                return true;
            }

            // Anywhere in this view's own caption band is swallowed rather than
            // placing a caret in a row scrolled underneath it.
            if y < self.chrome_h() {
                return true;
            }
            // The scrollbar gutter claims its column even at the bottom of the
            // band, so a click there pages rather than appending a line. Without
            // this the two would fight only in that sliver — but predictably
            // losing it is better than a click that sometimes means one thing.
            let in_scrollbar = self
                .editor
                .view
                .scrollbar_geom(koru_editor::rowlayout::content_height(&self.editor.body))
                .is_some_and(|g| g.in_gutter(x, y));
            // **The status band is chrome, not the end of the page.** Without
            // `over_content` a press on it fell into the branch below and
            // appended a paragraph — the strip names the fonts and the sync
            // state, and clicking it wrote to the document. Found by a test that
            // had only ever run against the task outline, which asked this
            // question and was deleted with the view.
            if !in_scrollbar
                && self.editor.view.over_content(y)
                && self.editor.body.row_at_y(doc_y).is_none()
                && self.editor.body.below_last_row(doc_y)
            {
                self.append_point(ws);
                return true;
            }
        }
        Editing::mouse_down(self, ws, x, y, ctrl)
    }

    /// A bubble button was pressed.
    fn press_bubble(&mut self, ws: &mut WorkspaceDoc, panel: Rect, button: bubble::Button) {
        match button {
            bubble::Button::BlockType => {
                if let Some(r) = bubble::button_rect(panel, button) {
                    self.menu = Some(OpenMenu::Block(blockmenu::open(r)));
                }
            }
            bubble::Button::List => {
                if let Some(r) = bubble::button_rect(panel, button) {
                    self.menu = Some(OpenMenu::List(blockmenu::open_list(r)));
                }
            }
            bubble::Button::More => {
                if let Some(r) = bubble::button_rect(panel, button) {
                    self.menu = Some(OpenMenu::More(blockmenu::open(r)));
                }
            }
            bubble::Button::Cmd(cmd) => {
                Editing::act(self, ws, cmd.action());
                self.settle();
            }
        }
    }

    /// The control an open menu hangs from, if the pointer is on it. Used to
    /// make a second press on an opener close the menu rather than reopen it.
    fn opener_at(&self, x: f32, y: f32) -> Option<()> {
        let doc_y = y + self.editor.view.scroll;
        let cx = x - self.editor.view.content_x;
        if Editing::title_menu_rect(&self.editor).is_some_and(|r| r.contains(cx, doc_y)) {
            return Some(());
        }
        let panel = self.bubble_panel()?;
        matches!(
            bubble::hit(panel, x, y),
            Some(bubble::Button::BlockType | bubble::Button::List | bubble::Button::More)
        )
        .then_some(())
    }

    /// The caption's window-menu button opens on hover, and closes when the pointer
    /// leaves both it and the panel it opened.
    ///
    /// **The panel is not "outside".** It hangs directly under the button, so
    /// the walk from one to the other is continuous — a menu that closed the
    /// moment the pointer left the button would be unreachable. The container's
    /// copy of this is `MainView::track_window_menu_hover`.
    fn track_window_menu_hover(&mut self, x: f32, y: f32) -> bool {
        if !self.window_chrome {
            return false;
        }
        let button = caption::window_menu_button(self.editor.view.viewport.0);
        let on_button = button.is_some_and(|r| r.contains(x, y));
        if self.window_menu_open() {
            let vp = self.editor.view.viewport;
            let rows = self.menu_shape();
            let panel = self.menu.map(|m| m.menu().panel(&rows, vp));
            let in_panel = panel.is_some_and(|r| r.contains(x, y));
            // The strip between the button and its panel counts as inside — see
            // the container's copy of this.
            let in_corridor = panel.zip(button).is_some_and(|(p, b)| {
                let left = p.x.min(b.x);
                let right = (p.x + p.w).max(b.x + b.w);
                x >= left && x <= right && y >= b.y + b.h && y <= p.y
            });
            if !on_button && !in_panel && !in_corridor {
                self.menu = None;
                return true;
            }
            return false;
        }
        if on_button && self.menu.is_none() {
            self.menu = button.map(|r| OpenMenu::Window(window_menu::open(r)));
            return true;
        }
        false
    }

    /// The chrome redraws on any move within it, so a button lights up.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.editor.view.pointer = (x, y);
        // The modal owns the pointer while it is up — its two buttons light on
        // hover, and nothing underneath may.
        let viewport = self.editor.view.viewport;
        if let Some((confirm, _)) = &mut self.confirm {
            return confirm.mouse_move(&mut self.editor.view.engine, viewport, x, y);
        }
        if self.search.is_open() {
            self.search.mouse_move(&mut self.editor, x, y);
            return true;
        }
        if self.search.mouse_move(&mut self.editor, x, y) {
            return true;
        }
        // Before the branch below, or the menu would follow the pointer out of
        // the caption and never let go. Leaving is always part of a motion
        // stream, so the next event settles whatever is under it.
        if self.track_window_menu_hover(x, y) {
            self.editor.view.pointer = (x, y);
            return true;
        }
        if self.menu.is_some() {
            let vp = self.editor.view.viewport;
            let rows = self.menu_shape();
            let toolbar = self.context_toolbar_panel();
            let toolbar_button = toolbar.and_then(|p| bubble::hit(p, x, y));
            let over_toolbar = toolbar.is_some_and(|p| p.contains(x, y));
            if toolbar_button != self.bubble_hover.map(|(b, _)| b) {
                self.bubble_hover = toolbar_button.map(|b| (b, Instant::now()));
            }
            let hit = (!over_toolbar)
                .then(|| self.menu.and_then(|m| m.menu().item_at(&rows, vp, x, y)))
                .flatten();
            if let Some(open) = self.menu.as_mut() {
                open.menu_mut().hover = hit;
            }
            // **A menu covers what is under it** — the whole window, not only
            // the panel rect. Leaving the panel used to fall through to the
            // editor: the I-beam came back and the row under the menu lit up
            // while TURN INTO was still open. Dismiss is a *click* outside
            // (`mouse_down`), not a move.
            self.editor.view.pointer = (x, y);
            let _ = self.editor.view.hover.take();
            return true;
        }
        // The bubble is chrome over the rows: a move within it repaints so its
        // buttons light up, and must not be read as a text drag.
        let panel = self.bubble_panel();
        let over_bubble = panel.is_some_and(|p| p.contains(x, y));
        // The tooltip's clock restarts whenever the button under the pointer
        // changes, and stops the moment the pointer leaves.
        let button = panel.and_then(|p| bubble::hit(p, x, y));
        if button != self.bubble_hover.map(|(b, _)| b) {
            self.bubble_hover = button.map(|b| (b, Instant::now()));
        }
        if over_bubble {
            // The bubble is an opaque pointer surface as well as a painted
            // one. Do not let the row below begin its `+` tooltip clock just
            // because the bubble itself has no hover state for its gaps.
            let _ = self.editor.view.hover.take();
            return true;
        }
        let chrome = self.chrome_h();
        let was = self.editor.view.pointer.1;
        let over_chrome = y < chrome || was < chrome;
        Editing::mouse_move(self, x, y) || over_chrome
    }

    /// A double click on the caption maximises, the way a title bar does.
    /// Anywhere else it is the editor's — the name, a word, a pill.
    ///
    /// Chrome drawn over the rows owns the pointer the same way
    /// [`Self::mouse_down`] does: a rapid second click on Bold is a double-click
    /// to the shell, and without this gate the editor would word-select the line
    /// under the bubble and drop the selection the bar is for.
    pub fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        // A double-click is two presses, and the first has already been
        // swallowed by the modal; letting the second select a word underneath
        // it would move the caret out from under the question.
        if self.confirm.is_some() {
            return true;
        }
        if self.search.double_click(&mut self.editor, x, y) {
            return true;
        }
        if self.caption_hit(x, y) == Some(CaptionHit::Caption) {
            self.window_request = Some(WindowRequest::ToggleMaximise);
            return true;
        }
        if let Some(open) = self.menu {
            let rows = self.menu_shape();
            let vp = self.editor.view.viewport;
            if open.menu().panel(&rows, vp).contains(x, y)
                || self
                    .context_toolbar_panel()
                    .is_some_and(|panel| panel.contains(x, y))
            {
                return true;
            }
        }
        if self
            .bubble_panel()
            .is_some_and(|panel| panel.contains(x, y))
        {
            return true;
        }
        Editing::mouse_double_click(self, ws, x, y)
    }

    /// What a point lands on in this view's own title bar — always `None` when
    /// there is no title bar because a container drew one.
    fn caption_hit(&self, x: f32, y: f32) -> Option<CaptionHit> {
        self.window_chrome
            .then(|| caption::hit(x, y, self.editor.view.viewport.0))
            .flatten()
    }

    /// Put the caret at the end of the document, ready to type.
    ///
    /// Reuses the last line when it is empty rather than stacking blank rows —
    /// clicking the page twice should not leave two of them behind.
    fn append_point(&mut self, ws: &mut WorkspaceDoc) {
        self.editor.view.active = true;
        // **The caret is moving, so a row selection made somewhere else is
        // over.** Leaving it behind draws rows highlighted while the caret sits
        // in a paragraph below them, and every chord that acts on a selection —
        // Ctrl+X, Ctrl+D, Tab — then acts on rows the person is no longer
        // looking at. `deactivating_drops_the_row_selection_too` is the same
        // rule arrived at from the other direction.
        self.editor.body.clear_selection();
        ws.checkpoint();

        let last = self
            .editor
            .body
            .rows
            .last()
            .map(|r| (r.id, r.text.is_empty()));
        let target = match last {
            Some((id, true)) => Some(id),
            Some((id, false)) => self.editor.body.append_row(ws, Some(id)),
            // An empty document: give it a first line to type on.
            None => {
                let parent = match self.editor.body.root {
                    Some(root) => koru_core::schema::Parent::Node(root),
                    None => koru_core::schema::Parent::Root,
                };
                let id = koru_core::node::Node::paragraph("")
                    .ok()
                    .and_then(|node| ws.insert(parent, node).ok());
                if id.is_some() {
                    self.editor.body.rebuild(ws);
                }
                id
            }
        };
        if let Some(id) = target {
            self.editor.body.focus_on(id);
            self.place_caret(usize::MAX);
        }
        self.reflow();
        self.scroll_to_caret();
    }
}

/// Which page-colour preset `canvas` is, for the swatch row's tick. An unknown
/// colour reads as the first entry ("Auto"), the same fall-back
/// [`docview::cycle_canvas_bg`] uses.
fn canvas_index(canvas: Option<[u8; 4]>) -> usize {
    docview::canvas_bg_presets()
        .iter()
        .position(|c| *c == canvas)
        .unwrap_or(0)
}

/// **A document has no footer, in any of its presentations.**
///
/// The strip named the three font roles, the replica id and whether Ctrl and
/// Shift were down: ambient state about the *app*, drawn under a document that
/// it said nothing about, in a band taken off every one of them. `ViewState`
/// keeps `status_h` as a field rather than reading `theme::STATUS_H` directly,
/// because `koru-editor` still draws a footer for a view that asks for one —
/// this is every current caller declining, said once instead of five times.
fn without_footer(editor: &mut Editor) {
    editor.view.status_h = 0.0;
}

// --------------------------------------------------------- doc view-pref helpers
//
// Resolve and write a document's per-document view prefs against `Settings`.
// Keyed by the active workspace — a `NodeId` is only meaningful inside the
// document that minted it, the same reason `collapsed` is keyed by workspace.

/// Apply the global default view mode and canvas colour to a freshly built
/// editor. Used by `open`/`first` so a new document opens the way the welcome
/// page is set to, without those builders having to take `Settings`.
fn apply_default_view_prefs(editor: &mut Editor) {
    let mode = theme::default_view_mode();
    editor.view.view_mode = mode;
    editor.view.canvas_bg = theme::default_canvas_bg();
    editor.view.content_x = mode.content_x(editor.view.viewport.0);
}

/// Set `root`'s override to `prefs` (creating the workspace's row if need be),
/// and drop it again when `prefs` is empty — a document back on the defaults
/// leaves no trace, not even an empty workspace row.
fn upsert_doc_prefs(settings: &mut Settings, root: NodeId, prefs: DocViewPrefs) {
    let Some(ws) = settings.workspace else {
        return;
    };
    let existing_idx = settings.doc_view_prefs.iter().position(|(w, _)| *w == ws);
    if prefs.is_empty() {
        // Remove the node if it had an override, and drop the workspace row if
        // that empties it — never leave a `(ws, [])` behind.
        if let Some(idx) = existing_idx {
            settings.doc_view_prefs[idx].1.retain(|(n, _)| *n != root);
            if settings.doc_view_prefs[idx].1.is_empty() {
                settings.doc_view_prefs.remove(idx);
            }
        }
        return;
    }
    let idx = existing_idx.unwrap_or_else(|| {
        settings.doc_view_prefs.push((ws, Vec::new()));
        settings.doc_view_prefs.len() - 1
    });
    let entries = &mut settings.doc_view_prefs[idx].1;
    if let Some(slot) = entries.iter_mut().find(|(n, _)| *n == root) {
        slot.1 = prefs;
    } else {
        entries.push((root, prefs));
    }
}

/// Whether a key is one of the things a person does with a page they are
/// *reading*, rather than writing.
///
/// **An allowlist, and that direction is the whole point.** Listing the keys
/// that mutate would mean listing the editor's key map, and it would be wrong on
/// the day somebody adds a chord to it — wrong silently, in a way you find by
/// typing into a release note and watching it change. Default-deny means a key
/// nobody thought about does nothing, which is the safe half of being wrong.
///
/// What is allowed is the four things reading actually needs: moving the caret,
/// selecting, copying, and finding. Everything else — every character, Enter,
/// Tab, Backspace, Delete, paste, cut, undo, and every formatting chord — answers
/// `false` and the view never sees it. Nothing is said about it: a page that is
/// not yours quietly not accepting a keystroke is better than a notice on every
/// keypress.
///
/// Container chords resolve *before* this, so Ctrl+W, Ctrl+T and tab cycling are
/// unaffected — this only ever sees what was already destined for the document.
fn reading_key(key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
    if matches!(
        key,
        Key::Named(
            NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::PageUp
                | NamedKey::PageDown
                // Dismisses the find panel, and closes a menu that should not
                // have been open. Never edits.
                | NamedKey::Escape
        )
    ) {
        // Modifiers included on purpose: Shift extends a selection and the
        // primary modifier moves by word, and both are reading.
        return true;
    }
    // Select all, copy, and find. `Ctrl+Insert` is copy's other spelling and
    // costs one line to keep working for the people who use it.
    if koru_keys::has_primary(mods) {
        if matches!(key, Key::Named(NamedKey::Insert)) {
            return true;
        }
        if let Some(letter) = keymap::ctrl_letter(key, text) {
            return matches!(letter, 'a' | 'c' | 'f');
        }
    }
    false
}

#[cfg(test)]
mod readonly_tests {
    use super::*;

    fn ctrl() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    #[test]
    fn reading_a_page_still_works() {
        for named in [
            NamedKey::ArrowUp,
            NamedKey::ArrowDown,
            NamedKey::ArrowLeft,
            NamedKey::ArrowRight,
            NamedKey::Home,
            NamedKey::End,
            NamedKey::PageUp,
            NamedKey::PageDown,
            NamedKey::Escape,
        ] {
            assert!(
                reading_key(&Key::Named(named), None, ModifiersState::empty()),
                "{named:?} is how you read"
            );
            assert!(
                reading_key(&Key::Named(named), None, ModifiersState::SHIFT),
                "{named:?} with Shift selects, which is also reading"
            );
        }
        for letter in ['a', 'c', 'f'] {
            let key = Key::Character(letter.to_string().into());
            assert!(
                reading_key(&key, Some(&letter.to_string()), ctrl()),
                "select all, copy and find are reading"
            );
        }
    }

    /// The list this test walks is not exhaustive and cannot be. That is why the
    /// function is an allowlist: the assertion that matters is the last one.
    #[test]
    fn nothing_that_writes_gets_through() {
        assert!(
            !reading_key(
                &Key::Character("x".into()),
                Some("x"),
                ModifiersState::empty()
            ),
            "typing a character"
        );
        for named in [
            NamedKey::Enter,
            NamedKey::Tab,
            NamedKey::Backspace,
            NamedKey::Delete,
            NamedKey::Space,
        ] {
            assert!(
                !reading_key(&Key::Named(named), None, ModifiersState::empty()),
                "{named:?} changes the document"
            );
        }
        for letter in ['v', 'x', 'z', 'y', 'b', 'i', 'u', 'k', 'd'] {
            let key = Key::Character(letter.to_string().into());
            assert!(
                !reading_key(&key, Some(&letter.to_string()), ctrl()),
                "Ctrl+{letter} writes, formats or undoes"
            );
        }
    }

    /// **The property, rather than a list.** A chord added to the editor
    /// tomorrow is refused here without anybody remembering to come back.
    #[test]
    fn an_unlisted_key_is_refused_by_default() {
        assert!(!reading_key(&Key::Character("q".into()), Some("q"), ctrl()));
        assert!(!reading_key(
            &Key::Named(NamedKey::F9),
            None,
            ModifiersState::empty()
        ));
    }
}
