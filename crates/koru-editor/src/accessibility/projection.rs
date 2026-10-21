// SPDX-License-Identifier: MPL-2.0

//! **Ordered paragraphs and runs, and what changed since the last publish.**
//!
//! One sentence, and what it excludes: *this module turns laid-out
//! [`Paragraph`]s into the text runs, positions and selection an assistive
//! technology reads, remembers exactly what it handed over, and next time hands
//! over only the difference.* The index arithmetic is
//! [`super::text_positions`]'s; the coordinate transforms are
//! [`super::geometry`]'s; the platform, identity, tree publishing and action
//! routing are somebody else's entirely. And it excludes *editing*: nothing
//! here writes to a document. It hands back byte offsets, and the caller drives
//! the same [`Paragraph`] methods a keystroke does.
//!
//! ## The cost this module exists to remove
//!
//! `parley::LayoutAccessibility::build_nodes` is **O(clusters) per row**. Plan
//! 1b.1 called it once per row per publish, which is fine for a four-paragraph
//! fixture and is O(document) per keystroke for a real one. Diffing the result
//! afterwards saves nothing, because the expensive part has already happened.
//!
//! So the projection is cached per row, keyed on
//! [`Paragraph::shaped_at`] — the stamp `rowlayout::build_row` leaves behind.
//! That key answers the only question worth asking: *is this the same layout I
//! already turned into nodes?* Text, marks, width, kind, heading and depth all
//! reach the layout through a reshape, so one comparison covers all of them,
//! and a row rebuilt from scratch by `Body::rebuild` cannot collide with the
//! row it replaced because the stamp is process-wide.
//!
//! Three outcomes, and [`Counters`] names each:
//!
//! | The row | What it costs | Counter |
//! | --- | --- | --- |
//! | was reshaped | `build_nodes`, O(clusters) | [`shapings`](Counters::shapings) |
//! | moved under the viewport | O(runs) additions | [`translated`](Counters::translated) |
//! | did not change at all | one `u64` comparison, nothing published | [`skipped`](Counters::skipped) |
//!
//! ## Off-screen paragraphs are discoverable, not published
//!
//! `rowlayout::reflow` only shapes the rows in its band; everything else keeps
//! an estimated height and stays `dirty`. Publishing an estimate as though it
//! were geometry would be publishing a rectangle that does not exist, and
//! shaping the whole document to avoid that is the reflow this feature is
//! forbidden to cause. So a row with no layout is published as a control with
//! **no text and no bounds** — [`RowText::Absent`] — which is discoverable,
//! focusable and revealable, and says nothing untrue. A focus or scroll request
//! on it goes to the view's existing scroll behaviour; the row enters the band,
//! the ordinary reflow shapes it, and the next publish carries its text.
//!
//! A row that *is* shaped but is nowhere near the viewport keeps its text and
//! loses its rectangle ([`geometry::clear_geometry`]). That is what keeps a
//! scroll's update proportional to the viewport: a run with no bounds does not
//! move when the view does, so it is not republished.
//!
//! ## Nothing is stored on a row
//!
//! `Body::rebuild` throws every [`Paragraph`] away and makes it again. A text
//! run's AccessKit id has to *survive* that, or a screen reader loses its place
//! on every keystroke, so the ids — and the cache, and the record of what was
//! last published — live in [`DocumentAccessibility`], beside the view that
//! publishes, keyed by the model node id.
//!
//! **That makes every method here take `&mut self`, and
//! `koru_workspace_host::View::semantics` takes `&self`.** The mismatch is
//! deliberate and the answer is a `RefCell` beside the view rather than a
//! widened seam: `&self` there is load-bearing because it makes
//! `Editing::reflow` and the Store *unreachable*, and neither becomes reachable
//! because an id map behind it is written to. What mutates here is
//! accessibility bookkeeping; the document, its layout and its caret are read
//! through `&Paragraph` and cannot be touched.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use accesskit::{Node, NodeId, Role, TextSelection, TreeId, TreeUpdate};
use koru_core::AttrId;
use koru_core::NodeId as KoruNodeId;
use koru_draw::geometry::Rect;
use koru_draw::pill::PillKind;
use koru_draw::theme::Brush;
use koru_i18n::Lang;
use parley::LayoutAccessibility;
use parley::layout::PositionedLayoutItem;

use super::geometry;
use super::text_positions::{
    self, RunSpan, byte_of, downstream, merge_to_graphemes, position_of, trim_strut,
};
use crate::diagram::{self, DiagramRowState};
use crate::paragraph::Paragraph;
use crate::rowlayout;

