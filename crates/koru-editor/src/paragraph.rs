// SPDX-License-Identifier: MPL-2.0

//! One line of the document, and everything needed to draw and edit it.
//!
//! A `Paragraph` knows nothing about its neighbours. Splitting on Enter and
//! merging on Backspace touch two nodes, so they belong to the view that owns
//! the list — this is the boundary that lets the outline and the document share
//! all of their text editing without sharing anything else.
//!
//! It is a *cache*, not the truth: `rebuild` throws these away and makes them
//! again from the document. Nothing here may hold state the document does not.

use std::collections::HashMap;

use parley::Layout;
use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, Cluster};
use unicode_segmentation::UnicodeSegmentation;

use koru_core::NodeId;
use koru_core::TableLayout;
use koru_core::node::{Align, BlobHash, Heading, Kind, ListStyle, Node};
use koru_core::schema::WorkspaceDoc;
use koru_core::text::{ANCHOR, ANCHOR_LEN, TextRun};
use koru_core::{Mark, MarkSet, Script, TextStyle};

use koru_draw::geometry::text_x;
use koru_draw::pill::PillInfo;
use koru_draw::theme::{self, Brush, FontChoice};

/// Horizontal room reserved for a quote's rule and the air after it. Kept on
/// row geometry so layout, hit-testing, selection and caret inherit one inset.
pub const QUOTE_INSET_X: f32 = 34.0;
pub const QUOTE_BAR_W: f32 = 4.0;

/// The image a row renders in place of text: enough to address its bytes and
/// size it. The decoded pixels are fetched from the shell's `ImageCache` at draw
/// time — this struct carries only what lives in the document (the hash, mime
/// and dimensions), never the bytes ([ADR-12](../../../docs/decisions/README.md)).
#[derive(Clone, PartialEq, Debug)]
pub struct ImageRef {
    pub hash: BlobHash,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The size to draw at, if the user has resized it; `None` is natural.
    pub display: Option<(u32, u32)>,
}

/// What a row is **as a task**, cached at rebuild so neither drawing nor a
/// menu re-derives it ([ADR-285]).
///
/// `Paragraph::is_task` is a different question and stays a different one: it
/// asks whether this row *draws a checkbox*, which a plain checklist item does
/// without ever being a task. This asks whether `koru_core::tasks::is_task`
/// holds — a marker, or a due — and is `None` on every row where it does not.
///
/// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TaskRow {
    /// The **canonical** lifecycle, not the `done` mirror. SPEC §5 lets the two
    /// disagree on a task with children, and this is the half that decides what
    /// Complete/Reopen offers and what the residue indicator means.
    pub completed: bool,
    /// A completed task still holding an unfinished structural child — SPEC §5's
    /// **concurrent residue**, and the whole reason the indicator exists.
    ///
    /// Read from the document by `Body`, not by `from_node`/`adopt`, because it
    /// is a fact about this row's *children* rather than about its node. It is
    /// therefore reset to `false` on every re-read and set again immediately
    /// after; a row reused untouched keeps the answer it already had, which is
    /// correct because a structural change names a node's parent as well as the
    /// node.
    pub unfinished_children: bool,
}

/// One visible line: a node plus everything the renderer caches about it.
pub struct Paragraph {
    pub id: NodeId,
    /// What this row *is*. A checkbox, a strikethrough and a grip belong to a
    /// task; a document's paragraph gets none of them.
    pub kind: Kind,
    /// Derived from the tree, never stored — which is why it cannot go illegal.
    pub depth: u16,
    pub done: bool,
    /// This row's task situation, or `None` when it is not a task. See
    /// [`TaskRow`]: `done` above is the checkbox, this is the lifecycle.
    pub task: Option<TaskRow>,
    /// The list this task belongs to, or `None`.
    ///
    /// **A row-level fact, beside [`task`](Self::task), not a chip.** `koru.list`
    /// is single-valued, and in a document the membership is carried by the due
    /// chip's colour rather than by a lozenge of its own — so what the drawing
    /// pass needs is the id, once per row, to resolve against the pushed table.
    pub list: Option<koru_core::ids::ListId>,
    pub align: Align,
    /// This block's heading level, or `None` for body text. Like `align`, it
    /// changes every glyph without changing a byte of the text.
    pub heading: Option<Heading>,
    /// The list style this row's enclosing `ListBlock` carries, or `None` for
    /// prose. A renderer cache populated by `Body::rebuild` from the ancestor
    /// ListBlock — never stored on this row's own node. The marker ordinal is
    /// [`list_index`](Self::list_index).
    pub list_style: Option<ListStyle>,
    /// The 1-based position of this item within its list. Only meaningful when
    /// [`list_style`](Self::list_style) is set; `0` otherwise.
    pub list_index: usize,
    /// The grammar a code block highlights as. `None` on every other kind and
    /// on a code block with no language chosen. Read into the row so the
    /// highlight pass does not re-read the CRDT, and part of `adopt`'s `same`
    /// test so changing it re-highlights.
    pub language: Option<String>,
    /// Synced table width policy, read into the row with the rest of its meta.
    pub table_layout: TableLayout,
    /// Derived grid cache. Canonical content remains [`Self::text`].
    pub table: Option<crate::table::TableVisual>,
    /// Whether this view is showing the table's canonical Markdown instead of
    /// its derived grid. Local projection state, refreshed from `ViewState`
    /// before reflow so the row reserves height for the thing actually drawn.
    pub table_source_visible: bool,
    /// What this frame shows for a `mermaid` code block: the drawing, the
    /// placeholder, or the text with its reason. `None` on every other row.
    /// Mirrored from `ViewState::diagrams` by `crate::diagram::sync` before
    /// reflow, the way `table_source_visible` is; `adopt` leaves it alone.
    pub diagram: Option<crate::diagram::DiagramRowState>,
    /// The width of the language chip for *this* row's label, measured in
    /// `build_row` so the hit-test rect and the drawn rect agree — the chip
    /// wraps its text with padding rather than a fixed width.
    pub lang_chip_w: f32,
    /// The line's text, cached so rendering does not re-read the CRDT.
    pub text: String,
    /// Formatting and pill anchors, the input to `layout::build_row`.
    pub runs: Vec<TextRun>,
    /// Each pill's palette, caption and dangling state, resolved once per
    /// rebuild so drawing a frame never reads the document.
    pub pills: HashMap<koru_core::AttrId, PillInfo>,
    /// The attribute chips this row's node earns — a tag chip per `koru.tag`,
    /// a swatch for `koru.color` — derived once per rebuild/refresh by
    /// [`crate::chips::attribute_chips`]. Empty for the common row, and an
    /// empty vec draws nothing: unset attributes are invisible.
    pub chips: Vec<crate::chips::ChipSpec>,
    pub icon: Option<String>,
    pub level_name: Option<String>,

    /// The image this row renders instead of text. `None` for every text row;
    /// the one content shape a `Paragraph` may carry besides text.
    pub image: Option<ImageRef>,

