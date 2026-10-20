// SPDX-License-Identifier: MPL-2.0

//! What a window showing editable lines needs, whatever the lines are.
//!
//! Deliberately at the top level and **not** under `editor/`: a view can own one
//! without owning an `Editor`, and putting it in the editor's directory would
//! have such a view importing from the module it is being separated from.
//!
//! The test for whether something belongs here is "would a calendar need it?" —
//! a font cache, an open picker, a scroll offset and what the pointer is over
//! are all true of any view that draws a list. The *rows* are not, which is why
//! this holds none of them.
//!
//! Only one picker can be open in a window at a time. That is the reason the
//! overlay lives here rather than on each line: `Option<Overlay>` per paragraph
//! would make N options where at most one may ever be `Some`.

use koru_core::schema::Cutting;
use koru_core::{AttrId, NodeId, Parent};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::ViewMode;
use koru_ui::cursor;
use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;
use winit::keyboard::ModifiersState;
use winit::window::CursorIcon;

use crate::components::Overlay;

/// Tags the host has to create in the workspace **system document**, and the
/// nodes waiting for each.
///
/// A queue rather than one slot, because a paste can carry several tags this
/// workspace has never heard of while the chooser's create row can only ever
/// ask for one. [`take`](Self::take) pops the front, so the host's per-turn
/// drain is unchanged — it services one request, the resulting edit asks for a
/// repaint, and the next turn takes the next.
///
/// Duplicate names are folded on the way in ([`koru_core::tags::fold_key`]):
/// one paste that tags six rows `#home` is one request, not six.
#[derive(Default, PartialEq, Eq, Debug)]
pub struct CreateTagRequests(std::collections::VecDeque<(Vec<NodeId>, String)>);

impl CreateTagRequests {
    /// Queue a creation, or add these nodes to the one already queued under
    /// the same name.
    pub fn push(&mut self, nodes: Vec<NodeId>, name: String) {
        let key = koru_core::tags::fold_key(&name);
        if let Some((waiting, _)) = self
            .0
            .iter_mut()
            .find(|(_, queued)| koru_core::tags::fold_key(queued) == key)
        {
            for node in nodes {
                if !waiting.contains(&node) {
                    waiting.push(node);
                }
            }
            return;
        }
        self.0.push_back((nodes, name));
    }

    /// The next request, if any. Named `take` because that is what every other
    /// `wants_*` field answers to, and the host's call site says nothing about
    /// there being a queue behind it.
    pub fn take(&mut self) -> Option<(Vec<NodeId>, String)> {
        self.0.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Armed long-press on a body emoji, waiting for the tone strip or a short click.
#[derive(Clone, Debug)]
pub struct BodyTonePress {
    pub node: NodeId,
    pub range: Range<usize>,
    pub expected: String,
    pub since: Instant,
    pub origin: (f32, f32),
    /// Window-space rect of the glyph, for anchoring the strip.
    pub cell: Rect,
}

/// Skin-tone strip open over a body emoji.
#[derive(Clone, Debug)]
pub struct BodyToneStrip {
    pub node: NodeId,
    pub range: Range<usize>,
    pub expected: String,
    pub tones: Vec<&'static str>,
    pub cell: Rect,
    pub hover: Option<usize>,
}

/// A smooth scroll in flight.
///
/// `view.scroll` **is** the animated value — it starts at `from` and is
/// advanced toward `target` by `advance_scroll_anim` each frame. `None` at
/// rest, which is what the shell's `animating` check reads.
pub struct ScrollAnim {
    /// Where the scroll was when the animation began.
    pub from: f32,
    /// Where it is headed.
    pub target: f32,
    /// When it began.
    pub started: Instant,
}

/// **What a view would hand to an agent**, for the shell to open the modal on.
///
/// Deliberately not the container's `DelegationOrigin`: that vocabulary belongs
/// to the surface's owner, and a view naming it would be linking the delegation
/// crates to say two facts. These are the two facts.
/// Not `Copy`: it carries the row's words, which are a person's text and
/// belong to whoever takes the request rather than to every reader of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DelegateRequest {
    /// The row the gesture started on, when there is one. `None` on a view
    /// with nothing focused; the modal then opens with the document alone.
    pub node: Option<NodeId>,
    /// Whether the gesture was a document selection's right-click row rather
    /// than the slash command. The two pre-fill the same modal differently,
    /// and only the container can tell them apart afterwards.
    pub from_selection: bool,
    /// **The words on that row**, when the gesture was made on a row that had
    /// any. A person right-clicking a line and choosing *Delegate* has
    /// already written what they want done — asking them to write it again
    /// into an empty box is the surface refusing to read what is in front of
    /// it.
    ///
    /// **`/delegate` reads the same field**, from the same row, once the
    /// command itself has been erased: `/delegate` typed at the end of a line
    /// somebody wrote is the same gesture with the same words behind it, and
    /// the two arriving with different pre-fills was the only way the two
    /// document gestures differed. A row holding nothing but the command
    /// leaves this empty, which is the empty box that gesture always opened.
    ///
    /// Read by the view rather than by the container, because the row's text
    /// is the document's and a container that reached for it would be reading
    /// a document it does not hold.
    pub objective: String,
}

/// Where a document the user asked to open should appear.
///
/// A view can only *ask* — opening a tab needs the tab set and opening a window
/// needs the event loop, neither of which is a view's to touch. This is the
/// vocabulary of that request, and the container decides what each one means.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum OpenWhere {
    /// Show it in the tab already on it, or a new tab if none is. What clicking
    /// a pill has always done, and what the shell does with a bare request.
    #[default]
    Reuse,
    /// **This tab**, following the reference in place — the browser's plain
    /// click, and what Enter on a selected pill means. Distinct from
    /// [`Reuse`](Self::Reuse): that one opens a *second* tab when nothing is
    /// already showing the document, which is not "the same tab".
    SameTab,
    /// A new tab, even if another already shows this document — Ctrl+Enter, the
    /// modifier that means "somewhere else" everywhere else in the app.
    NewTab,
    /// A new tab beside the current one that does not become active — Ctrl+click
    /// on a document pill. The container reuses a tab already on the document
    /// rather than inserting a second copy, and still does not switch to it.
    BackgroundTab,
    /// A new window — Shift+Enter, and Shift+click on a document pill.
    NewWindow,
}

/// The clickable URL hover chip: destination plus copy and edit.
#[derive(Clone, Debug)]
pub struct LinkChip {
    pub rect: Rect,
    pub copy: Rect,
    pub edit: Rect,
    pub url: String,
    pub row: usize,
    pub attr: AttrId,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum LinkChipHit {
    None,
    Body,
    Copy,
    Edit,
}

impl LinkChip {
    pub fn hit(&self, x: f32, y: f32) -> LinkChipHit {
        if self.copy.contains(x, y) {
            LinkChipHit::Copy
        } else if self.edit.contains(x, y) {
            LinkChipHit::Edit
        } else if self.rect.contains(x, y) {
            LinkChipHit::Body
        } else {
            LinkChipHit::None
        }
    }
}

/// Where a selection move is headed. A view can only ask: the destination is
/// a document of the *workspace* (or another workspace entirely), and writing
/// into one is the shell's.
#[derive(Clone, Debug)]
pub enum MoveSelectionDestination {
    /// The picker's "Add new document" row: create this document, then move.
    /// `follow: false` (Ctrl+Enter or Shift+Enter) performs the move but
    /// stays here — no tab for the new document, no navigation. `child: true`
    /// (Shift+Enter) creates it as a **child** of the document the selection
    /// left, rather than its next sibling.
    NewPage {
        title: String,
        follow: bool,
        child: bool,
    },
    /// An existing document — with a heading node when the picker drilled
    /// into one, meaning "first under that heading".
    Existing {
        target: crate::trigger::DocumentTarget,
        title: String,
    },
}

/// The selection's content in model terms, captured when the picker opened —
/// what the container forwards to the shell.
#[derive(Clone, Debug)]
pub enum MoveSelectionPayload {
    /// Whole subtrees: the selection roots, title excluded, none nested.
    Rows(Vec<NodeId>),
    /// Part of one row, already sliced to the selected range.
    Inline {
        runs: Vec<koru_core::TextRun>,
        attrs: Vec<koru_core::node::Attr>,
    },
}

/// How to replace the source once the destination write has succeeded.
#[derive(Clone, Debug)]
pub struct PendingSelectionMove {
    pub payload: MoveSelectionPayload,
    /// The selected text range, for an inline move — `commit_pill` replaces
    /// exactly it. `None` for a rows move, which goes through
    /// `replace_subtrees_with_pill` instead.
    pub trigger: Option<crate::trigger::Trigger>,
    /// What "Add new page" falls back to when nothing was typed: the first
    /// selected line, trimmed and truncated, or "Untitled".
    pub default_title: String,
}

/// What the pointer is over, for hover feedback.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hover {
    Grip(usize),
    /// The insert `+` in the affordance lane, left of the grip. Clicking it
    /// opens the same command menu `/` opens, for a block that does not exist
    /// yet.
    Plus(usize),
    Checkbox(usize),
    Pill {
        row: usize,
        attr: AttrId,
    },
    /// Anywhere else within a row.
    Text(usize),
    /// A tone-capable emoji grapheme in the body. Long-press opens the skin-
    /// tone strip; it is still text, so it keeps the I-beam.
    Emoji(usize),
    /// The picture area of an image row. It belongs to the row for hover and
    /// dragging, but is not text and therefore never wears an I-beam.
    Image(usize),
    /// The `…` beside the title, which opens the page-settings menu. Carries no
    /// index: there is only ever one title row, and it is row 0.
    TitleMenu,
    /// The language chip at the corner of a code block. Clicking it opens the
    /// language picker.
    Lang(usize),
    /// The copy icon left of the language chip.
    Copy(usize),
    /// An attribute chip trailing a row's text. `chip` indexes `row.chips`,
    /// which is what decides *which* picker a click opens; the row is carried
    /// so the row still lights the way it does under a pill.
    Chip {
        row: usize,
        chip: usize,
    },
    /// The unfinished-items triangle on a completed task, trailing that row's
    /// chips. It is a statement, not a control — nothing happens on a press —
    /// so it exists as a hover only to carry its tooltip.
    TaskWarning(usize),
    /// One host-supplied reminder pill. Both indices are presentation-local;
    /// the pill carries the Store reminder id used after a press.
    Reminder {
        row: usize,
        reminder: usize,
    },
    /// A table's compact/extended width toggle.
    TableLayout(usize),
    /// A table's source/grid mode toggle.
    TableMode(usize),
    /// A drawn diagram's one control, *Edit text*, at the block's top-right.
    DiagramEdit(usize),
    /// A row of an open overlay panel.
    PanelRow(usize),
    /// A text field inside an open panel.
    ///
    /// Distinct from `PanelRow` only because the pointer shape differs: a field
    /// is text and a menu row is not. Nothing highlights on it, so it carries
    /// no index.
    PanelField,
    /// A button inside an open panel that is **not** one of its rows: an emoji
    /// category, the rename prompt's page icon.
    ///
    /// It has to be a variant of its own rather than a `PanelRow`, because a
    /// `PanelRow` pre-selects the row it names — hovering a category would move
    /// the emoji selection. Carries no index for the same reason.
    PanelControl,
    /// A scrollbar thumb, either a panel's or the document's own. Grabbed and
    /// dragged, so it wears up/down arrows: the shape says *this scrolls*.
    Thumb,
    /// The band above or below a thumb. A press there pages, so it is a thing
    /// you click even though nothing is drawn in it (ADR-28: a thumb with no
    /// track).
    ScrollTrack,
}