/// One inline object inside a paragraph — a Koru pill.
///
/// **The one thing in a paragraph that a text run cannot carry.**
/// `accesskit_windows` implements neither `RangeFromChild` nor
/// `ITextRangeProvider::GetChildren` (`guide/contributing/deps/ACCESSKIT.md`), so an object
/// embedded *in* a text range is invisible to a client at every layer below
/// this one. Koru's pill reaches the text as `koru_core::text::ANCHOR`, one
/// default-ignorable `U+2060` — which §5 rightly calls being silently
/// swallowed.
///
/// So the object is published **beside** the runs instead: a sibling control
/// with the pill's own kind and its own label. Nothing is invented — the label
/// is the caption the document already carries, and it is *user content*, never
/// translated and never normalised.
///
/// [`at`](Self::at) carries the anchor's byte offset so a publisher can place
/// the object in reading order where one can. Today's publishers put the
/// objects after the runs, because this adapter offers nowhere better; a reader
/// therefore meets a pill at the end of its paragraph rather than at the point
/// it sits. Late is the smaller failure than silent, and what it *sounds* like
/// is `docs/features/accessibility/README.md` Q-B2.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineObject {
    /// The attribute behind the pill. Its uuid is what an occurrence is scoped
    /// by, so two pills in one paragraph are two controls and a pill keeps its
    /// identity when the text around it is edited.
    pub attr: AttrId,
    pub kind: PillKind,
    /// The pill's caption as the document has it. **User content.**
    pub label: String,
    /// The byte offset of the anchor character in `row.text`, so a caller can
    /// place the object in reading order among the runs.
    pub at: usize,
    /// Where it is, in the window's logical coordinates. `None` when the row is
    /// published without geometry.
    pub bounds: Option<Rect>,
}

impl InlineObject {
    /// What kind of control this object is, in AccessKit's vocabulary.
    ///
    /// **Kept here rather than at each publisher**, so the shell's projection
    /// and the interactive proof cannot describe the same pill differently.
    /// Only two answers, and neither invents anything: a pill that goes
    /// somewhere is a `Link`, and a pill that only says something is a `Label`.
    /// No action is advertised, because activating one is not wired to a
    /// behaviour owner yet and an action a client can see that performs nothing
    /// is worse than one that is absent.
    pub fn role(&self) -> Role {
        match self.kind {
            PillKind::Link
            | PillKind::Document
            | PillKind::File
            | PillKind::Run
            | PillKind::Schedule
            | PillKind::SchedulePaused => Role::Link,
            PillKind::Contact | PillKind::Date => Role::Label,
        }
    }

    /// The `value` this object must *also* carry, or `None`.
    ///
    /// **Not a second label.** `accesskit_consumer::Node::label_comes_from_value`
    /// is `role() == Role::Label`, so a `Role::Label` node's UIA `Name` is read
    /// from its **value** and its `label` is ignored entirely — a pill
    /// published with a label and no value reaches a client as an unnamed
    /// control, which is the swallowing this whole mechanism exists to stop.
    /// Found by a real client reading an empty `CurrentName`, not by reading
    /// the crate; recorded in `guide/contributing/deps/ACCESSKIT.md`.
    ///
    /// The rule lives here rather than at each publisher so the shell's
    /// projection and the interactive proof cannot disagree about it.
    pub fn value(&self) -> Option<&str> {
        (self.role() == Role::Label).then_some(self.label.as_str())
    }
}

/// One paragraph's text, as AccessKit describes text.
#[derive(Clone, Debug, Default)]
pub struct ParagraphText {
    /// `Role::TextRun` nodes in reading order, with the ids their owner should
    /// publish them under.
    ///
    /// One per parley run per visual line — so a wrapped paragraph has several
    /// and a formatting change inside a line splits one. They are **not**
    /// controls: `accesskit_consumer`'s filter drops `Role::TextRun` from the
    /// platform's control view, and a client meets them only inside a text
    /// position.
    ///
    /// Bounds and character positions are in Koru's **logical** pixels, in the
    /// window's coordinate space. Whoever publishes them multiplies by the
    /// scale factor, in the one place that already does that for every other
    /// rectangle.
    pub runs: Vec<(NodeId, Node)>,
    /// Where the editor's caret or selection is, as positions into
    /// [`runs`](Self::runs).
    ///
    /// `None` when this paragraph does not hold the caret — which is not the
    /// same as a collapsed selection, that being `Some` with anchor equal to
    /// focus. A paragraph with no `text_selection` answers UIA
    /// `SupportedTextSelection_None`, which is the honest report for a
    /// paragraph the caret is not in: there is one caret and the editor owns
    /// it.
    pub selection: Option<TextSelection>,
    /// The pills in this paragraph, in reading order. See [`InlineObject`].
    pub objects: Vec<InlineObject>,
}

impl ParagraphText {
    /// How many runs were published. A counter a test can read.
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }
}

/// Where a view is looking, in the coordinates a row's `y` is measured in.
///
/// Geometry the view already has. They are taken as a value rather than
/// read off an `Editor` so this module cannot reach anything else on it — in
/// particular not `reflow`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ViewGeometry {
    /// The editor pane origin in logical window coordinates, from its draw transform.
    pub origin: (f32, f32),
    /// The left edge of the content column, in the editor pane's logical pixels.
    pub content_x: f32,
    /// How far the body is scrolled.
    pub scroll: f32,
    /// The visible height. `f32::INFINITY` means "everything is visible",
    /// which is what a whole-document test wants.
    pub viewport_h: f32,
}

/// Whether this publish is a coherent snapshot or a difference.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Publish {
    /// Everything, whether or not it changed. What a newly attached client is
    /// owed, and the only thing that may follow a client detaching.
    Whole,
    /// Only what differs from the last publish.
    Delta,
}