    pub layout: Layout<Brush>,
    pub selection: Selection,
    /// Where the caret goes once the layout is rebuilt. A text edit invalidates
    /// every byte index, so the caret is set *after* reflow, never during.
    pending_caret: Option<usize>,
    /// The same, for a range.
    pending_selection: Option<(usize, usize)>,
    /// Formatting armed for the next keystroke, from Ctrl+B with no selection.
    ///
    /// A mark cannot mark nothing, so there is no way to record the intent in
    /// the document until there is text to put it on. It waits here.
    pending_marks: Option<MarkSet>,
    /// The same for value marks — a highlight colour, a script.
    pending_style: Option<TextStyle>,
    pub y: f32,
    pub height: f32,
    /// Blank space opened *above* this row by its heading level.
    ///
    /// Not part of [`y`](Self::y) and not part of [`height`](Self::height).
    /// Every geometry that maps a point into the parley layout — a caret, a
    /// pill, a selection rect — measures from the first glyph, and a row whose
    /// origin sat above its own text would have to subtract this everywhere. It
    /// is recorded instead, and the two places that care about the *band* a row
    /// owns add it back: `Body::row_at_y` and `Body::row_near_y`.
    ///
    /// Layout output, like `y` and `height` — `adopt` leaves it alone, and a
    /// background tab's is stale until it next reflows.
    pub space_above: f32,
    /// Set when the cached text or runs no longer match the document.
    pub dirty: bool,
    /// Which [`rowlayout::build_row`](crate::rowlayout::build_row) call last
    /// laid this row out, or `0` for a row that has never been shaped.
    ///
    /// **Not a version of the text — a version of the *layout*.** It is drawn
    /// from a process-wide counter
    /// ([`rowlayout::shapings`](crate::rowlayout::shapings)), so no two
    /// shapings anywhere ever share a number and a cache cannot mistake a row
    /// rebuilt by `Body::rebuild` for the row it replaced. `dirty` says a
    /// layout is *owed*; this says which layout is *there*, which is the
    /// question anything holding a projection of the row has to ask.
    pub(crate) shaped: u64,
    /// This row *is* the document node the body is rooted at — its title.
    ///
    /// Set by [`Body::rebuild`](crate::Body::rebuild) when
    /// [`Body::title_row`](crate::Body::title_row) is on, never read from the
    /// document: a title is a position, not a property, and writing it onto the
    /// node would sync a presentation choice. It draws at
    /// [`theme::TITLE_LEVEL`] without carrying `meta.heading`, so making the
    /// title a heading is not an edit anybody's peer receives.
    pub is_title: bool,
    /// The dictation splice this row is carrying, or `None`.
    ///
    /// **While it is `Some`, [`text`](Self::text) is the *composed* string** —
    /// the authored text with `authored` swapped for the transcript so far —
    /// and so is every byte index derived from the layout. This field is what
    /// tells the two coordinate spaces apart; [`doc_pos`](Self::doc_pos) and
    /// [`layout_pos`](Self::layout_pos) are the only arithmetic that crosses
    /// them. Loro never sees any of it ([ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction)).
    ///
    /// Set by [`Body`](crate::Body) after every row re-read and cleared by
    /// [`adopt`](Self::adopt); see [`crate::dictation`].
    pub composition: Option<crate::dictation::ProvisionalRun>,
    /// Part of a multi-row selection.
    ///
    /// **Not stored, and it must never be.** A selection is one person's, in one
    /// window: syncing it would move a peer's highlight when you pressed
    /// Shift+Down, and open two windows on one workspace and they would fight
    /// over it. `rebuild` throws it away with the row, which is correct — it is
    /// re-derived from the anchor.
    pub selected: bool,
}

/// The image carried by a blob node, or `None` for anything else.
fn image_of(content: &koru_core::Content, meta: &koru_core::NodeMeta) -> Option<ImageRef> {
    match content {
        koru_core::Content::Blob {
            hash,
            mime,
            width,
            height,
            ..
        } => Some(ImageRef {
            hash: hash.clone(),
            mime: mime.clone(),
            width: *width,
            height: *height,
            display: meta.image_size,
        }),
        _ => None,
    }
}

/// The pure half of [`TaskRow`] — everything derivable from a node already read.
///
/// The residue flag is deliberately not answered here: it needs the document,
/// and `from_node`/`adopt` are handed a `Node` precisely so a row can be rebuilt
/// from what the walk already read. `Body` fills it in.
fn task_of(node: &Node) -> Option<TaskRow> {
    let fields = koru_core::tasks::TaskFields::of(node);
    koru_core::tasks::is_task(&fields).then(|| TaskRow {
        completed: fields.lifecycle == koru_core::attributes::Lifecycle::Completed,
        unfinished_children: false,
    })
}

/// The list this node is in, read off `koru.list`.
///
/// Single-valued by construction — `WorkspaceDoc::set_list` writes "one
/// `KEY_LIST` value, replacing whatever list it was in" — so the first is the
/// only, and there is no ambiguity for a caller to resolve.
fn list_of(node: &Node) -> Option<koru_core::ids::ListId> {
    node.content
        .attrs()
        .iter()
        .find(|a| a.role == koru_core::attributes::KEY_LIST)
        .and_then(|a| a.as_text())
        .and_then(koru_core::ids::ListId::parse)
}

impl Paragraph {
    pub(crate) fn from_node(
        node: &Node,
        depth: u16,
        runs: Vec<TextRun>,
        pills: HashMap<koru_core::AttrId, PillInfo>,
        list_style: Option<ListStyle>,
        list_index: usize,
    ) -> Self {
        Self {
            id: node.id,
            kind: node.kind,
            depth,
            done: node.meta.done,
            task: task_of(node),
            list: list_of(node),
            align: node.meta.align.unwrap_or_default(),
            heading: node.meta.heading,
            list_style,
            list_index,
            language: node.meta.language.clone(),
            table_layout: node.meta.table_layout.unwrap_or_default(),
            table: None,
            table_source_visible: false,
            diagram: None,
            lang_chip_w: 0.0,
            text: node.content.as_text().unwrap_or_default().to_string(),
            runs,
            pills,
            chips: crate::chips::attribute_chips(node),
            icon: node.meta.icon.clone(),
            level_name: node.meta.level_name.clone(),
            image: image_of(&node.content, &node.meta),
            layout: Layout::new(),
            selection: Selection::default(),
            pending_caret: None,
            pending_selection: None,
            pending_marks: None,
            pending_style: None,
            y: 0.0,
            height: 0.0,
            space_above: 0.0,
            dirty: true,
            shaped: 0,
            is_title: false,
            composition: None,
            selected: false,
        }
    }

    /// Whether this row draws a checkbox and can be ticked. A `Kind::Task`
    /// always does; a paragraph inside a `Checkbox`-style `ListBlock` does too —
    /// the list style controls the rendering, and `Checkbox` is the one that
    /// interacts with `done` state.
    pub fn is_task(&self) -> bool {
        self.kind.is_task_like() || self.list_style == Some(ListStyle::Checkbox)
    }

    /// This row is a diagram showing its picture — the drawing, or the
    /// placeholder waiting for one — rather than its text. Selected like an
    /// image, opened with Enter, no caret, no chips.
    pub fn diagram_shows_picture(&self) -> bool {
        self.diagram
            .as_ref()
            .is_some_and(crate::diagram::DiagramRowState::shows_picture)
    }

    /// Where this row's text column begins, in logical pixels.
    ///
    /// Every layout, hit-test and caret goes through here rather than calling
    /// `geometry::text_x` with a depth: the column now depends on the checkbox
    /// too, and a call site that remembered one and forgot the other would draw
    /// the glyphs somewhere the clicks do not land.
    pub fn text_x(&self) -> f32 {
        text_x(self.depth, self.is_task() || self.list_style.is_some())
            + if self.kind == Kind::Quote {
                QUOTE_INSET_X
            } else {
                0.0
            }
    }

    /// This row's step on the type scale.
    ///
    /// `koru-draw` names no nodes, so the ramp is keyed by a level number
    /// rather than by `Heading`; this and [`weight`](Self::weight) are the only
    /// places that convert.
    pub fn type_step(&self) -> theme::TypeStep {
        theme::type_step(self.level())
    }

    /// The family this row is set in, decided by its kind/level and the three
    /// document font roles: a code block uses the code font, a heading the
    /// headings font, and everything else the body font. Read live from the
    /// globals, so a role change re-shapes every row once the body is
    /// invalidated.
    pub fn font_choice(&self) -> FontChoice {
        if self.kind == Kind::Code {
            theme::code_font()
        } else if self.level().is_some() {
            theme::headings_font()
        } else {
            theme::body_font()
        }
    }

    /// The weight this row's glyphs are drawn at.
    pub fn weight(&self) -> f32 {
        self.font_choice().weight_for(self.level())
    }

    /// The level on the heading ramp this row draws at.
    ///
    /// The title's level is [`theme::TITLE_LEVEL`] rather than its
    /// `meta.heading`, which it does not have and must not gain: a title is
    /// already the document's name, and writing a heading onto it would be a
    /// presentation choice broadcast to every peer.
    ///
    /// A code block has no level, whatever heading the node carried before
    /// it became code: a heading turned into a ``` fence or a diagram would
    /// otherwise keep the heading's size and leading, and read as a code
    /// block set larger than every other. The heading stays on the node for
    /// the day the block is turned back; it just does not draw while the
    /// block is code.
    fn level(&self) -> Option<u8> {
        if self.is_title {
            return Some(theme::TITLE_LEVEL);
        }
        if self.kind == Kind::Code {
            return None;
        }
        self.heading.map(Heading::level)
    }

    /// The caret's byte offset in this row's text.
    pub fn caret(&self) -> usize {
        self.selection.focus().index().min(self.text.len())
    }