impl Hover {
    /// The row this is about, if it is about one. A panel row is not a document
    /// row, so it must not highlight one.
    pub fn row(self) -> Option<usize> {
        match self {
            Self::Grip(i)
            | Self::Plus(i)
            | Self::Checkbox(i)
            | Self::Text(i)
            | Self::Emoji(i)
            | Self::Image(i)
            | Self::Lang(i)
            | Self::Copy(i)
            | Self::TaskWarning(i)
            | Self::TableLayout(i)
            | Self::TableMode(i)
            | Self::DiagramEdit(i)
            | Self::Pill { row: i, .. }
            | Self::Chip { row: i, .. }
            | Self::Reminder { row: i, .. } => Some(i),
            // The title's menu button is chrome hanging off row 0, not the row:
            // hovering it must not light the row up as if it were text.
            Self::TitleMenu
            | Self::PanelRow(_)
            | Self::PanelField
            | Self::PanelControl
            | Self::Thumb
            | Self::ScrollTrack => None,
        }
    }

    /// The pointer shape this warrants.
    ///
    /// **The rule is in `docs/features/design/DESIGN.md`: Koru does not use the hand cursor.**
    /// A press target is identified by its own appearance, not by the pointer
    /// changing over it, so everything pressable here answers
    /// [`cursor::PRESS`] — the plain arrow. What keeps a shape of its own is
    /// what the shape *tells* you: where a caret can go, what can be picked
    /// up, what resizes.
    pub fn cursor_icon(self) -> CursorIcon {
        match self {
            // A prompt field is text like any other, so it wears the I-beam —
            // which is an aiming device as much as an affordance. Tone-capable
            // emoji stay I-beam too, because a caret can sit either side.
            Self::Text(_) | Self::Emoji(_) | Self::PanelField => CursorIcon::Text,
            // The open hand: things you pick up and move.
            Self::Image(_) | Self::Grip(_) => CursorIcon::Grab,
            // Up/down arrows on the thumb: it scrolls the page, not a free drag.
            Self::Thumb => CursorIcon::NsResize,
            // The unfinished-items triangle presses nothing: it is a statement
            // the pointer may rest on to read, so it keeps the shape of the row
            // it sits beside rather than claiming to be a control.
            Self::TaskWarning(_) => CursorIcon::Text,
            // Everything else opens, toggles or inserts something — and says so
            // by how it is drawn, not by the cursor.
            Self::Pill { .. }
            | Self::Chip { .. }
            | Self::Reminder { .. }
            | Self::Lang(_)
            | Self::Copy(_)
            | Self::TableLayout(_)
            | Self::TableMode(_)
            | Self::DiagramEdit(_)
            | Self::Plus(_)
            | Self::TitleMenu
            | Self::PanelRow(_)
            | Self::PanelControl
            | Self::ScrollTrack
            | Self::Checkbox(_) => cursor::PRESS,
        }
    }
}

/// *Save as…* asked on a drawn diagram: which block to save a picture of.
///
/// The view raises it and the shell performs it — the dialog, the raster
/// and the write are the shell's — so it carries only the node. The drawing
/// itself is in the view's [`ViewState::diagrams`] entry for that node.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SaveDiagramAs {
    pub node: NodeId,
}

#[derive(Copy, Clone, Debug)]
pub struct DragRow {
    pub from: usize,
    /// Row the dragged one would land on if released now.
    pub to: usize,
}

/// Geometry of the scrollbar thumb, when there is one.
///
/// `None` (from [`ViewState::scrollbar_geom`]) when the content fits — nothing
/// scrolls, so nothing is drawn and nothing is hit-tested. One source for both
/// drawing and hit-testing, so the two can never disagree about where the thumb
/// is this frame. Pure over [`ViewState`] plus a content height, which is what
/// makes it testable without a vello scene.
///
/// The **gutter** is wider than the drawn **thumb**: hit-testing uses the full
/// lane so the control is easy to grab; paint uses a thinner bar that expands
/// on hover. The lane sits inward of the window resize border.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ScrollbarGeom {
    /// Left edge of the drawn thumb.
    pub thumb_x: f32,
    pub thumb_w: f32,
    pub thumb_top: f32,
    pub thumb_h: f32,
    /// Left edge of the clickable column (may be left of the drawn thumb).
    pub gutter_x: f32,
    pub gutter_w: f32,
    /// The clickable band, top and bottom, in window space. The thumb runs
    /// inside it; clicking above or below the thumb pages rather than grabs.
    pub track_top: f32,
    pub track_bottom: f32,
    /// `content_h - viewport_h`. Zero here means no scrollbar, but the helper
    /// returns `None` before that rather than handing back a zero.
    pub max_scroll: f32,
}

impl ScrollbarGeom {
    /// Whether a window-space point is on the thumb's grab band — full gutter
    /// width, thumb height. A press here starts a drag.
    pub fn on_thumb(self, x: f32, y: f32) -> bool {
        x >= self.gutter_x
            && x <= self.gutter_x + self.gutter_w
            && y >= self.thumb_top
            && y <= self.thumb_top + self.thumb_h
    }

    /// Whether a window-space point is in the clickable gutter — the thumb, or
    /// the band above or below it. A press here is claimed by the scrollbar
    /// before the rows, which is what the empty-page guards check against.
    pub fn in_gutter(self, x: f32, y: f32) -> bool {
        x >= self.gutter_x
            && x <= self.gutter_x + self.gutter_w
            && y >= self.track_top
            && y <= self.track_bottom
    }
}

/// A local-only landing cue for a search result. Unlike authored highlights,
/// this never enters Loro and expires after one deadline-driven redraw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFlash {
    pub node: NodeId,
    pub range: Range<usize>,
    pub until: Instant,
}

/// A local-search background over authored text.
///
/// The range addresses a paragraph's text, except when `table_cell` is set: in
/// that case it addresses the rendered cell text. This is presentation only;
/// source ranges remain with the document-owned search controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHighlight {
    pub node: NodeId,
    pub range: Range<usize>,
    pub table_cell: Option<(usize, usize)>,
    pub active: bool,
    pub activated: Option<Instant>,
}

/// One block displaced by a deferred rich/image paste.
///
/// Loro cannot reliably undo inserted image tree nodes, so the existing paste
/// compensation also needs enough of the overwritten selection to put it back.
/// A cutting retains the complete subtree and its stable identities.
pub struct PasteReplacement {
    pub cutting: Cutting,
    pub parent: Parent,
    pub index: usize,
}

/// The narrow undo hint for one deferred rich/image paste.
pub struct PasteBatch {
    pub inserted: Vec<NodeId>,
    pub replaced: Vec<PasteReplacement>,
}

impl PasteBatch {
    pub fn inserted(ids: Vec<NodeId>) -> Self {
        Self {
            inserted: ids,
            replaced: Vec::new(),
        }
    }
}

/// One misspelled word's underline.
///
/// The same shape as [`SearchHighlight`] and for the same reason: derived,
/// local-only presentation over authored text that never enters Loro. The
/// range addresses the paragraph's text, except when `table_cell` is set — then
/// it addresses that rendered cell's text. The issue store and its revision
/// bookkeeping stay with the document-owned spelling controller; this is only
/// what the next frame draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellingUnderline {
    pub node: NodeId,
    pub range: Range<usize>,
    pub table_cell: Option<(usize, usize)>,
}