/// What one publish has to say about a body of ordered paragraphs.
#[derive(Clone, Debug, Default)]
pub struct BodyUpdate {
    /// The paragraphs to publish, in document order. Empty means nothing about
    /// the body changed — which is the answer a caret blink, a frame and an
    /// automation poll must all get.
    pub rows: Vec<RowUpdate>,
    /// Rows that have left the document since the last publish.
    pub removed: Vec<KoruNodeId>,
    /// The run ids those rows were last published with.
    ///
    /// **Kept because they are unreachable a moment later.** The rows are
    /// pruned from the cache at the end of [`DocumentAccessibility::body`], so
    /// this is the only chance to say which text-run ids the client is about to
    /// lose — and §5's *a screen reader must not receive a selection pointing at
    /// text-run IDs removed in the same update* is exactly a question about this
    /// list. [`super::events::coherent`] is what asks it.
    pub removed_runs: Vec<NodeId>,
    /// The container's complete ordered child list, when it differs from the
    /// last publish — an insert, a delete or a reorder. `None` means the
    /// container itself needs no republishing, which is the ordinary case for
    /// an edit inside one paragraph.
    pub order: Option<Vec<KoruNodeId>>,
}

impl BodyUpdate {
    /// Whether this update says anything at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.removed.is_empty() && self.order.is_none()
    }
}

/// **What a row is as a checklist item and as a task — two questions, and the
/// answers are allowed to differ.**
///
/// [`Paragraph::is_task`] asks whether the row *draws a checkbox*, which a
/// plain checklist item does without ever being a task; [`Paragraph::task`]
/// asks whether `koru_core::tasks::is_task` holds. `docs/features/tasks/SPEC.md` §5
/// lets the drawn box and the canonical lifecycle disagree — a task with
/// structural children has a box that is only a box — and a completed task
/// still holding unfinished work is *concurrent residue*, which is the whole
/// reason the indicator exists.
///
/// So this carries both, unmerged, and a publisher says both out loud. A
/// projection that collapsed them into one boolean would be telling a screen
/// reader something the model does not say.
///
/// A third fact rides beside them: what the row says about itself as a
/// diagram, when it is one. See [`DiagramFact`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RowFacts {
    /// `Some(ticked)` when this row draws a checkbox at all — the `done`
    /// mirror, which is the whole meaning of a checklist item and only a
    /// mirror on a task. `None` on a row with no box.
    pub checkbox: Option<bool>,
    /// The canonical lifecycle, when this row is a task. `None` on every row
    /// that is not one — a ticked checklist item included.
    pub task: Option<crate::paragraph::TaskRow>,
    /// What this row says about itself as a diagram: a `mermaid` block whose
    /// text is closed. `None` on every other row, and on a diagram whose text
    /// is open — that is the code row it is, and is read as one.
    pub diagram: Option<DiagramFact>,
}

impl RowFacts {
    /// Read straight off the row, from the predicates that already answer
    /// these questions. Nothing is derived a second time here — and nothing
    /// is *said* here either: a diagram's sentence is built only for a row a
    /// publish carries ([`RowUpdate::diagram`]).
    pub fn of(row: &Paragraph) -> Self {
        Self {
            checkbox: row.is_task().then_some(row.done),
            task: row.task,
            diagram: row
                .diagram
                .as_ref()
                .and_then(|state| DiagramFact::of(state, koru_i18n::current())),
        }
    }
}

/// **A diagram row's fact, without its sentence.**
///
/// A drawing has no text to read, so what a screen reader is told about a
/// diagram is a *name* — the narration [`diagram::narration_text`] builds
/// from the prepared scene. Building it is a walk over the scene's arrows:
/// cheap once, and not something to do for every row on every publish; and
/// comparing two of them is not something to do at all. So the fact a publish
/// compares is this — *which* text the row's state describes and *which*
/// state it is in, as one stamp. The sentence is a function of the stamp and
/// is built when the stamp moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DiagramFact {
    /// The row shows a picture in place of its text — the drawing, or the
    /// placeholder waiting for one — and is published as an image with no
    /// text. `false` for a block whose text is showing because it does not
    /// draw: that is a code row wearing *Diagram, not drawn: …* as its name.
    pub picture: bool,
    /// Everything the narration is a function of: the text the state
    /// describes, which of the three states it is in, and the language.
    pub stamp: u64,
}

impl DiagramFact {
    fn of(state: &DiagramRowState, lang: Lang) -> Option<Self> {
        if state.open {
            return None;
        }
        let mut hasher = DefaultHasher::new();
        state.shown.hash(&mut hasher);
        state.pending.hash(&mut hasher);
        state.empty.hash(&mut hasher);
        state.reason.is_some().hash(&mut hasher);
        state.drawing.is_some().hash(&mut hasher);
        lang.hash(&mut hasher);
        Some(Self {
            picture: state.shows_picture(),
            stamp: hasher.finish(),
        })
    }
}

/// One paragraph, as this publish has it.
#[derive(Clone, Debug)]
pub struct RowUpdate {
    pub id: KoruNodeId,
    /// Its place in the body, for a caller assembling reading order.
    pub index: usize,
    /// What this row is as a checklist item and as a task, which are two
    /// different questions. See [`RowFacts`].
    pub facts: RowFacts,
    /// Where it is, in the window's logical coordinates. `None` for a row that
    /// is not near the viewport: legal, and the honest answer for a rectangle
    /// that either does not exist yet or is nowhere a person can look.
    pub bounds: Option<Rect>,
    /// Where this row's checkbox answers a press, when it draws one and the
    /// row is near enough for that to be a fact. See
    /// [`geometry::checkbox_bounds`](super::geometry::checkbox_bounds): the box
    /// lives in the gutter, so it cannot be worked out from [`bounds`](Self::bounds).
    pub checkbox_bounds: Option<Rect>,
    pub text: RowText,
    /// The pills in this paragraph, in reading order. Empty for the ordinary
    /// row. They follow the *text* rather than the geometry: a paragraph off
    /// screen is still readable, so its pills are still announceable, and like
    /// its runs they carry no rectangle until the view scrolls to them.
    pub objects: Vec<InlineObject>,
    /// What a screen reader hears about this row as a diagram, already in the
    /// current language, when [`facts`](Self::facts) says it is one: the
    /// narration of the drawing, the placeholder's name, or the reason it is
    /// not drawn. A picture row's whole name; a not-drawn block's name over
    /// its text. Built here, once, for a row a publish carries.
    pub diagram: Option<String>,
}