    /// Adopt a selection derived from `self.layout`, clamped back into `text`.
    ///
    /// **The two are not the same coordinate space.** `build_row` shapes
    /// `text + LAYOUT_STRUT` (`rowlayout.rs`), so a layout index may legally sit
    /// up to `LAYOUT_STRUT.len()` past the end of the text — pressing End is the
    /// everyday way to get one. Assigning `self.selection` directly is how that
    /// overshoot reaches `row.text` and panics a slice, or silently pushes an
    /// out-of-range offset into Loro, where the error is discarded and the
    /// keystroke vanishes. Every layout-derived write goes through here
    /// ([ADR-75](../../../docs/decisions/README.md)).
    ///
    /// Not to be confused with [`place_selection`](Self::place_selection),
    /// which *queues* a selection in byte indices for after the next reflow.
    /// This one takes layout indices and applies them now.
    pub(crate) fn set_selection_from_layout(&mut self, sel: Selection) {
        self.selection = sel;
        self.clamp_selection_to_text();
    }

    /// Keep selection indices inside the document text. Layout may carry a
    /// trailing primary-font ZWSP that is not part of `text` (baseline strut).
    pub(crate) fn clamp_selection_to_text(&mut self) {
        let len = self.text.len();
        let a = self.selection.anchor().index();
        let f = self.selection.focus().index();
        if a <= len && f <= len {
            return;
        }
        let a = a.min(len);
        let f = f.min(len);
        let aa = self.selection.anchor().affinity();
        let fa = self.selection.focus().affinity();
        self.selection = if a == f {
            Selection::from_byte_index(&self.layout, f, fa)
        } else {
            Selection::from_byte_index(&self.layout, a, aa).extend(Cursor::from_byte_index(
                &self.layout,
                f,
                fa,
            ))
        };
    }

    /// Adopt fresh content, keeping the layout when nothing that affects it
    /// changed. Without this every keystroke would re-shape every row in the
    /// outline, since a rebuild reads the whole document.
    pub(crate) fn adopt(
        &mut self,
        node: &Node,
        depth: u16,
        runs: Vec<TextRun>,
        mut pills: HashMap<koru_core::AttrId, PillInfo>,
        list_style: Option<ListStyle>,
        list_index: usize,
    ) {
        // A routed document caption is resolved outside this one LoroDoc, from
        // workspace manifests/search. Preserve that newer local resolution
        // when an ordinary text edit re-reads the attribute's cached caption.
        // The stable `(workspace, document)` route decides whether it is the
        // same entity; an unrelated replacement never inherits the override.
        for (attr, next) in &mut pills {
            let Some(current) = self.pills.get(attr) else {
                continue;
            };
            if next.entity.is_some() && next.entity == current.entity {
                next.label.clone_from(&current.label);
                next.trashed = current.trashed;
                if matches!(
                    current.kind,
                    koru_draw::pill::PillKind::Schedule | koru_draw::pill::PillKind::SchedulePaused
                ) {
                    next.kind = current.kind;
                }
            }
        }
        let text = node.content.as_text().unwrap_or_default();
        let align = node.meta.align.unwrap_or_default();
        let heading = node.meta.heading;
        let language = node.meta.language.clone();
        let table_layout = node.meta.table_layout.unwrap_or_default();
        let image = image_of(&node.content, &node.meta);
        let chips = crate::chips::attribute_chips(node);
        // A row's font is no longer stored — it is read live from the body /
        // headings / code globals in `font_choice`, so `adopt` has no font field
        // to track. A role change re-shapes rows through `Body::invalidate`,
        // not through this `same` test.
        let same = self.depth == depth
            && self.done == node.meta.done
            && self.align == align
            && self.heading == heading
            && self.list_style == list_style
            && self.list_index == list_index
            && self.language == language
            && self.table_layout == table_layout
            && self.text == text
            && self.runs == runs
            && self.pills == pills
            && self.chips == chips
            && self.image == image;

        self.kind = node.kind;
        self.depth = depth;
        self.done = node.meta.done;
        // Not part of `same`: the lifecycle changes no glyph and no metric — the
        // strikethrough is `done`'s — and the indicator is drawn beside the
        // layout rather than inside it. The residue half goes back to `false`
        // here on purpose; `Body` answers it again from the document, and a stale
        // `true` would outlive the child that earned it.
        self.task = task_of(node);
        self.list = list_of(node);
        self.align = align;
        self.heading = heading;
        self.list_style = list_style;
        self.list_index = list_index;
        self.language = language;
        self.table_layout = table_layout;
        self.icon = node.meta.icon.clone();
        self.level_name = node.meta.level_name.clone();
        // **A row carrying a dictation splice is always re-read.** `same`
        // compares `self.text` — which is the *composed* string while a
        // composition is open — against the node's authored text, so it is
        // already false in every case but the degenerate one (an empty
        // transcript over an empty range). Taking the splice here rather than
        // trusting that is what makes the re-splice in `Body` idempotent: a row
        // that has been adopted is authored, full stop.
        let composed = self.composition.take().is_some();
        if !same || composed {
            self.text.clear();
            self.text.push_str(text);
            self.runs = runs;
            self.pills = pills;
            self.chips = chips;
            self.image = image;
            self.table = None;
            self.dirty = true;
        }
    }

    /// Splice a dictation transcript over an authored byte range.
    ///
    /// Answers whether it happened. It does not when the row is already
    /// carrying a splice — the caller re-reads first, which is what
    /// [`Body::refresh`](crate::Body::refresh) does — or when the range is not
    /// a valid pair of character boundaries in this row's text, which is how a
    /// range resolved against a document this row has not caught up with is
    /// refused rather than panicking a slice.
    pub(crate) fn compose(&mut self, authored: std::ops::Range<usize>, provisional: &str) -> bool {
        if self.composition.is_some() {
            return false;
        }
        let len = self.text.len();
        if authored.start > authored.end
            || authored.end > len
            || !self.text.is_char_boundary(authored.start)
            || !self.text.is_char_boundary(authored.end)
        {
            return false;
        }
        let mut composed =
            String::with_capacity(len - (authored.end - authored.start) + provisional.len());
        composed.push_str(&self.text[..authored.start]);
        composed.push_str(provisional);
        composed.push_str(&self.text[authored.end..]);
        self.runs = crate::dictation::splice_runs(&self.runs, &authored, provisional.len());
        self.text = composed;
        self.composition = Some(crate::dictation::ProvisionalRun {
            authored,
            text: provisional.to_owned(),
        });
        self.table = None;
        self.dirty = true;
        true
    }

    /// Where a **layout** byte index sits in the authored text.
    ///
    /// The identity when no composition is open. See
    /// [`Mapped`](crate::dictation::Mapped) for why this is not one half of a
    /// pair of total inverses.
    pub fn doc_pos(&self, layout: usize) -> crate::dictation::Mapped {
        crate::dictation::doc_pos(self.composition.as_ref(), layout)
    }

    /// Where an **authored** byte index sits in the composed layout.
    ///
    /// The identity when no composition is open. See
    /// [`Mapped`](crate::dictation::Mapped).
    pub fn layout_pos(&self, doc: usize) -> crate::dictation::Mapped {
        crate::dictation::layout_pos(self.composition.as_ref(), doc)
    }

    /// A parley selection over one byte range of this row's *layout*, for
    /// measuring rather than for editing — the provisional run's rectangles.
    pub(crate) fn layout_selection(&self, span: std::ops::Range<usize>) -> Selection {
        Selection::from_byte_index(&self.layout, span.start, Affinity::Downstream).extend(
            Cursor::from_byte_index(&self.layout, span.end, Affinity::Upstream),
        )
    }

    /// Which visual line the caret sits on.
    ///
    /// A node is one `Layout`, but a long one wraps, so "up" inside a paragraph
    /// is not the same as "up" to the previous one. The owner needs this to know
    /// which it is.
    pub fn line_index(&self) -> usize {
        Cluster::from_byte_index(&self.layout, self.caret())
            .map(|c| c.path().line_index())
            .unwrap_or_else(|| self.layout.len().saturating_sub(1))
    }

    /// How many visual lines this paragraph occupies.
    pub fn line_count(&self) -> usize {
        self.layout.len()
    }

    /// Which shaping produced the layout this row is carrying, or `0` if it has
    /// never been shaped. See the field's own note for why it is process-wide.
    ///
    /// A [`dirty`](Self::dirty) row is carrying the *previous* shaping's layout
    /// or none at all, which is why anything keyed on this must look at `dirty`
    /// too before believing the geometry.
    pub fn shaped_at(&self) -> u64 {
        self.shaped
    }