pub struct ViewState {
    /// Which row the caret was in when it was last deleted, and where in it.
    ///
    /// Where the caret was before the edit that undo is about to reverse.
    ///
    /// Undoing a delete brings the row back, but nothing in the document says
    /// the caret belonged to it — `rebuild` keeps the caret on whatever row is
    /// focused now, which is the row that took its place. The same gap hits a
    /// split: Enter moves focus onto the new tail, undo deletes that tail, and
    /// without a remembered offset the caret sticks at the tail's old index.
    /// This is what lets the caret go home in both cases.
    ///
    /// Only the most recent one is kept. Loro's `UndoItemMeta` can carry a value
    /// per undo step, which is the complete answer for a burst of deletes
    /// undone one at a time; it needs a channel from here into `koru-core`, so
    /// it is not built yet. Everything else lands on the restored row at offset
    /// zero, which is the "if possible" in the name.
    pub deleted_caret: Option<(NodeId, usize)>,
    pub desired_x: Option<f32>,
    /// The body caret to restore after temporarily jumping to the page title.
    pub page_body_caret: Option<(NodeId, usize)>,
    pub dragging: bool,
    /// Last animation tick while a text/block selection was being dragged at a
    /// viewport edge. `None` outside that gesture, so an idle editor never
    /// wakes on its behalf.
    pub drag_scroll_tick: Option<Instant>,
    /// A press that landed in a prompt field, so the drag that follows extends
    /// *that field's* selection rather than the rows underneath the panel.
    ///
    /// A third, exclusive drag mode: `dragging` is a text drag in a row and
    /// `drag_row` is a reorder. A press sets exactly one of the three.
    ///
    /// A bool rather than a field identifier, because the field that took the
    /// press is by definition the focused one and the prompt already knows
    /// which that is. Storing it twice would let the two disagree.
    pub drag_field: bool,
    /// In-progress row reorder started from a row's drag grip, or directly
    /// from a selected image.
    pub drag_row: Option<DragRow>,
    /// A scrollbar thumb drag in progress. The value is the grab offset — how
    /// far the pointer was below the thumb's top at the press — so the thumb
    /// does not jump to sit under the pointer. A fourth, exclusive drag mode.
    pub scroll_drag: Option<f32>,
    /// An image resize drag in progress. A fifth, exclusive drag mode.
    pub image_resize: Option<ImageResize>,
    /// The cached focused image rect, in document coordinates, when the focus is
    /// on an image — cached by `reflow` so `cursor_icon` can hit-test the resize
    /// handles without reaching into the rows.
    pub focused_image: Option<(NodeId, Rect)>,
    /// The pill the **keyboard** is on: `(row index, attr)`.
    ///
    /// A pill is one cluster, so Left/Right used to step straight over it and
    /// there was no way to reach one without the mouse. Arrowing onto a pill now
    /// stops on it and draws a ring; a second press in the same direction steps
    /// past. Enter opens it — which is the only reason the state exists.
    ///
    /// A view-scoped singleton rather than something on the row, like
    /// `focused_image` above and for the same reason: only one pill can be
    /// selected in a window at a time, and a row index is cheaper to compare
    /// than to search for. It is cleared by [`Editor::disarm`], so every caret
    /// motion, click and structural edit already lets go of it.
    pub selected_pill: Option<(usize, AttrId)>,
    pub search_flash: Option<SearchFlash>,
    /// Every live match for the active document-local search surface.
    pub search_highlights: Vec<SearchHighlight>,
    /// Every misspelled word the next frame should underline.
    pub spelling: Vec<SpellingUnderline>,
    /// A whitespace boundary just finished a word in this node. Set by
    /// `type_text`, taken by the spelling controller so the finished word is
    /// rechecked immediately instead of after the debounce.
    pub word_completed: Option<NodeId>,
    /// "Add to workspace dictionary" was chosen: `(language tag, word)`. Only
    /// the shell can reach the workspace's system document, so this travels
    /// the wants road like tag creation does.
    pub wants_workspace_word: Option<(String, String)>,
    /// The current modifier keys, pushed by the shell so a mouse drag (which has
    /// no modifiers argument) can still tell whether Shift is held.
    pub mods: ModifiersState,

    /// Fonts, and the pill measurement cache. A resource rather than state, but
    /// it is per-window because the cache is sized to what this window draws.
    pub engine: TextEngine,
    /// The one picker that may be open.
    pub overlay: Overlay,
    /// Long-press on a body emoji in progress.
    pub body_tone_press: Option<BodyTonePress>,
    /// Skin-tone strip open over a body emoji.
    pub body_tone_strip: Option<BodyToneStrip>,
    /// Logical size of the drawable area.
    pub viewport: (f32, f32),
    /// How this view lays out its content column. A view that has no page
    /// modes leaves this at [`ViewMode::Full`]. The shared `frame`/`reflow`
    /// read it, so the container never has to know a page mode exists.
    pub view_mode: ViewMode,
    /// The horizontal offset of the centred content column — zero in Full, and
    /// `(viewport.0 − effective_w)/2` in Page/Narrow. Cached whenever the
    /// viewport or the mode changes, so drawing translates by it once and
    /// pointer hit-tests subtract it once.
    pub content_x: f32,
    /// The canvas colour in Page mode — the area *around* the page sheet, not
    /// the page itself. `None` means "follow the theme canvas"
    /// (`theme::p().canvas`), so an unset canvas retints on a theme flip; an
    /// explicit colour survives one. The page sheet itself always uses
    /// `theme::p().bg`. Per-document, resolved before the first reflow. RGBA
    /// bytes so the type crosses into settings without a peniko dependency;
    /// `frame` builds the `Color` at draw time.
    pub canvas_bg: Option<[u8; 4]>,
    pub scroll: f32,
    /// A smooth scroll in flight. `None` at rest. While `Some`, `scroll` is
    /// the animated value and the shell's fast-wake loop is running (ADR-26).
    pub scroll_anim: Option<ScrollAnim>,
    /// Total height of the laid-out rows. Cached by `reflow` so [`cursor_icon`]
    /// can answer the scrollbar question without reaching into the body — the
    /// one thing this struct does not know about. Always fresh after any change
    /// that moves a row, because every such change goes through `reflow`.
    pub content_h: f32,
    /// How many times **this view** has been laid out.
    ///
    /// `Editing::reflow` is the O(rows) call the whole Responsiveness section of
    /// `AGENTS.md` is about not making twice, and "a peer import must not turn
    /// into an extra reflow" is a claim about a *count* that no other state
    /// answers: a second reflow over clean rows shapes nothing and leaves the
    /// layout identical, so it is invisible to every other assertion. So it is
    /// counted, and the tests that hold the rule read it here.
    ///
    /// **Per view rather than process-global**, unlike `rowlayout::shapings`:
    /// the suite runs eight threads against one linked copy of this crate, and a
    /// global would race with whatever else was laying out at the time.
    ///
    /// A saturating increment on a path that is already walking every row — it
    /// costs nothing measurable, and it is deliberately not behind `cfg(test)`,
    /// because a counter the shipping build does not have is a counter that
    /// stops being true.
    pub reflows: u64,
    pub hover: Option<Hover>,
    /// When [`hover`](Self::hover) last became what it is.
    ///
    /// A tooltip that appears the instant the pointer crosses its button
    /// flashes on every pass over the margin, so the `+`'s waits
    /// `theme::motion::tooltip().hover_delay` and then fades in over its `enter_duration`.
    /// Kept beside the hover rather than in the tooltip, because it is a fact
    /// about the *pointer*: anything else that wants to wait for a rest asks
    /// the same field.
    pub hover_since: Instant,
    /// Where the pointer is, in window coordinates. Chrome hit-tests against
    /// this directly, since it sits above the scrolled content.
    pub pointer: (f32, f32),
    /// Whether a line is active. When false there is no caret and no editing —
    /// clicking outside every row drops focus, the way a canvas app does.
    pub active: bool,
    /// Height of the chrome this view draws above the rows.
    pub chrome_h: f32,
    /// Height of the status bar below them — `STATUS_H`, or **zero for a view
    /// that does not draw one**.
    ///
    /// A field rather than the constant, for the same reason `chrome_h` is one:
    /// the band the rows get, where a click still counts as the document, and
    /// where a popover flips to avoid the floor are all measured from it, and a
    /// view without a footer that still reserved its height would leave a strip
    /// of dead space nothing drew into.
    pub status_h: f32,
    /// The canvas gap above the page sheet in Page mode — `PAGE_GAP`, or
    /// **zero for a view whose chrome is already the page's colour**.
    ///
    /// The gap exists to separate the sheet from a *toolbar*: a strip of canvas
    /// saying "the thing above is chrome, the thing below is your page". A
    /// standalone window's bar is the page's own colour, so there is nothing to
    /// separate it from — the gap just reads as the document starting late.
    /// Zero is also how such a view says *my chrome is the page* — the gap
    /// below the sheet reads off it, so the two answers cannot disagree.
    pub page_gap_top: f32,
    /// Something the user should be told, cleared by the next thing they do.
    ///
    /// **Nothing reads this today, and that is a bug rather than a design** —
    /// [TECHDEBT 39](../../../docs/contributing/techdebt/README.md#39-removing-the-document-footer-removed-the-only-place-a-notice-could-appear).
    ///
    /// Its only reader was the document footer's right half, which is gone; the
    /// fourteen places that write it are still here, so an emoji that could not
    /// be inserted, a page icon that would not save and a pasted table that
    /// would not parse all fail in silence. Do not add a fifteenth writer
    /// believing it shows.
    ///
    /// **No timer**, deliberately: the loop only redraws when something
    /// changes, so a message that expired on a clock would need a wake-up of
    /// its own and would often vanish unseen. Being replaced by the next action
    /// is both simpler and better behaved.
    pub notice: Option<String>,
    /// The attribute chip a picker was opened from, in **content-column x and
    /// document y** — the space `chips::chip_boxes` answers in.
    ///
    /// Written whenever a picker opens with [`Anchor::Chip`], and read by
    /// `overlay_panel_rect` for that anchor and nothing else. A rect rather
    /// than a recomputation because the chip's geometry needs the text engine
    /// to measure with, and `overlay_panel_rect` has only `&self` — the same
    /// reason `link_chip` above keeps the rect it was drawn at.
    ///
    /// [`Anchor::Chip`]: crate::trigger::Anchor::Chip
    pub chip_anchor: Option<Rect>,