/// What a published paragraph says about its text.
#[derive(Clone, Debug)]
pub enum RowText {
    /// No layout yet. Publish a control with no text pattern: discoverable,
    /// focusable, and revealed by the view's existing scroll behaviour.
    Absent,
    /// Publish these run nodes. The runs changed, or moved, or are new to the
    /// client.
    Runs {
        runs: Vec<(NodeId, Node)>,
        selection: Option<TextSelection>,
    },
    /// The runs are already in the client's tree and are byte-for-byte what it
    /// has. Their ids are re-stated so the paragraph node keeps its children —
    /// an AccessKit node is republished whole — and nothing else is copied.
    Retained {
        runs: Vec<NodeId>,
        selection: Option<TextSelection>,
    },
}

/// What this view has done, for somebody who wants to look.
///
/// **Counters rather than a time budget**, for the reason the roadmap gives: a
/// stopwatch passes on a fast machine and says nothing about whether the work
/// happened.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Calls to [`DocumentAccessibility::paragraph`], the single-row form.
    ///
    /// Kept because Plan 1b.1's tests read it and because a caller that still
    /// projects one row at a time wants to know how often it does.
    /// [`published`](Self::published) is the number for
    /// [`body`](DocumentAccessibility::body), and
    /// [`shapings`](Self::shapings) is the number that costs anything.
    pub paragraphs: u64,
    /// Text runs handed over. Grows with wrapping and with formatting.
    pub runs: u64,
    /// Requests naming runs this view never published — a stale range, or one
    /// belonging to a different paragraph. Always refused, never clamped.
    pub stale_requests: u64,
    /// **The number this plan is about.** `parley::build_nodes` calls: the
    /// O(clusters) work. It must not grow with the document on a keystroke, and
    /// must not grow at all on a caret move, a blink or a poll.
    pub shapings: u64,
    /// Projections answered from the cache with no shaping.
    pub reused: u64,
    /// Cached projections moved to a new origin rather than rebuilt.
    pub translated: u64,
    /// Paragraph nodes actually included in an update — the size of the delta.
    pub published: u64,
    /// Rows a publish looked at and decided not to touch at all.
    pub skipped: u64,
    /// Requests from an assistive technology that reached an editor action.
    /// See [`super::actions`].
    pub requests_performed: u64,
    /// Requests refused — a row that has gone, an action the editor does not
    /// perform, a range naming runs this view never published, or one of the
    /// two that would write. **Never clamped onto something nearby.**
    pub requests_refused: u64,
}

/// The AccessKit ids, positions and published state one document view holds
/// between publishes.
///
/// Four things need to survive `Body::rebuild`, and none of them can live on a
/// [`Paragraph`], which it discards:
///
/// - **A text run's node id.** A screen reader holds ranges across updates. If
///   a run comes back with a new id after every keystroke, every range it holds
///   is stale and it starts the paragraph again.
/// - **parley's span map**, which is what makes those ids stable.
/// - **The byte offsets of the runs as last published**, which is the only
///   honest answer to an AT that sends back a position: the tree it is pointing
///   at is the tree it was given, not the one the editor has now.
/// - **The projection itself**, so an unchanged row costs a comparison.
///
/// Keyed by Koru node id, so a row that survives a rebuild keeps its runs and a
/// row that is gone drops out on the next [`retain`](Self::retain) — or on the
/// next [`body`](Self::body), which does it for you.
#[derive(Default)]
pub struct DocumentAccessibility {
    rows: HashMap<KoruNodeId, RowAccess>,
    /// The child order as last published, so an insert or a reorder can be told
    /// from an edit inside a paragraph.
    order: Vec<KoruNodeId>,
    counters: Counters,
}

#[derive(Default)]
struct RowAccess {
    /// parley's own map from layout spans to the run ids it minted.
    access: LayoutAccessibility,
    /// How many ids this row has been given. Only ever increases, so an id is
    /// never reused for a different span after a rebuild.
    minted: u64,
    /// The runs as last published, in reading order.
    spans: Vec<RunSpan>,
    /// The projection itself, and which shaping and origin it is for.
    cache: Option<Cached>,
    /// What the client was last given for this row, so an unchanged row can be
    /// left alone. `None` until the row has been published once.
    published: Option<Published>,
}

/// A built projection, and the two things that decide whether it can be reused.
struct Cached {
    /// The [`Paragraph::shaped_at`] stamp this was built from.
    shaped: u64,
    /// Where its bounds currently sit. Always `Some`: a cached projection keeps
    /// real geometry so it can be *translated*, and geometry is only dropped on
    /// the copy handed out.
    origin: (f32, f32),
    runs: Vec<(NodeId, Node)>,
    objects: Vec<InlineObject>,
}