    /// The caret's x in *document* coordinates.
    ///
    /// The sticky column vertical motion aims at. It has to include the depth
    /// indent, or moving between rows at different depths drifts sideways.
    ///
    /// When a pill starts the line, parley's raw geometry sits on the pill's
    /// right edge; callers that have a pill cache should prefer
    /// [`crate::rowlayout::caret_geometry`]. This path keeps the uncached
    /// approximation used by sticky-column motion before a reflow.
    pub fn caret_x(&self) -> f32 {
        self.selection.focus().geometry(&self.layout, 1.0).x0 as f32 + self.text_x()
    }

    /// Caret x with pill edges corrected (needs the layout-time pill cache).
    /// **Whether this row's text is struck through**, and the one place that
    /// rule lives.
    ///
    /// A *task* answers with its canonical lifecycle. `Kind::Task` is not what
    /// decides it: a `/due` on an ordinary paragraph makes a task and leaves
    /// the kind alone, so completing it must strike a line that draws no box.
    ///
    /// A checkbox-styled list item that is **not** a task has no lifecycle to
    /// read, and its box is the whole of what it means — that arm keeps `done`.
    /// A plain paragraph is never struck.
    ///
    /// **`done` alone is never enough**, and that is the bug this method was
    /// extracted for: `remove_from_tasks` deliberately leaves `NodeMeta::done`
    /// standing (the checkbox survives), so a rule reading only the mirror kept
    /// striking a row that was no longer a task and had no Reopen left to
    /// offer.
    pub fn struck(&self) -> bool {
        match self.task {
            Some(task) => task.completed,
            None => self.done && self.is_task(),
        }
    }

    pub fn caret_x_with_pills(&self, pills: &koru_draw::pill::PillCache) -> f32 {
        crate::rowlayout::caret_geometry(self, pills, 1.0).x0 as f32 + self.text_x()
    }

    /// Everything selected.
    pub fn select_all(&mut self) {
        self.selection =
            Selection::from_byte_index(&self.layout, 0usize, Affinity::Downstream).extend(
                Cursor::from_byte_index(&self.layout, self.text.len(), Affinity::Upstream),
            );
    }

    /// The byte offset one extended grapheme cluster — or one word — before `at`.
    ///
    /// Character motion uses **Unicode grapheme clusters**, not parley visual
    /// clusters alone: a skin-tone emoji or ZWJ sequence is one user-perceived
    /// character, and landing the caret inside it (then typing) splits the
    /// sequence into tofu. Word motion still comes from parley, with a pill
    /// special-case: the anchor is `U+2060 WORD JOINER`, so parley would read
    /// `task ⁠` as one word and `Ctrl+Backspace` would take the whole line.
    pub fn before(&self, at: usize, by_word: bool) -> usize {
        let at = at.min(self.text.len());
        if by_word && at >= ANCHOR_LEN && self.text[..at].ends_with(ANCHOR) {
            return at - ANCHOR_LEN;
        }
        if by_word {
            let cursor = Cursor::from_byte_index(&self.layout, at, Affinity::Downstream);
            let stop = cursor
                .previous_visual_word(&self.layout)
                .index()
                .min(at)
                .min(self.text.len());
            // A word never reaches *across* a pill either.
            return match self.text[stop..at].rfind(ANCHOR) {
                Some(i) => stop + i + ANCHOR_LEN,
                None => stop,
            };
        }
        grapheme_before(&self.text, at)
    }

    /// The byte offset one extended grapheme cluster — or one word — after `at`.
    ///
    /// The mirror of [`before`], pill and all — `Ctrl+Delete` forwards over one
    /// takes the pill and stops.
    ///
    /// [`before`]: Self::before
    pub fn after(&self, at: usize, by_word: bool) -> usize {
        let at = at.min(self.text.len());
        if by_word && self.text[at..].starts_with(ANCHOR) {
            return at + ANCHOR_LEN;
        }
        if by_word {
            let cursor = Cursor::from_byte_index(&self.layout, at, Affinity::Downstream);
            // Layout may end past `text` (trailing ZWSP strut) — never slice past it.
            let stop = cursor
                .next_visual_word(&self.layout)
                .index()
                .max(at)
                .min(self.text.len());
            return match self.text[at..stop].find(ANCHOR) {
                Some(i) => at + i,
                None => stop,
            };
        }
        grapheme_after(&self.text, at)
    }

    /// Snap a byte index to the nearest extended-grapheme boundary (preferring
    /// the start of the containing cluster). Stops a click or restore from
    /// leaving the caret inside a multi-codepoint emoji.
    pub fn snap_caret(text: &str, at: usize) -> usize {
        snap_grapheme(text, at)
    }

    /// End of the extended grapheme cluster that starts at `start`.
    pub fn grapheme_end(text: &str, start: usize) -> usize {
        grapheme_after(text, start)
    }

    /// Whether the current (non-collapsed) selection is only emoji graphemes.
    ///
    /// The formatting bubble offers Bold/TURN INTO for text; those do nothing
    /// useful on pure emoji, so the document view suppresses the bubble then.
    pub fn selection_is_emoji_only(&self) -> bool {
        let r = self.selection.text_range();
        if r.is_empty() || r.end > self.text.len() || !self.text.is_char_boundary(r.start) {
            return false;
        }
        let slice = &self.text[r.start..r.end];
        if slice.is_empty() {
            return false;
        }
        slice
            .graphemes(true)
            .all(|g| !g.chars().all(|c| c.is_whitespace()) && emojis::get(g).is_some())
    }

    /// Tone-capable emoji grapheme under a content-local point, if any.
    ///
    /// Returns the byte range and the grapheme slice. Non-emoji and emoji that
    /// do not take a Fitzpatrick tone are `None`.
    pub fn tone_emoji_at(&self, lx: f32, ly: f32) -> Option<(std::ops::Range<usize>, &str)> {
        let at = Selection::from_point(&self.layout, lx, ly)
            .focus()
            .index()
            .min(self.text.len());
        let start = snap_grapheme(&self.text, at);
        // If the hit snapped to a boundary between two clusters, prefer the
        // grapheme that contains the original byte when it fell inside one.
        let start = if at > start && at < grapheme_after(&self.text, start) {
            start
        } else if at > 0 && at == start {
            // Landed exactly on a boundary — use the cluster to the left only
            // if `at` was closer to the previous cluster's interior (skip).
            start
        } else {
            start
        };
        let end = grapheme_after(&self.text, start);
        if start >= end {
            return None;
        }
        let g = &self.text[start..end];
        if !crate::emoji::has_skin_tones(g) {
            return None;
        }
        Some((start..end, g))
    }

    /// Is the caret at the very start? The owner asks before deciding whether
    /// Backspace deletes a character or merges this line into the one above.
    pub fn at_start(&self) -> bool {
        self.caret() == 0
    }

    pub fn at_end(&self) -> bool {
        self.caret() >= self.text.len()
    }

    // ------------------------------------------------------------ editing
    //
    // Everything below acts on this line and no other. What is *not* here is
    // anything touching two nodes — Enter splitting, Backspace merging into the
    // row above — because a paragraph has no way to reach its neighbours and
    // should not grow one.

    /// The selected byte range, empty when the caret is collapsed.
    ///
    /// **Clamped, like `caret()`** — and for a reason `place_selection` does not
    /// already cover: `adopt` replaces `text` and only marks the row dirty, so
    /// between a remote edit arriving and the next `build_row` a perfectly good
    /// index can be left pointing past the end of shorter text. `caret()` has
    /// always defended against that; this is its sibling, and the asymmetry
    /// between them was a live bug.
    pub fn range(&self) -> std::ops::Range<usize> {
        let len = self.text.len();
        let r = self.selection.text_range();
        r.start.min(len)..r.end.min(len)
    }

    /// The range a formatting command acts on, and whether it was **widened**
    /// from a collapsed caret to the whole row.
    ///
    /// A collapsed caret at the very *start* of a row that has text means the
    /// row: `Ctrl+B` there reads as "bold this paragraph", and arming the next
    /// keystroke instead looks like the shortcut did nothing. Anywhere else — an
    /// empty row, or a caret mid-line — keeps the arming, which is what makes
    /// "press bold, then type" work.
    fn format_range(&self) -> (std::ops::Range<usize>, bool) {
        let range = self.range();
        if range.is_empty() && range.start == 0 && !self.text.is_empty() {
            return (0..self.text.len(), true);
        }
        (range, false)
    }