    // ---------------------------------------------------------- asks of the shell
    //
    // A view can only request these: performing them needs the `Workspace` or a
    // new window, and neither is a view's to touch.
    /// Set by F5 and cleared by the shell. Sharing is a property of the
    /// *document*, so a view can only ask for it.
    pub wants_sharing_toggle: bool,
    /// The last laid-out URL hover chip, so copy/edit can be hit-tested without
    /// redrawing and so the chip stays up while the pointer moves onto it.
    pub link_chip: Option<LinkChip>,
    /// A toast the container should show. One-shot; drained by MainView.
    pub wants_toast: Option<String>,
    /// After opening a URL without activating the browser, ask the shell to
    /// keep this window focused.
    pub wants_stay_focused: bool,
    /// A document the user asked to open, and where they asked for it. Cleared
    /// by the shell.
    ///
    /// One field rather than two, so the node and the disposition cannot
    /// disagree: a stale `where` left behind by an earlier open would send the
    /// next click to a window nobody asked for.
    pub wants_open: Option<(NodeId, OpenWhere)>,
    /// A store-backed document reference, including its workspace route.
    pub wants_entity_open: Option<(crate::trigger::DocumentTarget, OpenWhere)>,
    /// A **delegated run** somebody clicked the pill of, by its Run document
    /// (delegation roadmap D8). Its own field rather than
    /// [`wants_entity_open`](Self::wants_entity_open), because a run opens the
    /// run's views and never a document tab — a `koru-run` document has no
    /// node tree to draw.
    pub wants_run_open: Option<(koru_core::DocumentId, OpenWhere)>,
    /// The run a **right-click** landed on. Its menu lists what the run
    /// produced, and this layer can name none of that: an artifact is not a
    /// node and this view holds no catalog. So the ask travels and the
    /// container builds the menu, exactly as `wants_run_open` travels and
    /// the container opens the tab.
    pub wants_run_menu: Option<koru_core::DocumentId>,
    /// A recurring schedule pill asks the container to reveal its Agents row.
    /// The id stays opaque here so the document editor does not depend on the
    /// delegation domain.
    pub wants_schedule_open: Option<(String, OpenWhere)>,
    /// The same schedule identity when the pill was right-clicked.
    pub wants_schedule_menu: Option<String>,
    /// The entity picker's "Add new page" action, carrying the text typed after
    /// `@` as the proposed title. An empty string means the Ctrl+T draft path.
    pub wants_new_page: Option<String>,
    /// The range occupied by `@query` while the shell creates its document.
    /// Kept stable across the round trip so the successful response can replace
    /// exactly that text with the real routed pill.
    pub pending_new_page_trigger: Option<crate::trigger::Trigger>,
    /// A selection the user asked to move to another document, with where.
    /// Drained by the container into a `PaneRequest`; the shell performs the
    /// destination write and calls `complete_selection_move` back.
    pub wants_move_selection: Option<(MoveSelectionDestination, MoveSelectionPayload)>,
    /// What replaces the selection once the destination write succeeds. Held
    /// from the moment the move-mode picker opens until completion or
    /// cancellation — its absence at completion time means the content is
    /// duplicated rather than deleted, which is the safe direction.
    pub pending_selection_move: Option<PendingSelectionMove>,
    /// Tags to create in the workspace tag table and then apply to these
    /// nodes — the tag chooser's "Create tag" row, and a paste carrying a tag
    /// this workspace has never seen. Drained by the shell: the table lives in
    /// the workspace **system document**, which a view cannot see (ADR-176's
    /// seam), so creating is the host's and applying comes back through the
    /// ordinary document mutation path.
    pub wants_create_tag: CreateTagRequests,
    /// A list to create in the workspace list table — name and accent token
    /// — and then put these nodes in, making each a task first: the list
    /// picker's *New list* row, after its colour was chosen. The tag
    /// request's twin, through the same seam and for the same reason.
    pub wants_join_new_list: Option<(Vec<NodeId>, String, Option<String>)>,
    /// A node the user wants to turn into an image (the `/image` slash command).
    /// Cleared by the shell, which opens the file picker and performs the
    /// conversion — both need disk, which is not a view's to touch.
    pub wants_image: Option<NodeId>,
    /// A row whose `/attach` command should become a routed file pill.
    pub wants_attachment: Option<NodeId>,
    /// **`/dictate` was chosen: begin a dictation here.**
    ///
    /// A `bool` and not a `NodeId`, unlike the two above it, because the
    /// controller's `Begin` is routed to *the focused editable surface* by the
    /// same two answers a keystroke uses — there is nothing for a view to name.
    /// Cleared by the shell, which owns the one `Dictation` this process has:
    /// a microphone is no more a view's to open than a file dialog is, and one
    /// window asking is one window's session however many are open.
    pub wants_dictation: bool,
    /// **Hand this work to an agent: open the one delegation modal here.**
    ///
    /// Two of the four gestures behind that modal (ROADMAP D7) start inside a
    /// view — the editor's `/delegate` row and a document selection's
    /// right-click row — and neither of them may open it. Starting an agent
    /// needs the gateway, a device and a store, none of which is a view's, so
    /// the view says *what* it would delegate and the shell opens the surface.
    /// Cleared by the shell.
    pub wants_delegate: Option<DelegateRequest>,
    /// `/remind` requests for the host to drain. Store work never runs from
    /// editor input, layout, or drawing, and one request cannot overwrite
    /// another before maintenance runs.
    pub wants_reminder: std::collections::VecDeque<crate::reminders::Request>,
    /// Exact command text ownership retained only until each request is
    /// submitted or cancelled.
    pub pending_reminder_text:
        HashMap<crate::reminders::RequestToken, crate::reminders::PendingText>,
    /// Monotonic stamp that makes late async answers harmless.
    /// Batched host-supplied reminders, one entry per visible inline pill.
    /// Labels are localized during host maintenance; drawing and input never
    /// format or fetch reminder data.
    pub reminder_chips: HashMap<NodeId, Vec<crate::reminders::InlineChip>>,
    /// Exact node and Store reminder id whose inline pill was pressed.
    pub wants_reminder_management: Option<(NodeId, String)>,
    /// Exact reminder action chosen from an inline pill's context menu.
    pub wants_reminder_context: Option<crate::reminders::ContextRequest>,
    /// An attachment pill the user activated. The shell opens, reveals or
    /// saves the file; the editor never touches disk itself.
    pub wants_file_action: Option<(koru_core::DocumentId, crate::FileAction)>,
    /// **Show this task on the Tasks page.** The node, which is all a document
    /// knows: the container adds the document it is showing, works out which
    /// queue the task is in, and selects the row there.
    ///
    /// The other direction of the journey. Following a task to its origin has
    /// been one gesture since V-2, and coming back the other way was not
    /// offered at all — so a task you were reading in its document could only
    /// be found again by remembering which queue it fell in.
    pub wants_task_view: Option<NodeId>,
    /// **The row whose task the due chip's `×` was pressed on.**
    ///
    /// A request rather than an edit, because what the `×` asks for is a
    /// *question* — removing a task takes away the marker, the lifecycle and
    /// the due date, and none of that is visible from the chip you pressed, so
    /// the host puts a confirmation up first. This crate owns no dialogs, and
    /// `wants_task_view` beside it is the same shape for the same reason.
    ///
    /// It names its node rather than relying on the caret: pressing an `×`
    /// does not move the caret, and a confirmation about whichever row the
    /// keyboard happened to be on would be the wrong row's task.
    pub wants_remove_from_tasks: Option<NodeId>,
    /// A selected image the user copied (Ctrl+C). Cleared by the shell, which
    /// owns the decoded bytes and the system clipboard.
    /// Blob hash of an image to put on the system clipboard. Hash, not node
    /// id: cut deletes the node before the shell runs, and the cache is keyed
    /// by hash.
    pub wants_copy_image: Option<String>,
    /// A non-text clipboard the user pasted (Ctrl+V) — a screenshot, a browser
    /// "copy image", or image file(s) copied in Explorer. Holds the insertion
    /// anchor and selection roots to replace. Cleared by the shell, which is
    /// the only side with disk and an image-capable clipboard: it decodes the
    /// bytes, stores them in the blob tree, and calls `paste_images` on the view.
    pub wants_paste_image: Option<(NodeId, Vec<NodeId>)>,
    /// Text pasted from **outside** that may carry images (a Word document with
    /// inline pictures). Holds the insertion anchor, selection roots, **and the
    /// text itself**: the editor defers the whole paste to the shell (so it
    /// can interleave any RTF pictures with the text), and passes the text along
    /// rather than making the shell re-read a clipboard it just touched with
    /// `clipboard-win` — that re-read races and can come back empty. Cleared by
    /// the shell, which reads the RTF and either calls `paste_rich` (text +
    /// images interleaved) or `paste_text` (the carried text). Only a document
    /// view ever sets this (it gates on `images_enabled`).
    pub wants_paste_rich: Option<(NodeId, Vec<NodeId>, String)>,
    /// CF_HTML + plain text the most recent copy produced, for the shell to
    /// write in **one** clipboard session (HTML Format + CF_UNICODETEXT). The
    /// editor builds the bytes; the shell does the OS write after embedding
    /// image blobs. Plain is carried here so the shell does not rely on a prior
    /// platform clipboard write that would race or leave HTML unset.
    pub wants_copy_html: Option<(String, String)>,
    /// Paste batches (image / rich) so Ctrl+Z can remove inserted nodes and
    /// restore overwritten blocks when Loro leaves image tree edits in place.
    /// Push on paste; pop on undo into [`Self::last_image_redos`].
    pub last_image_pastes: Vec<PasteBatch>,
    /// Batches compensated from [`Self::last_image_pastes`]. Redo pops one and
    /// calls `ws.undo()` to reverse that edit (the same NodeIds return). Not
    /// a second history of ops — only a pointer into Loro's undo of our deletes.
    pub last_image_redos: Vec<PasteBatch>,
    /// After a full paste redo cycle, Loro's redo stack still holds the inverse
    /// deletes; extra Ctrl+Y would replay them. While set, Redo is a no-op when
    /// `last_image_redos` is empty. Cleared on any real edit / new paste.
    pub paste_redo_exhausted: bool,
    /// Image row to delete **after** the shell has written its bitmap to the
    /// system clipboard (cut). Deleting first left the cache write racing and
    /// external apps (OneNote) saw an empty clipboard.
    pub pending_cut_image: Option<NodeId>,
    /// Whether a hovered row shows the insert `+` in its affordance lane.
    ///
    /// The `+` opens the block menu, so it is worth having only where that menu
    /// has entries: a button that leads almost nowhere is worse than no button.
    /// On in a document, off in a bare list.
    pub insert_button: bool,
    /// Whether this document may be changed at all.
    ///
    /// **A capability of the view, like [`images_enabled`], not a condition like
    /// [`images_offline`].** It is set once when the view is opened and never
    /// toggled: a page that could become editable halfway through would need
    /// every affordance to appear and disappear, and nothing wants that.
    ///
    /// One thing sets it today — the *What's new* tab, which is a release's own
    /// notes rendered into a private in-memory workspace. That page is not the
    /// person's document and cannot be saved anywhere, so accepting edits and
    /// silently discarding them is the worst of the three options.
    ///
    /// **What it costs to honour is three places, not one**, and they are listed
    /// here because a fourth write path added later will not know about this
    /// flag: the key map refuses everything outside a navigation allowlist, the
    /// gutter draws no `+` and no drag grip, and the selection bubble does not
    /// open. There is no enforcement at the model's write boundary, which is
    /// what a real read-only mode would be — see `docs/contributing/techdebt/README.md`.
    ///
    /// [`images_enabled`]: Self::images_enabled
    /// [`images_offline`]: Self::images_offline
    pub read_only: bool,
    /// The workspace tag table, resolved for display: `TagId` → name + accent
    /// token. Pushed by the host whenever the system document changes and read
    /// by chip drawing and the tag chooser — never walked per frame.
    pub tags: crate::chips::TagLookup,
    /// The workspace list table, resolved for display: `ListId` → accent token.
    /// Pushed beside [`tags`](Self::tags) and for the same reason — the lists
    /// live in the workspace system document, which this document cannot see —
    /// and read at draw time to tint a task's due chip with its list's colour.
    pub lists: crate::chips::ListLookup,
    /// **Which workspace this view is showing a document of.** Pushed by the
    /// host beside [`tags`](Self::tags), because it answers the same kind of
    /// question and is unknowable from here for the same reason: a document's
    /// own `LoroDoc` carries no workspace id — the store hands out an empty one
    /// and the manifest is what places it.
    ///
    /// It exists for the clipboard. A **custom** attribute means whatever the
    /// workspace's system document says it means, so it travels inside one
    /// workspace and is dropped crossing into another; the copy records this
    /// id and the paste compares it (docs/features/trackable/DECISIONS.md Q35,
    /// [ADR-212](../../../docs/decisions/ADR-201-225.md#adr-212-blockcopy--copying-a-block-copies-the-blocks-attributes-and-a-tag-crosses-a-workspace-by-name)). `None` — a
    /// host that never pushed one, or a view under test — is *not* the same
    /// workspace as anything, so the conservative half of the rule is also the
    /// default.
    pub workspace: Option<koru_core::WorkspaceId>,
    /// Whether this view offers `/image` — images belong in prose.
    pub images_enabled: bool,
    /// Whether images cannot be stored **right now**, because the store is
    /// unreachable.
    ///
    /// **Deliberately not folded into [`images_enabled`]**, which answers a
    /// different question. That one is a *capability* of the view — prose takes
    /// images, a bare list does not — and it makes the command menu drop `/image`
    /// from the list entirely, which is right for a place images can never go.
    /// This one is a *condition*, it comes and goes while the window is open, and
    /// the command stays listed and goes dim with a reason beside it. Collapsing
    /// the two would make "there is no prose here" look exactly like "the store
    /// is down", and would silently hide the command instead of explaining it.
    ///
    /// An image's bytes go to the store through `put_blob`, an HTTP call, so
    /// there is nothing local to fall back on. Buffering one until the store
    /// returns is a later phase, and until then refusing plainly beats failing
    /// after the fact.
    ///
    /// [`images_enabled`]: Self::images_enabled
    pub images_offline: bool,
    /// Whether this view offers dividers (`/divider` and the `---` shorthand) —
    /// prose furniture, and offered where prose is.
    pub dividers_enabled: bool,
    /// Whether this view offers quoted prose (`/quote` and `> `).
    pub quotes_enabled: bool,
    /// Whether this view offers heading levels.
    ///
    /// Prose gets its structure from headings; a view that structures itself
    /// with the tree wants depth instead, which is the other half of ADR-23.
    ///
    /// One flag rather than a `Scope::Document` on the action, because the four
    /// ways in do not share a path: the chord and the toolbar button both reach
    /// `Editor::set_heading`, but the `/` menu and the `#` shorthand run through
    /// `type_text`, which never consults a scope.
    pub headings_enabled: bool,
    /// Whether this view offers list blocks (`[] ` shorthand, `/tasks`) — a way
    /// to get nesting inside a body that is otherwise flat.
    pub task_lists_enabled: bool,
    /// Whether this view offers `/code` and the markdown fence — a code block
    /// belongs in prose.
    pub code_enabled: bool,
    /// Table whose canonical Markdown is currently shown instead of its grid.
    pub table_source: Option<NodeId>,
    /// Horizontal offsets for extended tables. This is projection state, like
    /// the document scroll position: it never enters the CRDT and cannot drift
    /// from the table's canonical Markdown.
    pub table_scroll: HashMap<NodeId, f32>,
    /// Grid cell with keyboard focus: `(table, row, column)`, where row zero is
    /// the header. The selected source range remains the editing target; this
    /// tuple only lets the projection paint and traverse that target.
    pub table_cell: Option<(NodeId, usize, usize)>,
    /// Shared tooltip interaction clock used to reveal a hovered table's
    /// floating controls. The controls consume its opacity but draw no tooltip
    /// surface; this keeps the 400/100/75 ms contract in one implementation.
    pub table_controls: koru_ui::Tooltip,
    /// Table — or diagram — whose controls are waiting, visible, or fading
    /// out. A diagram's one control (*Edit text*) rides the same clock, so a
    /// table and a drawing side by side cannot both be showing furniture.
    pub table_controls_node: Option<NodeId>,
    /// What this view knows about each `mermaid` code block: whether its text
    /// draws, the drawing, and the last drawing that worked. Keyed by node and
    /// kept **here, not on the row**, because `Paragraph::adopt` clears a row's
    /// caches on every keystroke and the last good drawing has to outlive the
    /// keystroke that broke the text. Entries retire when their node leaves
    /// the body (`crate::diagram::sync`). Never synced: a drawing is derived.
    pub diagrams: HashMap<NodeId, crate::diagram::DiagramEntry>,
    /// The diagram whose text is open for editing in this view, if any. A
    /// reading position, like scroll — per view, never synced, and closed the
    /// moment the focus leaves the row.
    pub diagram_text_open: Option<NodeId>,
    /// Layouts the editor is asking the host for, in the order it asked.
    /// Drained by `Editing::take_diagram_requests`; the host answers each
    /// through `Editing::diagram_result`. The editor never lays a diagram out
    /// itself and never waits for one.
    pub diagram_requests: Vec<crate::diagram::DiagramRequest>,
    /// *Save as…* on a drawn diagram: the shell owns the save dialog, the
    /// raster and the file, so the view only asks. Drained by the document
    /// app the way `wants_file_action` is.
    pub wants_save_diagram_as: Option<SaveDiagramAs>,
    /// Copy icon on a hovered code block — one clock, not per row.
    pub copy: koru_ui::CopyAffordance,
    /// The unfinished-items triangle's tooltip. One clock for the whole view:
    /// the glyph is a statement rather than a control, and the only thing it
    /// owes a pointer resting on it is the sentence that explains it.
    pub task_warning: koru_ui::Tooltip,
    /// Whether this view offers `/table`. Documents do; the outline does not.
    pub tables_enabled: bool,
    /// What typing may turn a row into. [`Policy::Document`] everywhere but
    /// the AI chat's composer; set it through [`apply_policy`](Self::apply_policy)
    /// so the capability flags above agree with it.
    ///
    /// [`Policy::Document`]: crate::policy::Policy::Document
    pub policy: crate::policy::Policy,
    /// Set when a host draws this editor **inside its own chrome** — the AI
    /// chat's composer, a few lines tall at the bottom of a conversation.
    /// `None` for every view that owns its whole viewport.
    pub embedded: Option<Embedded>,
}