/// What the client has for this row, as far as this view knows.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Published {
    /// `0` for a row published as [`RowText::Absent`].
    shaped: u64,
    /// `None` for a row published without geometry.
    origin: Option<(f32, f32)>,
    height: f32,
    selection: Option<TextSelection>,
    /// **Not a property of the layout**, which is why it is here rather than
    /// left to the shaping stamp. Ticking a box or making a row a task changes
    /// what a screen reader must say about it and need not change one glyph —
    /// so a row whose facts moved is republished even when its runs did not.
    facts: RowFacts,
    /// Whether the client was given this row's runs. A diagram's picture
    /// hides a shaped text layout, so the shaping stamp alone cannot say
    /// whether the runs it stands for were ever sent — and a row that opens
    /// its text after being published as a picture must publish them rather
    /// than name them as retained.
    textual: bool,
}

impl DocumentAccessibility {
    /// Project one **laid-out** paragraph, whole.
    ///
    /// `base` is the AccessKit id of the paragraph control itself — the node an
    /// external client finds by `AutomationId` — and run ids are derived from
    /// it so two paragraphs cannot collide. `origin` is the top-left of this
    /// row's parley layout in the window's logical coordinates; build it with
    /// [`geometry::row_origin`] rather than by hand, so a caret rectangle and a
    /// text-range rectangle cannot drift apart.
    ///
    /// `focused` is whether this row holds the caret. It is the caller's
    /// answer, not a guess from the selection: every row carries a
    /// `parley::Selection` whether or not anybody is on it.
    ///
    /// **Reads a layout that already exists.** No shaping, no reflow, no I/O,
    /// no `&mut Paragraph` — so this cannot be the thing that triggers a
    /// relayout from a platform callback. A second call for the same row at the
    /// same shaping and origin costs a clone rather than a `build_nodes`; see
    /// [`body`](Self::body) for the form that costs nothing at all.
    pub fn paragraph(
        &mut self,
        row: &Paragraph,
        base: NodeId,
        origin: (f32, f32),
        focused: bool,
    ) -> ParagraphText {
        self.project(row, base, origin);
        let entry = self.rows.entry(row.id).or_default();
        let runs = entry
            .cache
            .as_ref()
            .map(|cached| cached.runs.clone())
            .unwrap_or_default();
        let objects = entry
            .cache
            .as_ref()
            .map(|cached| cached.objects.clone())
            .unwrap_or_default();
        let selection =
            (focused && !entry.spans.is_empty()).then(|| selection_of(row, &entry.spans));
        self.counters.paragraphs += 1;
        self.counters.runs += runs.len() as u64;
        ParagraphText {
            runs,
            selection,
            objects,
        }
    }