    /// Where the caret goes once a formatting command has written.
    ///
    /// A command that acted on a real selection leaves it selected, so the
    /// bubble stays up and a second press visibly toggles back. **A widened one
    /// does not**: the row was never selected, and selecting it here would throw
    /// the selection bubble over a line nobody highlighted. The caret stays at
    /// the start, which is also what makes the second press read the whole row
    /// again and turn it off.
    fn settle_format(&mut self, range: std::ops::Range<usize>, widened: bool) {
        if widened {
            self.pending_caret = Some(0);
        } else {
            self.pending_selection = Some((range.start, range.end));
        }
    }

    /// Re-read this line from the document. `false` when the node is gone —
    /// a remote delete, or a merge — and the caller has to rebuild.
    pub fn refresh(&mut self, ws: &WorkspaceDoc) -> bool {
        let Ok(node) = ws.read(self.id) else {
            return false;
        };
        let runs = ws.runs(self.id).unwrap_or_default();
        let pills = crate::pillinfo::info_for_node(ws, &node);
        let depth = self.depth;
        // `list_style`/`list_index` are a renderer cache sourced from the
        // enclosing ListBlock; `refresh` does not re-derive them (a structural
        // change rebuilds the body rather than refreshing one row).
        let list_style = self.list_style;
        let list_index = self.list_index;
        self.adopt(&node, depth, runs, pills, list_style, list_index);
        true
    }

    /// Formatting armed for the next keystroke, so a toolbar can show it.
    pub fn armed(&self) -> (Option<MarkSet>, Option<TextStyle>) {
        (self.pending_marks, self.pending_style.clone())
    }

    /// Forget formatting armed for the next keystroke.
    ///
    /// Arming belongs to an insertion point, so moving that point cancels it —
    /// otherwise pressing bold, changing your mind and arrowing away would
    /// embolden whatever you typed next, somewhere you had stopped looking.
    pub fn disarm(&mut self) {
        self.pending_marks = None;
        self.pending_style = None;
    }

    /// Collapse any text selection to a caret. Used when the row joins a
    /// **block** selection — the two modes must not paint on top of each other.
    pub fn collapse_text_selection(&mut self) {
        if self.selection.is_collapsed() && self.pending_selection.is_none() {
            return;
        }
        let at = snap_grapheme(&self.text, self.caret().min(self.text.len()));
        self.pending_selection = None;
        self.pending_caret = None;
        // Apply immediately so draw does not wait for the next reflow to drop
        // the purple range on a block-selected row.
        self.selection = Selection::from_byte_index(&self.layout, at, Affinity::Downstream);
        self.snap_selection_to_graphemes();
    }

    /// Place the caret here once the layout has been rebuilt.
    ///
    /// Never during an edit: a text change invalidates every byte index, so the
    /// caret is always set afterwards, by [`apply_pending`](Self::apply_pending).
    /// The index is snapped to a grapheme boundary so a mid-emoji restore cannot
    /// leave the caret inside a ZWJ sequence.
    pub fn place_caret(&mut self, at: usize) {
        self.pending_caret = Some(snap_grapheme(&self.text, at));
    }

    /// Place a live caret from a point in this paragraph's layout.
    ///
    /// Right-click uses this without starting a text drag or opening a pill's
    /// picker. Point-to-byte conversion remains here beside every other parley
    /// selection operation, including the grapheme clamp.
    pub fn place_caret_from_point(&mut self, x: f32, y: f32) {
        self.selection = Selection::from_point(&self.layout, x, y);
        self.snap_selection_to_graphemes();
    }

    /// The byte offset under a point, without moving the caret or selection.
    ///
    /// A right-click *inside* a selection keeps that selection — but the menu
    /// still needs to know which word the press landed on (its spelling rows),
    /// and asking through the live caret would answer for the wrong place.
    pub fn byte_at_point(&self, x: f32, y: f32) -> usize {
        let hit = Selection::from_point(&self.layout, x, y);
        snap_grapheme(&self.text, hit.focus().index().min(self.text.len()))
    }

    pub fn place_selection(&mut self, from: usize, to: usize) {
        self.pending_selection = Some((
            snap_grapheme(&self.text, from),
            snap_grapheme(&self.text, to),
        ));
    }

    /// Apply a byte-range selection **now**, anchor to focus, without waiting
    /// for a reflow. For restores that run outside any layout pass — the text
    /// has not changed, so the live layout is the right one to index into.
    pub fn select_range_now(&mut self, from: usize, to: usize) {
        let from = snap_grapheme(&self.text, from.min(self.text.len()));
        let to = snap_grapheme(&self.text, to.min(self.text.len()));
        self.pending_selection = None;
        self.pending_caret = None;
        self.selection =
            Selection::from_byte_index(&self.layout, from, Affinity::Downstream).extend(
                Cursor::from_byte_index(&self.layout, to, Affinity::Upstream),
            );
    }

    /// Rebuild the live selection anchored at byte `anchor` and extended to a
    /// point — the drag-returned-to-its-press-row path, where the press-time
    /// parley anchor was destroyed by the block-selection detour's collapse.
    pub fn select_from_anchor_to_point(&mut self, anchor: usize, x: f32, y: f32) {
        let anchor = snap_grapheme(&self.text, anchor.min(self.text.len()));
        self.selection = Selection::from_byte_index(&self.layout, anchor, Affinity::Downstream)
            .extend_to_point(&self.layout, x, y);
        self.snap_selection_to_graphemes();
    }

    /// The live selection's directed (anchor, focus) byte pair, `None` when
    /// collapsed. Direction matters to the caller: a restore that anchored at
    /// the wrong end would grow backwards on the next Shift+Arrow.
    pub fn directed_range(&self) -> Option<(usize, usize)> {
        let a = self.selection.anchor().index().min(self.text.len());
        let f = self.caret();
        (a != f).then_some((a, f))
    }

    /// Snap the live selection onto grapheme boundaries. Used after
    /// `Selection::from_point`, which can land inside a multi-codepoint emoji.
    /// Also clamps off the layout-only trailing ZWSP.
    ///
    /// **Preserves anchor vs focus.** Rebuilding from `text_range()` (always
    /// min..max) flipped reverse selections every drag frame, so a backward
    /// mouse drag could not grow.
    pub fn snap_selection_to_graphemes(&mut self) {
        self.clamp_selection_to_text();
        let a_idx = snap_grapheme(&self.text, self.selection.anchor().index());
        let f_idx = snap_grapheme(&self.text, self.selection.focus().index());
        let aa = self.selection.anchor().affinity();
        let fa = self.selection.focus().affinity();
        if a_idx == f_idx {
            self.selection = Selection::from_byte_index(&self.layout, f_idx, fa);
        } else {
            self.selection = Selection::from_byte_index(&self.layout, a_idx, aa)
                .extend(Cursor::from_byte_index(&self.layout, f_idx, fa));
        }
    }

    /// Apply whatever was queued, now that `layout` is current. Called by the
    /// reflow, once per row — which is why a row that queued a caret gets it,
    /// rather than the focused row getting somebody else's.
    pub fn apply_pending(&mut self) {
        if let Some((from, to)) = self.pending_selection.take() {
            let from = snap_grapheme(&self.text, from.min(self.text.len()));
            let to = snap_grapheme(&self.text, to.min(self.text.len()));
            self.selection =
                Selection::from_byte_index(&self.layout, from, Affinity::Downstream).extend(
                    Cursor::from_byte_index(&self.layout, to, Affinity::Upstream),
                );
            self.pending_caret = None;
        }
        if let Some(index) = self.pending_caret.take() {
            let index = snap_grapheme(&self.text, index.min(self.text.len()));
            self.selection = Selection::from_byte_index(&self.layout, index, Affinity::Downstream);
        }
    }