/// The editor drawn inside somebody else's frame (AGENTUX I6).
///
/// A document owns its viewport: it paints the background, clips rows to it,
/// draws a scrollbar on its right edge and keeps its pickers inside it. A chat
/// composer owns none of that. Its box, border and background are the chat's;
/// the editor's own margins lie *outside* the box, so rows must be clipped to
/// the box rather than to the viewport; and the `@` picker has to float above
/// a box that is one line tall, over the conversation.
///
/// Both rectangles are in **view coordinates** — the space `mouse_down` and
/// `selection_rects` use — so the host converts once, where it places the
/// editor, and the editor never learns where on the window it is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Embedded {
    /// Where rows, the caret and the placeholder may paint.
    pub clip: Rect,
    /// Where a floating panel may sit. May extend beyond the viewport — above
    /// it, for a composer at the bottom of a window.
    pub panels: Rect,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            engine: TextEngine::new(),
            overlay: Overlay::None,
            body_tone_press: None,
            body_tone_strip: None,
            viewport: (900.0, 560.0),
            view_mode: ViewMode::default(),
            content_x: 0.0,
            canvas_bg: None,
            scroll: 0.0,
            scroll_anim: None,
            content_h: 0.0,
            reflows: 0,
            hover: None,
            hover_since: Instant::now(),
            pointer: (0.0, 0.0),
            active: true,
            chrome_h: 0.0,
            status_h: koru_draw::theme::STATUS_H,
            page_gap_top: koru_draw::theme::PAGE_GAP,
            notice: None,
            chip_anchor: None,
            link_chip: None,
            wants_toast: None,
            wants_stay_focused: false,
            wants_sharing_toggle: false,
            wants_open: None,
            wants_entity_open: None,
            wants_run_open: None,
            wants_run_menu: None,
            wants_schedule_open: None,
            wants_schedule_menu: None,
            wants_new_page: None,
            pending_new_page_trigger: None,
            wants_move_selection: None,
            pending_selection_move: None,
            wants_create_tag: CreateTagRequests::default(),
            wants_join_new_list: None,
            wants_image: None,
            wants_attachment: None,
            wants_dictation: false,
            wants_delegate: None,
            wants_reminder: std::collections::VecDeque::new(),
            pending_reminder_text: HashMap::new(),
            reminder_chips: HashMap::new(),
            wants_reminder_management: None,
            wants_reminder_context: None,
            wants_file_action: None,
            wants_task_view: None,
            wants_remove_from_tasks: None,
            wants_copy_image: None,
            wants_paste_image: None,
            wants_paste_rich: None,
            wants_copy_html: None,
            last_image_pastes: Vec::new(),
            last_image_redos: Vec::new(),
            paste_redo_exhausted: false,
            pending_cut_image: None,
            insert_button: false,
            read_only: false,
            tags: crate::chips::TagLookup::default(),
            lists: crate::chips::ListLookup::default(),
            workspace: None,
            images_enabled: false,
            images_offline: false,
            dividers_enabled: false,
            quotes_enabled: false,
            headings_enabled: false,
            task_lists_enabled: false,
            code_enabled: false,
            table_source: None,
            table_scroll: HashMap::new(),
            table_cell: None,
            table_controls: koru_ui::Tooltip::default(),
            table_controls_node: None,
            diagrams: HashMap::new(),
            diagram_text_open: None,
            diagram_requests: Vec::new(),
            wants_save_diagram_as: None,
            task_warning: koru_ui::Tooltip::default(),
            copy: koru_ui::CopyAffordance::default(),
            tables_enabled: false,
            policy: crate::policy::Policy::Document,
            embedded: None,
            deleted_caret: None,
            desired_x: None,
            page_body_caret: None,
            dragging: false,
            drag_scroll_tick: None,
            drag_field: false,
            drag_row: None,
            scroll_drag: None,
            image_resize: None,
            focused_image: None,
            selected_pill: None,
            search_flash: None,
            search_highlights: Vec::new(),
            spelling: Vec::new(),
            word_completed: None,
            wants_workspace_word: None,
            mods: ModifiersState::empty(),
        }
    }
}