    /// Everything one publish has to say about an ordered body of paragraphs.
    ///
    /// **The incremental form, and the one a real document uses.** Every row is
    /// looked at — a `u64` and two `f32` comparisons — and only the rows whose
    /// published state actually differs are projected, copied or returned.
    /// `base_of` turns a model node id into the AccessKit id of that
    /// paragraph's control, which is the host's business and not this crate's.
    ///
    /// `focus` is the index of the row holding the caret, which the body
    /// already knows. `Publish::Whole` ignores what was published before, which
    /// is what a newly attached client is owed and the only correct answer
    /// after one detached.
    pub fn body(
        &mut self,
        rows: &[Paragraph],
        view: ViewGeometry,
        focus: Option<usize>,
        publish: Publish,
        base_of: impl Fn(KoruNodeId) -> NodeId,
    ) -> BodyUpdate {
        let live: Vec<KoruNodeId> = rows.iter().map(|row| row.id).collect();
        let known: HashSet<KoruNodeId> = live.iter().copied().collect();
        let removed: Vec<KoruNodeId> = self
            .order
            .iter()
            .copied()
            .filter(|id| !known.contains(id))
            .collect();
        // Read now, because `self.rows.retain` at the end of this function is
        // what makes them unreachable. See [`BodyUpdate::removed_runs`].
        let removed_runs: Vec<NodeId> = removed
            .iter()
            .flat_map(|id| self.published_run_ids(*id))
            .collect();
        let order = (publish == Publish::Whole || self.order != live).then(|| live.clone());
        // The language a diagram's name is built in: read once per publish,
        // and folded into the fact a row is compared by, so a name is rebuilt
        // when the language changes and not otherwise.
        let lang = koru_i18n::current();

        let mut updates = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            // A row that has never been shaped — outside `reflow`'s band, or
            // never reached by one — has an *estimated* height and no layout.
            // Publishing that geometry would be publishing a rectangle that
            // does not exist; shaping it here would be the reflow this feature
            // may not cause.
            let shaped = if row.dirty { 0 } else { row.shaped_at() };
            let origin = (shaped != 0
                && geometry::near_viewport(row, view.scroll, view.viewport_h))
            .then(|| {
                let local = geometry::row_origin(view.content_x, view.scroll, row);
                (local.0 + view.origin.0, local.1 + view.origin.1)
            });
            let focused = focus == Some(index);
            let height = if origin.is_some() { row.height } else { 0.0 };
            let facts = RowFacts::of(row);
            // Whether this row has text to publish at all: shaped, and not an
            // image, a divider or a diagram showing its picture.
            let textual = shaped != 0 && has_text_layout(row);
            let was = self.rows.get(&row.id).and_then(|entry| entry.published);

            // **The cheap way out, taken before anything is touched.** A row
            // nobody is on, at the same shaping and the same place, is three
            // comparisons and no hash-map entry, no clone and no projection.
            // This is the branch a thousand-paragraph document takes on every
            // keystroke for every paragraph but one.
            let unchanged = Published {
                shaped,
                origin,
                height,
                selection: None,
                facts,
                textual,
            };
            if publish == Publish::Delta && !focused && was == Some(unchanged) {
                self.counters.skipped += 1;
                continue;
            }

            if shaped != 0 {
                self.project(row, base_of(row.id), origin.unwrap_or((0.0, 0.0)));
            }
            let entry = self.rows.entry(row.id).or_default();
            let selection = (focused && shaped != 0 && !entry.spans.is_empty())
                .then(|| selection_of(row, &entry.spans));
            let want = Published {
                shaped,
                origin,
                height,
                selection,
                facts,
                textual,
            };
            if publish == Publish::Delta && was == Some(want) {
                self.counters.skipped += 1;
                continue;
            }
            // A client holds a row's runs only if it was given them: a row
            // last published as a diagram's picture has none to retain,
            // whatever its shaping stamp says.
            let republish_runs = publish == Publish::Whole
                || match was {
                    None => true,
                    Some(was) => {
                        was.shaped != want.shaped || was.origin != want.origin || !was.textual
                    }
                };
            let text = if !textual {
                RowText::Absent
            } else if republish_runs {
                let mut runs = entry
                    .cache
                    .as_ref()
                    .map(|cached| cached.runs.clone())
                    .unwrap_or_default();
                if origin.is_none() {
                    geometry::clear_geometry(&mut runs);
                }
                self.counters.runs += runs.len() as u64;
                RowText::Runs { runs, selection }
            } else {
                RowText::Retained {
                    runs: entry.spans.iter().map(|span| span.id).collect(),
                    selection,
                }
            };
            let bounds = origin.map(|origin| {
                let runs = entry
                    .cache
                    .as_ref()
                    .map(|cached| cached.runs.as_slice())
                    .unwrap_or_default();
                geometry::row_bounds(runs, origin, row.height)
            });
            // The inline objects follow the text, not the geometry: a
            // paragraph off screen is still readable, so its pills are still
            // announceable, and — like its runs — they say nothing about where
            // they are until the view scrolls to them.
            let mut objects = entry
                .cache
                .as_ref()
                .map(|cached| cached.objects.clone())
                .unwrap_or_default();
            if origin.is_none() {
                for object in &mut objects {
                    object.bounds = None;
                }
            }
            entry.published = Some(want);
            self.counters.published += 1;
            updates.push(RowUpdate {
                id: row.id,
                index,
                facts,
                bounds,
                // The same gate the row's own rectangle passes: a box on a row
                // that is nowhere near the viewport is not a place yet.
                checkbox_bounds: origin
                    .and_then(|_| geometry::checkbox_bounds(view.content_x, view.scroll, row))
                    .map(|bounds| geometry::shift(bounds, view.origin.0, view.origin.1)),
                text,
                objects,
                // Built here, for a row this publish carries, and nowhere per
                // frame: a drawn diagram's name is a walk over its arrows.
                diagram: row
                    .diagram
                    .as_ref()
                    .and_then(|state| diagram::narration_text(lang, state)),
            });
        }