    /// Replace the selection with `s`, or insert it at the caret.
    pub fn insert_text(&mut self, ws: &mut WorkspaceDoc, s: &str) {
        let (id, range) = (self.id, self.range());
        // A run of keystrokes is one undo step; the boundaries are set by
        // `checkpoint`, at every structural change.
        ws.begin_group();
        let at = range.start;
        // What the new text will inherit, read *before* it exists. Clearing a
        // value mark is not a no-op — it disturbs a neighbouring pill's span —
        // so below we only write the ones that actually differ.
        let inherited = self
            .pending_style
            .is_some()
            .then(|| ws.style_at(id, at).unwrap_or_default())
            .unwrap_or_default();
        if !range.is_empty() {
            let _ = ws.delete_text(id, range);
        }
        let _ = ws.insert_text(id, at, s);

        // Marks expand rightwards, so text typed after a bold word arrives bold
        // on its own. An armed set is applied on top — including turning marks
        // *off*, which is what makes Ctrl+B at the end of a bold word work.
        if let Some(marks) = self.pending_marks.take() {
            for mark in Mark::ALL {
                let _ = ws.set_mark(id, at..at + s.len(), mark, marks.has(mark));
            }
        }
        if let Some(style) = self.pending_style.take() {
            let span = at..at + s.len();
            if style.highlight != inherited.highlight {
                let _ = ws.set_highlight(id, span.clone(), style.highlight.as_deref());
            }
            if style.script != inherited.script {
                let _ = ws.set_script(id, span.clone(), style.script);
            }
            if style.size != inherited.size {
                let _ = ws.set_size(id, span, style.size);
            }
        }
        self.refresh(ws);
        self.pending_caret = Some(at + s.len());
    }

    /// Delete within this line. `false` means the caret was already at the edge
    /// and the *owner* has to merge with a neighbour — the one thing here that
    /// cannot be done from inside one paragraph.
    pub fn delete_within(&mut self, ws: &mut WorkspaceDoc, backwards: bool, by_word: bool) -> bool {
        // Before anything else: deleting is part of the same undo run as the
        // typing it is correcting.
        ws.begin_group();
        let (id, range) = (self.id, self.range());
        if !range.is_empty() {
            let at = range.start;
            let _ = ws.delete_text(id, range);
            self.refresh(ws);
            self.pending_caret = Some(at);
            return true;
        }

        let caret = self.caret();
        if backwards {
            if self.at_start() {
                return false;
            }
            // Word boundaries and cluster boundaries both come from parley, so
            // a pill anchor is one unit either way — never a partial anchor.
            let prev = self.before(caret, by_word);
            let _ = ws.delete_text(id, prev..caret);
            self.refresh(ws);
            self.pending_caret = Some(prev);
        } else {
            if self.at_end() {
                return false;
            }
            let next = self.after(caret, by_word);
            let _ = ws.delete_text(id, caret..next);
            self.refresh(ws);
            self.pending_caret = Some(caret);
        }
        true
    }