impl ViewState {
    /// Keep the scroll inside a body `content_h` tall.
    ///
    /// Takes the height rather than reading it off the rows, because the rows
    /// are the one thing this file does not know about.
    pub fn clamp_scroll(&mut self, content_h: f32) {
        let max = (content_h - self.content_viewport_h()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Offer only what `policy` allows. The composer policy turns every block
    /// capability off with it, so a flag left on cannot reopen a door the
    /// policy closed; a document view sets its capabilities itself, as before.
    pub fn apply_policy(&mut self, policy: crate::policy::Policy) {
        self.policy = policy;
        if policy == crate::policy::Policy::Composer {
            self.insert_button = false;
            self.images_enabled = false;
            self.dividers_enabled = false;
            self.quotes_enabled = false;
            self.headings_enabled = false;
            self.task_lists_enabled = false;
            self.code_enabled = false;
            self.tables_enabled = false;
        }
    }

    /// The exact pane-local clip used to paint document rows.
    /// Shared with accessibility so chrome and status exclusions cannot drift.
    pub fn content_clip(&self) -> koru_draw::geometry::Rect {
        if let Some(embedded) = self.embedded {
            return embedded.clip;
        }
        if self.view_mode == ViewMode::Page {
            koru_draw::geometry::Rect::new(
                0.0,
                self.chrome_h,
                self.viewport.0,
                self.content_viewport_h(),
            )
        } else {
            koru_draw::geometry::Rect::new(
                self.content_x,
                0.0,
                self.view_mode.effective_w(self.viewport.0),
                self.viewport.1,
            )
        }
    }
    /// Height available to rows, once this view's own chrome is taken out.
    /// Page mode now scrolls the page *through* this area (the page grows with
    /// the content), so it is the same content-area height for every mode.
    pub fn content_viewport_h(&self) -> f32 {
        (self.viewport.1 - self.chrome_h - self.status_h).max(0.0)
    }

    /// The thumb's rectangle and the band it runs in, or `None` when the content
    /// fits without scrolling.
    ///
    /// Takes the height rather than reading it off the rows for the same reason
    /// [`clamp_scroll`](Self::clamp_scroll) does: the rows are the one thing this
    /// file does not know about. The thumb is the viewport's share of the
    /// content, never shorter than [`SCROLLBAR_MIN_THUMB`](koru_draw::theme::SCROLLBAR_MIN_THUMB)
    /// so a very long document still leaves something to grab.
    pub fn scrollbar_geom(&self, content_h: f32) -> Option<ScrollbarGeom> {
        let vh = self.content_viewport_h();
        let max_scroll = (content_h - vh).max(0.0);
        if max_scroll <= 0.0 {
            return None;
        }
        let track_top = self.chrome_h;
        let track_h = vh;
        let thumb_h = (vh * vh / content_h)
            .max(koru_draw::theme::SCROLLBAR_MIN_THUMB)
            .min(vh);
        let travel = (track_h - thumb_h).max(0.0);
        let thumb_top = track_top + travel * (self.scroll / max_scroll);

        let gutter_w = koru_draw::theme::SCROLLBAR_GUTTER_W;
        let edge = koru_draw::theme::SCROLLBAR_EDGE_CLEAR;
        // Lane ends before the shell resize band, so grab and resize do not share
        // the same pixels. `min` keeps a tiny viewport from going negative.
        let gutter_x = (self.viewport.0 - edge - gutter_w).max(0.0);
        let gutter_w = gutter_w.min(self.viewport.0 - gutter_x).max(0.0);

        // Fixed paint width — hover only darkens (`scrollbar_thumb_color`).
        let thumb_w = koru_draw::theme::SCROLLBAR_W.min(gutter_w);
        // Sit on the outer edge of the gutter (toward the window edge), so the
        // extra hit lane opens inward over the page rather than into the resize
        // band.
        let thumb_x = gutter_x + gutter_w - thumb_w;

        Some(ScrollbarGeom {
            thumb_x,
            thumb_w,
            thumb_top,
            thumb_h,
            gutter_x,
            gutter_w,
            track_top,
            track_bottom: track_top + track_h,
            max_scroll,
        })
    }

    /// Whether `(x, y)` sits in the scrollbar's hit lane. Used to know when
    /// entering the gutter needs a repaint for the darker thumb.
    pub fn over_scrollbar_gutter(&self, content_h: f32, x: f32, y: f32) -> bool {
        let Some(g) = self.scrollbar_geom(content_h) else {
            return false;
        };
        // Geom expands from `self.pointer`; hit-test the lane with the given
        // point so a move that only enters the gutter still counts.
        x >= g.gutter_x && x <= g.gutter_x + g.gutter_w && y >= g.track_top && y <= g.track_bottom
    }

    /// The pointer shape the window should be wearing right now.
    ///
    /// Chrome is drawn *over* the rows rather than beside them, so a row
    /// scrolled underneath the toolbar or the status bar still hit-tests as
    /// hovered. Asking where the pointer is first is what stops an I-beam
    /// appearing over either strip.
    pub fn cursor_icon(&self) -> CursorIcon {
        if !self.over_content(self.pointer.1) {
            return cursor::IDLE;
        }
        // Reordering an image row keeps the closed grab hand until release,
        // even if the pointer leaves the image or its handle mid-drag.
        if self.drag_row.is_some() {
            return CursorIcon::Grabbing;
        }
        // A resize in progress keeps its handle's cursor wherever the pointer
        // roams, the way a text drag keeps the I-beam.
        if let Some(drag) = self.image_resize {
            return drag.handle.cursor();
        }
        // Overlay pickers still use a grab hand on their own thumbs.
        if self.overlay.is_dragging_scrollbar() {
            return CursorIcon::Grabbing;
        }
        // Document scrollbar: the hit **lane** is wider than the drawn thumb
        // (`gutter` vs paint width), and both wear up/down arrows so the user
        // can see the control while aiming slightly left of the pill. A grab
        // hand next to the window resize edge fought that cursor; NsResize
        // does not — it is vertical, the edge is horizontal.
        if let Some(g) = self.scrollbar_geom(self.content_h) {
            let (x, y) = self.pointer;
            if g.in_gutter(x, y) || self.scroll_drag.is_some() {
                return CursorIcon::NsResize;
            }
        }
        // An image's resize handles, when it is the selected row. The cached
        // rect is in document coordinates, so offset it by the scroll. The
        // pointer is mapped into the centred content column first — the rect
        // lives there, not at the viewport's left edge.
        if let Some((_, r)) = self.focused_image {
            if let Some(handle) = image_handle_at(
                Rect::new(r.x, r.y - self.scroll, r.w, r.h),
                self.pointer.0 - self.content_x,
                self.pointer.1,
            ) {
                return handle.cursor();
            }
        }
        self.hover.map_or(cursor::IDLE, Hover::cursor_icon)
    }

    /// Whether a window-space `y` is in the band the rows have to themselves.
    pub fn over_content(&self, y: f32) -> bool {
        y >= self.chrome_h && y <= self.viewport.1 - self.status_h
    }
}

// ------------------------------------------------------------- image resize

/// Half the size of a corner box, and how far outside an edge still counts as
/// "on it" — so a handle is easy to grab without having to pixel-aim.
const HANDLE_HALF: f32 = 6.0;
const EDGE_TOL: f32 = 7.0;

/// One of an image's eight resize handles. The image is anchored at its
/// top-left, so the bottom and right handles track the pointer exactly while
/// the top and left resize in the mirrored direction (the picture still grows
/// from its top-left corner).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Handle {
    /// The pointer shape over this handle.
    pub fn cursor(self) -> CursorIcon {
        match self {
            Self::Top | Self::Bottom => CursorIcon::NsResize,
            Self::Left | Self::Right => CursorIcon::EwResize,
            Self::TopLeft | Self::BottomRight => CursorIcon::NwseResize,
            Self::TopRight | Self::BottomLeft => CursorIcon::NeswResize,
        }
    }

    fn widths(self) -> bool {
        matches!(
            self,
            Self::Left
                | Self::Right
                | Self::TopLeft
                | Self::TopRight
                | Self::BottomLeft
                | Self::BottomRight
        )
    }

    fn heights(self) -> bool {
        matches!(
            self,
            Self::Top
                | Self::Bottom
                | Self::TopLeft
                | Self::TopRight
                | Self::BottomLeft
                | Self::BottomRight
        )
    }

    /// Width grows in the mirrored direction when grabbed on the left.
    fn left(self) -> bool {
        matches!(self, Self::Left | Self::TopLeft | Self::BottomLeft)
    }

    /// Height grows in the mirrored direction when grabbed on the top.
    fn top(self) -> bool {
        matches!(self, Self::Top | Self::TopLeft | Self::TopRight)
    }
}

/// Which handle a point is over, if any. The rect is in the same coordinates as
/// the point — window space at the call sites, with the image's scroll offset
/// already applied.
pub fn image_handle_at(r: Rect, px: f32, py: f32) -> Option<Handle> {
    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
    // Corners first — they outrank the edges they sit on.
    let corner = |cx: f32, cy: f32, handle: Handle| -> Option<Handle> {
        if (px - cx).abs() <= HANDLE_HALF && (py - cy).abs() <= HANDLE_HALF {
            Some(handle)
        } else {
            None
        }
    };
    corner(x, y, Handle::TopLeft)
        .or_else(|| corner(x + w, y, Handle::TopRight))
        .or_else(|| corner(x, y + h, Handle::BottomLeft))
        .or_else(|| corner(x + w, y + h, Handle::BottomRight))
        .or_else(|| {
            // Then the edges, within a tolerance either side of each border.
            let near_x = px >= x - EDGE_TOL && px <= x + w + EDGE_TOL;
            let near_y = py >= y - EDGE_TOL && py <= y + h + EDGE_TOL;
            if near_x && (py - y).abs() <= EDGE_TOL {
                Some(Handle::Top)
            } else if near_x && (py - (y + h)).abs() <= EDGE_TOL {
                Some(Handle::Bottom)
            } else if near_y && (px - x).abs() <= EDGE_TOL {
                Some(Handle::Left)
            } else if near_y && (px - (x + w)).abs() <= EDGE_TOL {
                Some(Handle::Right)
            } else {
                None
            }
        })
}

/// An in-progress image resize. Pure over the pointer position, so the size it
/// computes is testable without a document.
#[derive(Copy, Clone, Debug)]
pub struct ImageResize {
    pub node: NodeId,
    pub handle: Handle,
    /// Display size at the start of the drag.
    pub start: (f32, f32),
    /// Pointer position at the start of the drag.
    pub anchor: (f32, f32),
    /// Original pixel aspect (`w / h`); used when Shift locks it. `0.0` = unknown.
    pub aspect: f32,
    /// The content column width — a display width cannot exceed it.
    pub column_w: f32,
}

impl ImageResize {
    /// The smallest dimension an image may shrink to.
    const MIN: f32 = 24.0;
    /// The tallest an image may grow — one picture should not swallow the window.
    const MAX_H: f32 = 1600.0;