        self.order = live;
        self.rows.retain(|id, _| known.contains(id));
        BodyUpdate {
            rows: updates,
            removed,
            removed_runs,
            order,
        }
    }

    /// Build, move or reuse this row's runs, leaving them in the cache.
    ///
    /// The one place `parley::build_nodes` is called, and the one place
    /// [`Counters::shapings`] grows.
    fn project(&mut self, row: &Paragraph, base: NodeId, origin: (f32, f32)) {
        let entry = self.rows.entry(row.id).or_default();
        let stamp = row.shaped_at();
        // Images and dividers receive a geometry stamp but build_row never
        // shapes their text. A default or previous layout has unrelated byte
        // ranges: passing it with shaped_text can panic inside Parley on U+200B.
        // A diagram showing its picture has a shaped text nobody is shown;
        // its runs are dropped too, and rebuilt when the text opens.
        if !has_text_layout(row) {
            entry.spans.clear();
            entry.cache = None;
            entry.access = LayoutAccessibility::default();
            return;
        }
        if let Some(cached) = entry.cache.as_mut() {
            // A stamp of zero means the row has never been shaped, and two
            // never-shaped rows are not the same layout — so it can never
            // satisfy the cache.
            if cached.shaped == stamp && stamp != 0 {
                if cached.origin != origin {
                    let (dx, dy) = (origin.0 - cached.origin.0, origin.1 - cached.origin.1);
                    geometry::translate(&mut cached.runs, dx, dy);
                    for object in &mut cached.objects {
                        object.bounds = object.bounds.map(|rect| geometry::shift(rect, dx, dy));
                    }
                    cached.origin = origin;
                    self.counters.translated += 1;
                } else {
                    self.counters.reused += 1;
                }
                return;
            }
        }

        let RowAccess {
            access,
            minted,
            spans,
            cache,
            published: _,
        } = entry;

        let shaped_text = rowlayout::shaped_text(row);
        let mut update = TreeUpdate {
            nodes: Vec::new(),
            tree: None,
            tree_id: TreeId::ROOT,
            focus: base,
        };
        // A throwaway parent: `build_nodes` pushes each run's id onto a node it
        // is given, and this module's caller owns the real one. The order it
        // pushes them in is reading order, and `update.nodes` already carries
        // that.
        let mut parent = Node::new(Role::MultilineTextInput);
        let base_bits = base.0;
        let mut next = || {
            *minted += 1;
            NodeId(run_id(base_bits, *minted))
        };
        access.build_nodes(
            &shaped_text,
            &row.layout,
            &mut update,
            &mut parent,
            &mut next,
            f64::from(origin.0),
            f64::from(origin.1),
            // Colour, highlight and decoration are formatting, and Koru sets no
            // brush properties on a run. Setting nothing is the honest answer:
            // a client asking `GetAttributeValue` is told "not supported"
            // rather than a colour Koru invented.
            |_node: &mut Node, _style: &parley::Style<Brush>| {},
        );

        let mut runs = update.nodes;
        if let Some((id, last)) = runs.last_mut()
            && trim_strut(last)
        {
            let was = *id;
            let now = NodeId(run_id(base_bits, STRUT_RUN));
            *id = now;
            // **A run id is not only an id.** parley links the runs of one
            // visual line with `next_on_line`/`previous_on_line`, and
            // `accesskit_consumer::text` follows them with an `unwrap()` — so a
            // link naming an id nothing publishes is not a wrong answer, it is
            // an abort inside the platform's own thread, arriving as a
            // `STATUS_STACK_BUFFER_OVERRUN` with no diagnostic. Renaming a run
            // means renaming every reference to it.
            relink(&mut runs, was, now);
        }
        for (_, node) in &mut runs {
            merge_to_graphemes(node);
        }

        let (built, covered) = text_positions::spans_of(&runs);
        *spans = built;
        debug_assert_eq!(
            covered,
            row.text.len(),
            "the published runs do not cover the row's text exactly"
        );
        let objects = inline_objects(row, origin);
        *cache = Some(Cached {
            shaped: stamp,
            origin,
            runs,
            objects,
        });
        self.counters.shapings += 1;
    }

    /// The byte offsets an assistive technology asked for, in this row's own
    /// text.
    ///
    /// The inverse of [`ParagraphText::selection`], and the whole of what a
    /// `SetSelection` request means: `(anchor, focus)` as UTF-8 byte offsets
    /// into `row.text`, snapped to grapheme boundaries and clamped inside it.
    ///
    /// Returns `None` when the request names runs this paragraph never
    /// published — a stale range, or one belonging to a different paragraph.
    /// **A refusal rather than a guess**: clamping an unknown range onto
    /// whatever is nearest is how an AT ends up replacing text the person never
    /// selected.
    pub fn byte_range(
        &mut self,
        row: &Paragraph,
        request: &TextSelection,
    ) -> Option<(usize, usize)> {
        let Some(spans) = self.rows.get(&row.id).map(|entry| &entry.spans) else {
            self.counters.stale_requests += 1;
            return None;
        };
        let (Some(anchor), Some(focus)) = (
            byte_of(spans, &request.anchor),
            byte_of(spans, &request.focus),
        ) else {
            self.counters.stale_requests += 1;
            return None;
        };
        let snap = |at: usize| Paragraph::snap_caret(&row.text, at.min(row.text.len()));
        Some((snap(anchor), snap(focus)))
    }

    /// Record what one assistive-technology request came to.
    ///
    /// Called by [`super::actions::perform`] and by nothing else — the counters
    /// live here because this is where every other number about this view lives,
    /// and a caller that had to remember to increment one would forget in the
    /// path that mattered.
    pub(super) fn note_request(&mut self, performed: bool) {
        if performed {
            self.counters.requests_performed += 1;
        } else {
            self.counters.requests_refused += 1;
        }
    }

    /// Forget every row that is no longer in the document.
    ///
    /// [`body`](Self::body) does this for you. It stays public for the
    /// single-row [`paragraph`](Self::paragraph) flow, where nobody else knows
    /// the full row list. Without it the map grows for the life of the view —
    /// one entry per paragraph ever shown — which is a leak in a long session
    /// rather than a correctness bug, and the kind that only shows up on
    /// somebody else's machine.
    pub fn retain(&mut self, live: impl IntoIterator<Item = KoruNodeId>) {
        let live: HashSet<KoruNodeId> = live.into_iter().collect();
        self.rows.retain(|id, _| live.contains(id));
    }

    /// Forget what the client has, keeping the ids and the cache.
    ///
    /// What a client detaching means: the next attachment is owed a coherent
    /// full snapshot of what is on screen *then*, and an incremental update
    /// against a tree nobody holds any more would be a tree of orphans. The
    /// projections themselves are still good — the layout did not change
    /// because somebody stopped listening — so they are kept and the next
    /// snapshot costs no shaping.
    pub fn forget_published(&mut self) {
        for entry in self.rows.values_mut() {
            entry.published = None;
        }
        self.order.clear();
    }

    /// How many rows this view is holding ids for. A counter for a test, not a
    /// number anything branches on.
    pub fn tracked_rows(&self) -> usize {
        self.rows.len()
    }

    /// What this view has projected so far. See [`Counters`].
    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// The rows this view last published, in document order.
    ///
    /// For a caller that has to say what it is about to stop showing — a tab
    /// switching away takes its paragraphs' occurrences out of the map an action
    /// is resolved against, and this is the only record of what they were.
    pub fn published_order(&self) -> &[KoruNodeId] {
        &self.order
    }

    /// The run ids this row was last published with, in reading order.
    ///
    /// For a diagnostic dump and for a test that has to name a run — a client
    /// never sees these as controls, so there is no other way to refer to one.
    pub fn published_run_ids(&self, row: KoruNodeId) -> Vec<NodeId> {
        self.rows
            .get(&row)
            .map(|entry| entry.spans.iter().map(|span| span.id).collect())
            .unwrap_or_default()
    }
}