    /// Move or extend within this line. `false` means the caret is at an edge
    /// with nothing selected and wants the neighbouring row instead.
    ///
    /// Character steps use extended grapheme clusters (`before` / `after`), not
    /// parley visual clusters — otherwise Left/Right walk inside skin-tone and
    /// ZWJ emoji. Word steps still come from parley.
    pub fn move_within(&mut self, dir: i32, by_word: bool, extend: bool) -> bool {
        let sel = self.selection;
        if !extend && sel.is_collapsed() {
            if dir < 0 && self.at_start() {
                return false;
            }
            if dir > 0 && self.at_end() {
                return false;
            }
        }
        if by_word {
            self.selection = if dir < 0 {
                sel.previous_visual_word(&self.layout, extend)
            } else {
                sel.next_visual_word(&self.layout, extend)
            };
            self.clamp_selection_to_text();
            return true;
        }
        // Move the focus by one grapheme; keep the anchor when extending.
        let at = sel.focus().index();
        let next = if dir < 0 {
            self.before(at, false)
        } else {
            self.after(at, false)
        };
        let affinity = if dir < 0 {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        let focus = Cursor::from_byte_index(&self.layout, next, affinity);
        self.selection = if extend {
            sel.extend(focus)
        } else {
            focus.into()
        };
        true
    }

    /// Put the caret at one end of the line, for arriving from a neighbour.
    pub fn caret_to_edge(&mut self, start: bool) {
        let at = if start { 0 } else { self.text.len() };
        let affinity = if start {
            Affinity::Downstream
        } else {
            Affinity::Upstream
        };
        self.selection = Selection::from_byte_index(&self.layout, at, affinity);
    }

    pub fn toggle_mark(&mut self, ws: &mut WorkspaceDoc, mark: Mark) {
        let (id, (range, widened)) = (self.id, self.format_range());
        if range.is_empty() {
            // Start from what the caret already sits inside, so Ctrl+B in the
            // middle of a bold word turns bold *off* for what comes next.
            let mut marks = self
                .pending_marks
                .unwrap_or_else(|| ws.marks_at(id, range.start).unwrap_or_default());
            marks.toggle(mark);
            self.pending_marks = Some(marks);
            return;
        }

        // Intersection, not union: a partly-bold selection becomes all bold.
        let on = !ws.marks_in(id, range.clone()).unwrap_or_default().has(mark);
        let _ = ws.set_mark(id, range.clone(), mark, on);
        self.refresh(ws);
        self.settle_format(range, widened);
    }

    pub fn toggle_script(&mut self, ws: &mut WorkspaceDoc, script: Script) {
        let (id, (range, widened)) = (self.id, self.format_range());
        // Armed rather than applied, for the same reason as a highlight.
        if range.is_empty() {
            let current = match &self.pending_style {
                Some(style) => style.script,
                None => ws.style_at(id, range.start).unwrap_or_default().script,
            };
            let mut style = self.pending_style.take().unwrap_or_default();
            style.script = (current != Some(script)).then_some(script);
            self.pending_style = Some(style);
            return;
        }
        let current = ws.style_in(id, range.clone()).unwrap_or_default().script;
        let next = (current != Some(script)).then_some(script);
        let _ = ws.set_script(id, range.clone(), next);
        self.refresh(ws);
        self.settle_format(range, widened);
    }

    pub fn cycle_highlight(&mut self, ws: &mut WorkspaceDoc) {
        let (id, (range, widened)) = (self.id, self.format_range());
        let names = theme::highlight_names();

        // No selection: there is nothing to mark yet, so the colour is armed
        // for whatever comes next — press highlight, type, and it is
        // highlighted. Pressing again cycles the armed colour rather than
        // re-reading the document, so the toolbar shows what you chose.
        if range.is_empty() {
            let current = match &self.pending_style {
                Some(style) => style.highlight.clone(),
                None => ws.style_at(id, range.start).unwrap_or_default().highlight,
            };
            let mut style = self.pending_style.take().unwrap_or_default();
            style.highlight = next_swatch(current.as_deref(), &names).map(str::to_string);
            self.pending_style = Some(style);
            return;
        }

        let current = ws.style_in(id, range.clone()).unwrap_or_default().highlight;
        let next = next_swatch(current.as_deref(), &names);
        let _ = ws.set_highlight(id, range.clone(), next);
        self.refresh(ws);
        self.settle_format(range, widened);
    }

    /// Inline marks and value marks, not block properties.
    ///
    /// Alignment and the heading level are deliberately untouched. This works on
    /// a *range*, and a block property has none — so with a two-word selection
    /// inside an H1 it would silently demote the whole line, and with no
    /// selection at all it would mean something different again. One chord, one
    /// scope; each block property's own control toggles it off.
    pub fn clear_formatting(&mut self, ws: &mut WorkspaceDoc) {
        let (id, (range, widened)) = (self.id, self.format_range());
        if range.is_empty() {
            self.pending_marks = Some(MarkSet::default());
            self.pending_style = Some(TextStyle::default());
            return;
        }
        for mark in Mark::ALL {
            let _ = ws.set_mark(id, range.clone(), mark, false);
        }
        // Values as well: "clear formatting" that leaves a highlight behind is
        // not what anybody means by it.
        let _ = ws.set_highlight(id, range.clone(), None);
        let _ = ws.set_size(id, range.clone(), None);
        let _ = ws.set_script(id, range.clone(), None);
        self.refresh(ws);
        self.settle_format(range, widened);
    }

    /// Alignment is a property of the block, so it needs no selection.
    ///
    /// Pressing "centre" on an already-centred row returns it to the default,
    /// so one chord is both apply and undo.
    pub fn set_align(&mut self, ws: &mut WorkspaceDoc, align: Align) {
        let next = (self.align != align).then_some(align);
        // Closes the run of typing this interrupted, so one Ctrl+Z takes back
        // the alignment and not the sentence. `UndoManager`'s merge interval is
        // 0 and `insert_text` leaves a group open — see AGENTS.md.
        ws.checkpoint();
        if ws.set_align(self.id, next).is_ok() {
            self.refresh(ws);
            // Alignment changes where every glyph sits, so the layout is stale
            // even though the text is not.
            self.dirty = true;
        }
    }

    /// A heading level is a property of the block, so it needs no selection.
    ///
    /// Pressing H2 on a line that is already an H2 returns it to body text: one
    /// control is both apply and undo, the way the alignment buttons are.
    /// `None` is body text unconditionally, which is what `Ctrl+Alt+0` means.
    pub fn set_heading(&mut self, ws: &mut WorkspaceDoc, heading: Option<Heading>) {
        let next = heading.filter(|h| self.heading != Some(*h));
        ws.checkpoint();
        if ws.set_heading(self.id, next).is_ok() {
            self.refresh(ws);
            // A heading changes the size, the weight and the leading of every
            // glyph, so the layout is stale even though the text is not.
            self.dirty = true;
        }
    }
}

/// The next swatch in the cycle, ending at none. A colour this build does not
/// know starts the cycle over rather than being skipped — a mixed selection does
/// the same. One function, so an armed colour and an applied one cannot disagree
/// about the order.
pub(crate) fn next_swatch(current: Option<&str>, names: &[&'static str]) -> Option<&'static str> {
    match current.and_then(|c| names.iter().position(|n| *n == c)) {
        Some(i) if i + 1 < names.len() => Some(names[i + 1]),
        Some(_) => None,
        None => names.first().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rowlayout;
    use koru_draw::text::TextEngine;

    /// A laid-out paragraph. The layout has to be real: every method here asks
    /// parley something, so a default `Layout` would make them all agree on
    /// nothing.
    fn para(text: &str) -> Paragraph {
        let node = Node::task(text).unwrap();
        let mut p = Paragraph::from_node(&node, 0, Vec::new(), HashMap::new(), None, 0);
        let mut engine = TextEngine::new();
        rowlayout::build_row(&mut engine, &mut p, 600.0);
        p
    }

    fn caret_at(text: &str, at: usize) -> Paragraph {
        let mut p = para(text);
        p.selection = Selection::from_byte_index(&p.layout, at, Affinity::Downstream);
        p
    }

    #[test]
    fn the_ends_are_recognised_as_ends() {
        let p = caret_at("hello", 0);
        assert!(p.at_start() && !p.at_end());

        let p = caret_at("hello", 5);
        assert!(p.at_end() && !p.at_start());

        let p = caret_at("hello", 2);
        assert!(!p.at_start() && !p.at_end());
    }

    /// An empty line is at both ends at once. Backspace there merges upward and
    /// Delete pulls the next line up, and both have to agree it is a boundary.
    #[test]
    fn an_empty_line_is_at_both_ends() {
        let p = para("");
        assert!(p.at_start() && p.at_end());
    }

    /// Ctrl+B with the caret at the very start means "bold this paragraph".
    /// Arming the next keystroke there looks like the shortcut did nothing.
    #[test]
    fn formatting_at_the_start_of_a_row_takes_the_whole_row() {
        assert_eq!(caret_at("hello", 0).format_range(), (0..5, true));
    }

    /// ...but only there. A caret mid-line still arms, or "press bold, then
    /// type" would embolden everything behind it.
    #[test]
    fn formatting_anywhere_else_still_arms() {
        for p in [caret_at("hello", 2), caret_at("hello", 5), para("")] {
            let (range, widened) = p.format_range();
            assert!(range.is_empty() && !widened);
        }
    }

    /// A real selection is never widened, even one that starts at 0.
    #[test]
    fn a_selection_is_left_alone() {
        let mut p = para("hello");
        p.selection = Selection::from_byte_index(&p.layout, 0, Affinity::Downstream)
            .extend(Cursor::from_byte_index(&p.layout, 2, Affinity::Upstream));
        assert_eq!(p.format_range(), (0..2, false));
    }

    /// **A widened format leaves the caret collapsed.** Selecting the row would
    /// throw the document's selection bubble over a line nobody highlighted —
    /// the shortcut is meant to format quietly, not to summon a toolbar.
    #[test]
    fn a_widened_format_does_not_select_the_row() {
        let mut p = caret_at("hello", 0);
        let (range, widened) = p.format_range();
        p.settle_format(range, widened);
        assert_eq!(p.pending_caret, Some(0));
        assert_eq!(p.pending_selection, None, "and so no bubble appears");
    }

    /// A command over a real selection still leaves it selected, so the bubble
    /// stays up and a second press visibly toggles back.
    #[test]
    fn a_real_selection_stays_selected_after_formatting() {
        let mut p = para("hello");
        p.selection = Selection::from_byte_index(&p.layout, 1, Affinity::Downstream)
            .extend(Cursor::from_byte_index(&p.layout, 4, Affinity::Upstream));
        let (range, widened) = p.format_range();
        p.settle_format(range, widened);
        assert_eq!(p.pending_selection, Some((1, 4)));
    }

    /// Cluster motion, not byte motion. A multi-byte character is one step, or
    /// arrowing through it would land inside a codepoint.
    #[test]
    fn motion_steps_by_cluster_not_by_byte() {
        let p = para("héllo");
        assert_eq!(p.after(0, false), 1);
        assert_eq!(p.after(1, false), 3, "é is two bytes and one step");
        assert_eq!(p.before(3, false), 1);
    }

    /// parley's word motion stops at the *end* of a word, not the start of the
    /// next one — so Ctrl+Delete from the start of "one" takes "one" and leaves
    /// the space. Measured rather than assumed; it is the opposite of what some
    /// editors do.
    #[test]
    fn word_motion_stops_at_the_end_of_a_word() {
        let p = para("one two three");
        assert_eq!(
            p.after(0, true),
            3,
            "the end of \"one\", not the start of \"two\""
        );
        assert_eq!(p.after(3, true), 7, "then the end of \"two\"");
        assert_eq!(p.before(13, true), 8, "back to the start of \"three\"");
    }

    /// Both directions clamp. Without this a delete could be handed a reversed
    /// range, which panics rather than deleting nothing.
    #[test]
    fn motion_never_moves_the_wrong_way() {
        let p = para("one two");
        assert_eq!(p.before(0, false), 0);
        assert_eq!(p.after(7, false), 7);
        assert_eq!(p.before(0, true), 0);
        assert_eq!(p.after(7, true), 7);
    }

    #[test]
    fn select_all_covers_the_whole_line() {
        let mut p = para("hello world");
        p.select_all();
        assert_eq!(p.selection.text_range(), 0..11);
    }

    /// Backward drag must keep anchor at the press point. Snapping via
    /// `text_range()` used to force focus to the right end every frame.
    #[test]
    fn snap_preserves_a_reverse_selection() {
        let mut p = para("hello world");
        p.selection = Selection::from_byte_index(&p.layout, 11, Affinity::Upstream)
            .extend(Cursor::from_byte_index(&p.layout, 0, Affinity::Downstream));
        assert!(p.selection.anchor().index() > p.selection.focus().index());
        p.snap_selection_to_graphemes();
        assert_eq!(p.selection.anchor().index(), 11);
        assert_eq!(p.selection.focus().index(), 0);
        assert_eq!(p.selection.text_range(), 0..11);
    }

    /// A short line is one visual line; a long one wraps, and vertical motion
    /// needs to know the difference before it decides to leave the paragraph.
    #[test]
    fn a_wrapped_paragraph_reports_more_than_one_line() {
        let short = para("short");
        assert_eq!(short.line_count(), 1);
        assert_eq!(short.line_index(), 0);

        let long = para(&"word ".repeat(60));
        assert!(long.line_count() > 1, "600px cannot hold 60 words");
    }

    /// The sticky column includes the indent, or vertical motion between rows
    /// at different depths drifts sideways.
    #[test]
    fn the_caret_column_includes_the_indent() {
        let flat = caret_at("hello", 0);
        let mut nested = caret_at("hello", 0);
        nested.depth = 2;
        assert!(nested.caret_x() > flat.caret_x());
        assert_eq!(
            nested.caret_x() - flat.caret_x(),
            text_x(2u16, true) - text_x(0u16, true)
        );
    }

    /// A row's step comes from its level, and body text is the body's step.
    #[test]
    fn a_heading_takes_its_size_and_weight_from_the_ramp() {
        let mut p = para("a title");
        assert_eq!(p.type_step(), theme::type_step(None));
        assert_eq!(p.weight(), p.font_choice().weight());

        p.heading = Some(Heading::H2);
        assert_eq!(p.type_step(), theme::type_step(Some(2)));
        assert!(p.type_step().size > theme::FONT_SIZE);
        assert!(p.weight() > p.font_choice().weight());
    }

    /// A level set in another window arrives through `adopt`, which compares
    /// before it rebuilds. Leaving the heading out of that comparison would keep
    /// the old layout and show a heading as prose until something else dirtied
    /// the row — silently, which is why this is pinned.
    #[test]
    fn a_heading_from_another_window_rebuilds_the_layout() {
        let mut p = para("a title");
        p.dirty = false;

        let mut node = Node::task("a title").unwrap();
        node.meta.heading = Some(Heading::H1);
        p.adopt(&node, 0, Vec::new(), HashMap::new(), None, 0);

        assert_eq!(p.heading, Some(Heading::H1));
        assert!(p.dirty, "a new level invalidates the layout");
    }

    #[test]
    fn a_resolved_document_caption_survives_an_ordinary_row_refresh() {
        let mut p = para("see page");
        let attr = koru_core::AttrId::new();
        // The stable document id, which is the whole of a document pill's
        // identity since ADR-127 -- its workspace is resolved when the link is
        // followed and is deliberately not part of what identifies it. What
        // this test needs is only that both sides carry the same key.
        let route = Some("document".to_string());
        p.pills.insert(
            attr,
            PillInfo {
                kind: koru_draw::pill::PillKind::Document,
                label: "Renamed".into(),
                dangling: false,
                trashed: false,
                url: None,
                entity: route.clone(),
            },
        );
        let incoming = HashMap::from([(
            attr,
            PillInfo {
                kind: koru_draw::pill::PillKind::Document,
                label: "Old cached title".into(),
                dangling: false,
                trashed: false,
                url: None,
                entity: route,
            },
        )]);

        p.adopt(
            &Node::task("see page").unwrap(),
            0,
            Vec::new(),
            incoming,
            None,
            0,
        );

        assert_eq!(p.pills[&attr].label, "Renamed");
    }

    /// **Two links into one document are two captions.** `adopt` carries a
    /// resolved caption forward on key equality, so a key naming only the
    /// document would let the first pill's caption land on the second.
    #[test]
    fn adopt_does_not_carry_a_caption_between_two_headings_of_one_document() {
        let mut p = para("see both");
        let (first, second) = (koru_core::AttrId::new(), koru_core::AttrId::new());
        let document = "document";
        let pill = |entity: &str, label: &str| PillInfo {
            kind: koru_draw::pill::PillKind::Document,
            label: label.into(),
            dangling: false,
            trashed: false,
            url: None,
            entity: Some(entity.to_owned()),
        };
        p.pills
            .insert(first, pill(&format!("{document}#node-a"), "Spec#Rollout"));
        p.pills
            .insert(second, pill(&format!("{document}#node-b"), "Spec#Rollback"));

        let incoming = HashMap::from([
            (first, pill(&format!("{document}#node-a"), "stale")),
            (second, pill(&format!("{document}#node-b"), "stale")),
        ]);
        p.adopt(
            &Node::task("see both").unwrap(),
            0,
            Vec::new(),
            incoming,
            None,
            0,
        );

        assert_eq!(p.pills[&first].label, "Spec#Rollout");
        assert_eq!(p.pills[&second].label, "Spec#Rollback");
    }

    /// A paragraph reserves no checkbox column, so its text starts one gutter
    /// left of a task at the same depth.
    #[test]
    fn a_paragraph_starts_where_a_tasks_checkbox_would() {
        let mut task = caret_at("hello", 0);
        task.kind = Kind::Task;
        let mut prose = caret_at("hello", 0);
        prose.kind = Kind::Paragraph;
        assert_eq!(task.text_x() - prose.text_x(), theme::GUTTER);
        assert_eq!(prose.text_x(), theme::MARGIN_X + theme::AFFORDANCE_W);
    }

    /// A skin-tone emoji is one step, not base + modifier.
    #[test]
    fn motion_steps_over_a_skin_tone_emoji_as_one() {
        let thumbs = "👍🏽"; // U+1F44D U+1F3FD
        let p = para(&format!("a{thumbs}b"));
        let after_a = p.after(0, false);
        assert_eq!(after_a, 1, "past 'a'");
        let after_emoji = p.after(after_a, false);
        assert_eq!(
            after_emoji,
            1 + thumbs.len(),
            "one step past the whole emoji"
        );
        assert_eq!(p.before(after_emoji, false), after_a);
        // Mid-sequence is not a legal caret: snap to the start of the cluster.
        assert_eq!(snap_grapheme(&p.text, after_a + 1), after_a);
    }

    /// A ZWJ sequence (handshake / family) is one step too.
    #[test]
    fn motion_steps_over_a_zwj_emoji_as_one() {
        let wave = "👋🏻"; // simple skin-tone as a stand-in if handshake is huge
        let p = para(wave);
        assert_eq!(p.after(0, false), wave.len());
        assert_eq!(p.before(wave.len(), false), 0);
    }

    /// Arrow keys go through `move_within`, not `before`/`after` alone — pin that
    /// path so three complex emoji take three Left presses, not eight.
    #[test]
    fn move_within_steps_one_grapheme_per_arrow() {
        let fist = "👊";
        let hand = "👋🏻";
        let shake = "🤝";
        let text = format!("{fist}{hand}{shake}");
        let mut p = para(&text);
        // Start at the end.
        p.selection = Selection::from_byte_index(&p.layout, text.len(), Affinity::Upstream);
        assert_eq!(p.caret(), text.len());

        assert!(p.move_within(-1, false, false));
        assert_eq!(p.caret(), fist.len() + hand.len(), "past handshake");
        assert!(p.move_within(-1, false, false));
        assert_eq!(p.caret(), fist.len(), "past waving hand");
        assert!(p.move_within(-1, false, false));
        assert_eq!(p.caret(), 0, "past fist");
        assert!(!p.move_within(-1, false, false), "already at start");

        assert!(p.move_within(1, false, false));
        assert_eq!(p.caret(), fist.len());
        assert!(p.move_within(1, false, false));
        assert_eq!(p.caret(), fist.len() + hand.len());
        assert!(p.move_within(1, false, false));
        assert_eq!(p.caret(), text.len());
        assert!(!p.move_within(1, false, false), "already at end");
    }

    #[test]
    fn selection_of_only_emoji_is_detected() {
        let mut p = para("👋🏻🚀");
        p.selection = Selection::from_byte_index(&p.layout, 0, Affinity::Downstream).extend(
            Cursor::from_byte_index(&p.layout, p.text.len(), Affinity::Upstream),
        );
        assert!(p.selection_is_emoji_only());

        let mut mixed = para("hi👋");
        mixed.selection =
            Selection::from_byte_index(&mixed.layout, 0, Affinity::Downstream).extend(
                Cursor::from_byte_index(&mixed.layout, mixed.text.len(), Affinity::Upstream),
            );
        assert!(!mixed.selection_is_emoji_only());

        let mut plain = para("hello");
        plain.selection =
            Selection::from_byte_index(&plain.layout, 0, Affinity::Downstream).extend(
                Cursor::from_byte_index(&plain.layout, 5, Affinity::Upstream),
            );
        assert!(!plain.selection_is_emoji_only());
    }
}

/// Previous extended-grapheme boundary strictly before `at`.
fn grapheme_before(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    if at == 0 {
        return 0;
    }
    text[..at]
        .grapheme_indices(true)
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Next extended-grapheme boundary strictly after `at`.
fn grapheme_after(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    if at >= text.len() {
        return text.len();
    }
    text[at..]
        .graphemes(true)
        .next()
        .map(|g| at + g.len())
        .unwrap_or(text.len())
}

/// If `at` falls inside a grapheme, return that grapheme's start; else `at`.
fn snap_grapheme(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    if at == 0 || at == text.len() || !text.is_char_boundary(at) {
        // Not on a char boundary: walk back to one, then snap.
        let mut i = at.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        return snap_grapheme_on_boundary(text, i);
    }
    snap_grapheme_on_boundary(text, at)
}

fn snap_grapheme_on_boundary(text: &str, at: usize) -> usize {
    if at == 0 || at == text.len() {
        return at;
    }
    for (i, g) in text.grapheme_indices(true) {
        let end = i + g.len();
        if i == at {
            return at;
        }
        if i < at && at < end {
            return i;
        }
        if i > at {
            break;
        }
    }
    at
}