    /// The display size for `pointer` now, with `shift` locking the aspect.
    pub fn size_at(&self, px: f32, py: f32, shift: bool) -> (f32, f32) {
        let (dx, dy) = (px - self.anchor.0, py - self.anchor.1);
        let (sw, sh) = self.start;
        let h = self.handle;

        let mut nw = sw;
        let mut nh = sh;
        if h.widths() {
            nw = sw + if h.left() { -dx } else { dx };
        }
        if h.heights() {
            nh = sh + if h.top() { -dy } else { dy };
        }

        if shift && self.aspect > 0.0 {
            // Lock to the original aspect. A corner takes the axis that moved
            // more; an edge derives the other axis from the one it drives.
            if h.widths() && h.heights() {
                let s = ((nw / sw).abs()).max((nh / sh).abs());
                nw = sw * s;
                nh = sh * s;
            } else if h.widths() {
                nh = nw / self.aspect;
            } else {
                nw = nh * self.aspect;
            }
        }

        (
            nw.clamp(Self::MIN, self.column_w.max(Self::MIN)),
            nh.clamp(Self::MIN, Self::MAX_H),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panel row is not a document row. Highlighting one because the pointer
    /// is over the other is the bug this exists to prevent.
    #[test]
    fn only_document_rows_report_a_row() {
        assert_eq!(Hover::Grip(3).row(), Some(3));
        assert_eq!(Hover::Checkbox(1).row(), Some(1));
        assert_eq!(Hover::Text(4).row(), Some(4));
        assert_eq!(Hover::Image(5).row(), Some(5));
        assert_eq!(
            Hover::Pill {
                row: 2,
                attr: AttrId::new()
            }
            .row(),
            Some(2)
        );
        assert_eq!(Hover::PanelRow(0).row(), None);
        assert_eq!(Hover::PanelField.row(), None);
    }

    #[test]
    fn scroll_stops_at_the_end_of_the_content() {
        let mut v = ViewState {
            viewport: (900.0, 500.0),
            scroll: 9999.0,
            ..Default::default()
        };
        v.clamp_scroll(2000.0);
        assert!(v.scroll > 0.0 && v.scroll < 2000.0);

        // Content shorter than the window cannot scroll at all.
        v.clamp_scroll(10.0);
        assert_eq!(v.scroll, 0.0);
    }

    /// Interactive content advertises whether it edits or drags — and nothing
    /// else. Koru does not use the hand cursor, so a press target wears the
    /// plain arrow and says what it is by how it is drawn.
    #[test]
    fn interactive_content_uses_the_matching_pointer() {
        // The three that carry information the appearance cannot.
        assert_eq!(Hover::Text(0).cursor_icon(), CursorIcon::Text);
        assert_eq!(Hover::Emoji(0).cursor_icon(), CursorIcon::Text);
        assert_eq!(Hover::PanelField.cursor_icon(), CursorIcon::Text);
        assert_eq!(Hover::Image(0).cursor_icon(), CursorIcon::Grab);
        assert_eq!(Hover::Grip(0).cursor_icon(), CursorIcon::Grab);
        assert_eq!(Hover::Thumb.cursor_icon(), CursorIcon::NsResize);

        // Everything pressable, and none of it wears the hand. Listed one by
        // one rather than looped so a new `Hover` arm has to be added here
        // too — the enum is the register of what this view can be over.
        for hover in [
            Hover::Pill {
                row: 0,
                attr: AttrId::new(),
            },
            Hover::Chip { row: 0, chip: 0 },
            Hover::Checkbox(0),
            Hover::TableLayout(0),
            Hover::TableMode(0),
            Hover::PanelRow(0),
            Hover::PanelControl,
            Hover::ScrollTrack,
            Hover::Plus(0),
            Hover::TitleMenu,
            Hover::Copy(0),
            Hover::Lang(0),
        ] {
            assert_eq!(hover.cursor_icon(), cursor::PRESS, "{hover:?}");
            assert_ne!(hover.cursor_icon(), CursorIcon::Pointer, "{hover:?}");
        }
    }

    #[test]
    fn row_drag_keeps_the_grabbing_pointer() {
        let v = ViewState {
            pointer: (100.0, 200.0),
            hover: Some(Hover::Image(0)),
            drag_row: Some(DragRow { from: 0, to: 1 }),
            ..Default::default()
        };
        assert_eq!(v.cursor_icon(), CursorIcon::Grabbing);
    }

    /// Chrome is drawn *over* the rows rather than beside them, so a row
    /// scrolled underneath it is still hovered — and an I-beam over the toolbar
    /// is the bug that produces.
    #[test]
    fn chrome_keeps_the_arrow_even_over_a_hovered_row() {
        let mut v = ViewState {
            chrome_h: koru_draw::theme::CAPTION_H,
            hover: Some(Hover::Text(0)),
            ..Default::default()
        };

        v.pointer = (100.0, v.chrome_h / 2.0);
        assert_eq!(v.cursor_icon(), CursorIcon::Default, "over the toolbar");

        v.pointer = (100.0, v.viewport.1 - koru_draw::theme::STATUS_H / 2.0);
        assert_eq!(v.cursor_icon(), CursorIcon::Default, "over the status bar");

        v.pointer = (100.0, v.viewport.1 / 2.0);
        assert_eq!(v.cursor_icon(), CursorIcon::Text, "over the rows");
    }

    /// Hovering nothing is the arrow, whatever the pointer is over.
    #[test]
    fn no_hover_is_the_plain_arrow() {
        let v = ViewState {
            pointer: (100.0, 200.0),
            ..Default::default()
        };
        assert_eq!(v.cursor_icon(), CursorIcon::Default);
    }

    // ------------------------------------------------------------- scrollbar

    /// A helper view: 900x600, no chrome, so the content band is the full height
    /// minus the status bar.
    fn scroll_view() -> ViewState {
        ViewState {
            viewport: (900.0, 600.0),
            ..Default::default()
        }
    }

    /// Content that fits has no scrollbar — this is the "too long" gate, and the
    /// one thing every other assertion depends on.
    #[test]
    fn scrollbar_hidden_when_content_fits() {
        let v = scroll_view();
        // Band height is 600 minus the 26px status bar.
        let vh = 600.0 - koru_draw::theme::STATUS_H;
        assert!(v.scrollbar_geom(vh).is_none(), "exactly fits");
        assert!(v.scrollbar_geom(vh - 100.0).is_none(), "shorter than band");
        assert!(v.scrollbar_geom(vh + 1.0).is_some(), "one pixel too long");
    }

    /// The thumb's ends track the scroll: pinned to the top at the start, to the
    /// bottom at the end, and the thumb never leaves the band.
    #[test]
    fn scrollbar_thumb_maps_scroll_to_position() {
        let mut v = scroll_view();
        let vh = 600.0 - koru_draw::theme::STATUS_H;
        let content_h = 2000.0;
        let max = content_h - vh;

        let g = v.scrollbar_geom(content_h).unwrap();
        assert_eq!(g.track_top, 0.0);
        assert_eq!(g.track_bottom, vh);
        assert_eq!(g.max_scroll, max);
        assert!(
            (g.thumb_top - 0.0).abs() < 0.01,
            "at the top: thumb_top == track_top"
        );

        v.scroll = max;
        let g = v.scrollbar_geom(content_h).unwrap();
        assert!(
            (g.thumb_top + g.thumb_h - vh).abs() < 0.01,
            "at the end: thumb bottom == track bottom"
        );

        v.scroll = max * 0.5;
        let g = v.scrollbar_geom(content_h).unwrap();
        let mid = (vh - g.thumb_h) * 0.5;
        assert!(
            (g.thumb_top - mid).abs() < 0.01,
            "halfway scrolled: thumb centred in the band"
        );
    }

    /// A very long document still leaves something to grab: the thumb never
    /// shrinks below the floor, and never grows past the band.
    #[test]
    fn scrollbar_thumb_has_a_floor_and_a_cap() {
        let v = scroll_view();
        let vh = 600.0 - koru_draw::theme::STATUS_H;

        let g = v.scrollbar_geom(100_000.0).unwrap();
        assert_eq!(g.thumb_h, koru_draw::theme::SCROLLBAR_MIN_THUMB);

        // Barely too long: the thumb is almost the whole band, but not over it.
        let g = v.scrollbar_geom(vh + 1.0).unwrap();
        assert!(g.thumb_h <= vh + 0.001);
    }

    /// The band starts below chrome, so a document with a toolbar does not let
    /// the thumb or the gutter reach into it.
    #[test]
    fn scrollbar_band_starts_below_chrome() {
        let v = ViewState {
            viewport: (900.0, 600.0),
            chrome_h: koru_draw::theme::CAPTION_H,
            ..Default::default()
        };
        let vh = v.content_viewport_h();
        let g = v.scrollbar_geom(2000.0).unwrap();
        assert_eq!(g.track_top, koru_draw::theme::CAPTION_H);
        assert_eq!(g.track_bottom, koru_draw::theme::CAPTION_H + vh);
        // The toolbar strip is not in the gutter, even at the right edge.
        assert!(!g.in_gutter(899.0, koru_draw::theme::CAPTION_H * 0.5));
    }

    /// The thumb grabs, the band pages, and the rest of the window does neither.
    #[test]
    fn scrollbar_hit_testing() {
        // Scrolled to the middle, so there is band both above and below the
        // thumb to tell apart from the thumb itself.
        let mut v = scroll_view();
        v.scroll = 713.0;
        let g = v.scrollbar_geom(2000.0).unwrap();
        let cx = g.thumb_x + g.thumb_w * 0.5;
        let cy = g.thumb_top + g.thumb_h * 0.5;

        assert!(g.on_thumb(cx, cy), "centre of the thumb");
        assert!(
            !g.on_thumb(cx, g.track_top),
            "top of band is above the thumb"
        );
        assert!(g.in_gutter(cx, g.track_top), "top of band is the gutter");
        assert!(
            g.in_gutter(cx, g.track_bottom - 1.0),
            "bottom of band is the gutter"
        );
        assert!(!g.in_gutter(cx, 599.0), "status bar is not the gutter");
        // Hit lane is wider than the drawn thumb — left edge of the gutter, not
        // of the painted bar.
        assert!(
            !g.in_gutter(g.gutter_x - 1.0, cy),
            "a pixel left of the gutter is content, not the scrollbar"
        );
        assert!(
            g.gutter_w > g.thumb_w,
            "gutter is the grab target; thumb is the paint"
        );
        assert!(
            g.gutter_x + g.gutter_w <= 900.0 - koru_draw::theme::SCROLLBAR_EDGE_CLEAR + 0.01,
            "gutter clears the window resize band"
        );
    }

    #[test]
    fn the_scrollbar_thumb_width_does_not_change_on_hover() {
        let mut v = scroll_view();
        v.content_h = 2000.0;
        v.pointer = (100.0, 300.0);
        let rest = v.scrollbar_geom(2000.0).unwrap();
        assert!((rest.thumb_w - koru_draw::theme::SCROLLBAR_W).abs() < 0.01);
        v.pointer = (
            rest.gutter_x + rest.gutter_w * 0.5,
            rest.thumb_top + rest.thumb_h * 0.5,
        );
        let hover = v.scrollbar_geom(2000.0).unwrap();
        assert!(
            (hover.thumb_w - rest.thumb_w).abs() < 0.01,
            "hover darkens only — width stays put"
        );
    }

    #[test]
    fn the_scrollbar_thumb_darkens_when_active() {
        use koru_draw::theme::scrollbar_thumb_color;
        let rest = scrollbar_thumb_color(false).components;
        let active = scrollbar_thumb_color(true).components;
        assert!(
            active[3] > rest[3] + 0.05,
            "active raises opacity so the change is visible on a translucent thumb"
        );
        assert!(
            active[0] + active[1] + active[2] < rest[0] + rest[1] + rest[2],
            "active also darkens RGB"
        );
    }

    /// Over the gutter the pointer is never the I-beam — the scrollbar is
    /// chrome. The whole hit lane (wider than the drawn pill) wears up/down
    /// arrows so aiming slightly left of the thumb still reads as scroll.
    #[test]
    fn the_scrollbar_lane_wears_up_down_arrows_not_the_ibeam() {
        let mut v = scroll_view();
        v.content_h = 2000.0;
        v.hover = Some(Hover::Text(0));
        let g = v.scrollbar_geom(v.content_h).unwrap();
        let thumb_x = g.thumb_x + g.thumb_w * 0.5;
        // Inward of the painted pill but still inside the wider hit lane.
        let lane_x = g.gutter_x + 1.0;
        assert!(lane_x < g.thumb_x, "lane opens left of the drawn thumb");

        v.pointer = (thumb_x, g.thumb_top + g.thumb_h * 0.5);
        assert_eq!(
            v.cursor_icon(),
            CursorIcon::NsResize,
            "thumb: up/down, even above a hovered row"
        );

        v.pointer = (lane_x, g.thumb_top + g.thumb_h * 0.5);
        assert_eq!(
            v.cursor_icon(),
            CursorIcon::NsResize,
            "lane left of the pill still scrolls"
        );

        v.pointer = (thumb_x, g.track_bottom - 1.0);
        assert_eq!(
            v.cursor_icon(),
            CursorIcon::NsResize,
            "gutter band above/below the thumb too"
        );

        v.scroll_drag = Some(0.0);
        assert_eq!(
            v.cursor_icon(),
            CursorIcon::NsResize,
            "drag keeps the up/down arrows"
        );
        v.scroll_drag = None;

        // A pixel out of the gutter goes back to whatever the hover earns.
        v.pointer = (g.gutter_x - 2.0, 300.0);
        assert_eq!(v.cursor_icon(), CursorIcon::Text, "off the gutter is text");

        // And when nothing scrolls, the gutter is not claimed at all.
        v.content_h = 100.0;
        v.pointer = (thumb_x, 300.0);
        assert_eq!(
            v.cursor_icon(),
            CursorIcon::Text,
            "no scrollbar, no carve-out"
        );
    }

    // ------------------------------------------------------------- image resize

    fn img_rect() -> Rect {
        Rect::new(100.0, 200.0, 400.0, 300.0)
    }

    /// The eight handles: four corners first, then the four edges between them.
    #[test]
    fn every_handle_is_hit_testable() {
        let r = img_rect();
        // Corners.
        assert_eq!(image_handle_at(r, 100.0, 200.0), Some(Handle::TopLeft));
        assert_eq!(image_handle_at(r, 500.0, 200.0), Some(Handle::TopRight));
        assert_eq!(image_handle_at(r, 100.0, 500.0), Some(Handle::BottomLeft));
        assert_eq!(image_handle_at(r, 500.0, 500.0), Some(Handle::BottomRight));
        // Edges — within the tolerance band, a little outside the image.
        assert_eq!(image_handle_at(r, 300.0, 196.0), Some(Handle::Top));
        assert_eq!(image_handle_at(r, 300.0, 504.0), Some(Handle::Bottom));
        assert_eq!(image_handle_at(r, 96.0, 350.0), Some(Handle::Left));
        assert_eq!(image_handle_at(r, 504.0, 350.0), Some(Handle::Right));
        // The middle of the picture is no handle at all.
        assert_eq!(image_handle_at(r, 300.0, 350.0), None);
    }

    /// Each handle maps to the resize cursor its axis earns.
    #[test]
    fn handles_map_to_the_right_cursor() {
        assert_eq!(Handle::Top.cursor(), CursorIcon::NsResize);
        assert_eq!(Handle::Bottom.cursor(), CursorIcon::NsResize);
        assert_eq!(Handle::Left.cursor(), CursorIcon::EwResize);
        assert_eq!(Handle::Right.cursor(), CursorIcon::EwResize);
        assert_eq!(Handle::TopLeft.cursor(), CursorIcon::NwseResize);
        assert_eq!(Handle::BottomRight.cursor(), CursorIcon::NwseResize);
        assert_eq!(Handle::TopRight.cursor(), CursorIcon::NeswResize);
        assert_eq!(Handle::BottomLeft.cursor(), CursorIcon::NeswResize);
    }

    /// The bottom edge grows the height with the pointer; the top edge grows it
    /// in the mirrored direction (the image is anchored at its top-left).
    #[test]
    fn an_edge_resize_changes_one_axis() {
        let drag = ImageResize {
            node: NodeId::new(),
            handle: Handle::Bottom,
            start: (400.0, 300.0),
            anchor: (0.0, 0.0),
            aspect: 1.0,
            column_w: 1000.0,
        };
        let (w, h) = drag.size_at(0.0, 50.0, false);
        assert!((w - 400.0).abs() < 0.5, "width untouched");
        assert!((h - 350.0).abs() < 0.5, "height grew by the pointer delta");

        // Top edge: dragging up (negative delta) still grows the height.
        let drag = ImageResize {
            handle: Handle::Top,
            ..drag
        };
        let (_, h) = drag.size_at(0.0, -50.0, false);
        assert!(
            (h - 350.0).abs() < 0.5,
            "mirrored: up on the top edge grows it"
        );
    }

    /// A corner resize changes both axes; Shift locks them to the aspect ratio.
    #[test]
    fn a_corner_with_shift_locks_the_aspect() {
        let drag = ImageResize {
            node: NodeId::new(),
            handle: Handle::BottomRight,
            start: (400.0, 300.0),
            anchor: (0.0, 0.0),
            aspect: 400.0 / 300.0,
            column_w: 2000.0,
        };
        // Without Shift both axes move by their own delta → distorted.
        let (w, h) = drag.size_at(100.0, 10.0, false);
        assert!(
            (w - 500.0).abs() < 0.5 && (h - 310.0).abs() < 0.5,
            "free corner"
        );

        // With Shift the aspect wins: a 400×300 with the same aspect cannot
        // become 500×310; both scale together.
        let (w, h) = drag.size_at(100.0, 10.0, true);
        assert!(
            (w / h - drag.aspect).abs() < 0.01,
            "aspect locked: {w}×{h} should be {drag_aspect}",
            drag_aspect = drag.aspect
        );
    }

    /// A resize never widens past the content column or shrinks past the floor.
    #[test]
    fn a_resize_clamps_to_the_column_and_a_floor() {
        let drag = ImageResize {
            node: NodeId::new(),
            handle: Handle::BottomRight,
            start: (400.0, 300.0),
            anchor: (0.0, 0.0),
            aspect: 1.0,
            column_w: 500.0,
        };
        let (w, _) = drag.size_at(10_000.0, 0.0, false);
        assert!(w <= 500.0, "width clamped to the column: {w}");
        let (_, h) = drag.size_at(0.0, -10_000.0, false);
        assert!(h >= ImageResize::MIN, "height floored: {h}");
    }
}