/// The pills in this row, in reading order, with the geometry the layout
/// already has.
///
/// Reads `row.runs` for the anchors — a `TextRun` carrying `pill` is exactly
/// where one is — and the positioned inline boxes for the rectangles. No
/// `PillCache` and no measuring: the box was measured when the row was shaped,
/// and asking again would be the second layout pass §5 forbids.
fn inline_objects(row: &Paragraph, origin: (f32, f32)) -> Vec<InlineObject> {
    let anchors: Vec<(AttrId, usize)> = row
        .runs
        .iter()
        .filter_map(|run| run.pill.map(|attr| (attr, run.start.get())))
        .collect();
    if anchors.is_empty() {
        return Vec::new();
    }
    let mut boxes: HashMap<u64, (f32, f32, f32, f32)> = HashMap::new();
    for line in row.layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::InlineBox(placed) = item {
                boxes.insert(placed.id, (placed.x, placed.y, placed.width, placed.height));
            }
        }
    }
    let mut out: Vec<InlineObject> = anchors
        .into_iter()
        .filter_map(|(attr, at)| {
            let info = row.pills.get(&attr)?;
            let bounds = boxes
                .get(&rowlayout::attr_box_id(attr))
                .map(|(x, y, w, h)| geometry::box_bounds(*x, *y, *w, *h, origin));
            Some(InlineObject {
                attr,
                kind: info.kind,
                label: info.label.clone(),
                at,
                bounds,
            })
        })
        .collect();
    out.sort_by_key(|object| object.at);
    out
}

/// Whether this row's layout is its text, shaped — rather than the geometry
/// stamp an image, a divider or a diagram's picture carries.
///
/// A diagram showing its picture has its code text shaped underneath, but a
/// person is not shown it and neither is a screen reader: the row is
/// published as a picture named with its narration, with no runs, and the
/// text comes back — with its runs — when the text opens.
fn has_text_layout(row: &Paragraph) -> bool {
    row.image.is_none()
        && row.kind != koru_core::node::Kind::Divider
        && !row
            .diagram
            .as_ref()
            .is_some_and(DiagramRowState::shows_picture)
}

/// The editor's caret or selection, as positions into this row's published
/// runs.
fn selection_of(row: &Paragraph, spans: &[RunSpan]) -> TextSelection {
    let anchor = row.selection.anchor();
    let focus = row.selection.focus();
    TextSelection {
        anchor: position_of(
            spans,
            anchor.index().min(row.text.len()),
            downstream(anchor.affinity()),
        ),
        focus: position_of(spans, row.caret(), downstream(focus.affinity())),
    }
}

/// The ordinal [`run_id`] gives the emptied layout-strut run.
///
/// **parley's run ids are stable, but not the way it sounds.** A run's id is
/// remembered against its `ClusterPath` — `(line index, run index, cluster
/// index within the line)` — so an id survives an edit only while the span's
/// *first cluster* stays where it was. That is true of the run at the start of
/// a line and false of everything after an insertion point, and the strut is
/// always last: type one character and the strut's cluster index moves, so
/// parley mints it a new id. Verified by experiment, and recorded in
/// `guide/contributing/deps/ACCESSKIT.md`.
///
/// The strut run is Koru's own artifact, it carries no characters after
/// [`trim_strut`], and it is what an end-of-text caret position names — so a
/// screen reader holding a range at the end of a paragraph would find it stale
/// after every keystroke. Giving it an id derived from the paragraph instead of
/// from a cluster path fixes that at the one place the two disagree.
///
/// `u64::MAX` rather than `0`, because `run_id(base, 0)` is `base` itself,
/// which is the paragraph control's own node id.
const STRUT_RUN: u64 = u64::MAX;

/// Point every same-line link at a run that has been given a new id.
///
/// See the call site: `accesskit_consumer` resolves `next_on_line` and
/// `previous_on_line` with an `unwrap()`, so a dangling one aborts the process
/// from inside a UI Automation callback.
fn relink(runs: &mut [(NodeId, Node)], was: NodeId, now: NodeId) {
    for (_, node) in runs {
        if node.next_on_line() == Some(was) {
            node.set_next_on_line(now);
        }
        if node.previous_on_line() == Some(was) {
            node.set_previous_on_line(now);
        }
    }
}

/// A run's node id, from the paragraph's own id and how many runs it has been
/// given.
///
/// Derived rather than taken from a global counter, so one paragraph's run ids
/// cannot depend on how many *other* paragraphs were published first — which
/// would make them differ between a fresh window and a scrolled one, and make a
/// log unreadable. The multiplier is the 64-bit golden ratio, the constant
/// `SplitMix64` uses; it only has to spread, because the consequence of a
/// collision is two nodes with one id inside a single window's tree.
pub(super) fn run_id(base: u64, nth: u64) -> u64 {
    let mixed = base ^ nth.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    // Zero is `accesskit_windows`' placeholder root and must never be
    // published — the same rule `Occurrence::node_id` follows.
    if mixed == 0 { 1 } else { mixed }
}

#[cfg(test)]
mod tests;
