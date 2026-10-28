// SPDX-License-Identifier: MPL-2.0

//! A `mermaid` code block drawn in place of its text.
//!
//! **The editor asks; it never computes.** Laying a diagram out means parsing
//! the text and measuring every label, and `Editing::reflow` runs at the start
//! of every frame, so nothing here may wait on an engine. The editor keeps,
//! per view, a [`DiagramEntry`] for every diagram row; on each reflow it hashes
//! the row's text, looks the entry up, and when the entry does not describe
//! that text it pushes a [`DiagramRequest`] and marks the hash as in flight.
//! The host drains the requests ([`Editing::take_diagram_requests`]), lays
//! the text out on a worker it owns, and hands back a prepared drawing or the
//! [`Status`] that says why there is none ([`Editing::diagram_result`]). The
//! cost on the loop thread is one hash and one lookup per diagram row.
//!
//! **The state lives on the view, not on the row.** `Paragraph::adopt` throws
//! a row's derived caches away on every keystroke, and the one thing this
//! feature promises while a person types is that the drawing of the last text
//! that worked stays on screen until a newer one lands. So the entries are in
//! [`ViewState::diagrams`], keyed by node, and [`sync`] mirrors what the
//! current frame needs into [`Paragraph::diagram`] before the rows are
//! positioned — the same shape `ViewState::table_source` takes on its way to
//! `Paragraph::table_source_visible`.
//!
//! The invariant every rule below serves: **the drawing shown is always the
//! drawing of text that parsed, and the block says which text that is.**
//!
//! - Text **closed** (the usual state): `Drawn` shows the drawing, `Pending`
//!   the tinted panel at the text's height, `Failed` the code text with the
//!   reason beneath — never a stale drawing.
//! - Text **open** (Enter on the drawing, or *Edit text*): the code text, the
//!   reason if the current text does not draw, and below it the current
//!   drawing — or the last one that worked, labelled as such.
//! - Whether the text is open is a reading position, per view, never synced.
//!
//! [`Editing::take_diagram_requests`]: crate::Editing::take_diagram_requests
//! [`Editing::diagram_result`]: crate::Editing::diagram_result
//! [`ViewState::diagrams`]: crate::ViewState::diagrams

use std::collections::HashMap;
use std::collections::hash_map::{DefaultHasher, Entry};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use koru_core::NodeId;
use koru_core::node::Kind;
use koru_diagram::{Counts, DiagramScene, Kind as DiagramKind, NarrationItem, SceneBody};
use koru_draw::diagram::PreparedDiagram;
use koru_i18n::{Key, Lang, fill, number, t};

/// The status a host answers with, re-exported so a view above this crate can
/// name it without linking the engine's crate itself.
pub use koru_diagram::Status;

use crate::body::Body;
use crate::paragraph::Paragraph;
use crate::view_state::ViewState;

#[cfg(test)]
pub(crate) mod tests;

/// The language token that makes a code block a diagram. Exact: `Mermaid` or
/// `mermaid-js` are ordinary code blocks.
pub const LANGUAGE: &str = "mermaid";

/// The one hash the editor and the host key a text on.
///
/// A std `DefaultHasher` over the text — stable for the life of the process,
/// which is all a request/result pair needs, and cheap enough to take on
/// every reflow for every diagram row. Not a content address: nothing stores
/// it.
pub fn text_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// What the editor asks the host to lay out: one text, for one node.
///
/// `text_hash` is [`text_hash`] of `text`, carried so the host can key its
/// cache and so the answer can name the text it is for without sending it
/// back. The text travels as an `Arc<str>` because a worker needs to own it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagramRequest {
    pub node: NodeId,
    pub text_hash: u64,
    pub text: Arc<str>,
}

/// What a view knows about one diagram's current text.
#[derive(Clone)]
pub enum DiagramState {
    /// Asked, not answered: the row shows the tinted panel at its text's
    /// height.
    Pending,
    /// The drawing of the text whose hash is the entry's `shown_hash`.
    Drawn(Arc<PreparedDiagram>),
    /// Why that text is not drawn. Never holds [`Status::Draws`]: a text that
    /// draws arrives as [`Drawn`](Self::Drawn).
    Failed(Status),
}

impl std::fmt::Debug for DiagramState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Drawn(prepared) => write!(f, "Drawn({:?})", prepared.natural_size()),
            Self::Failed(status) => write!(f, "Failed({status:?})"),
        }
    }
}

/// One view's record of one diagram row.
#[derive(Clone)]
pub struct DiagramEntry {
    /// The hash of the text `state` describes. For `Pending`, the text the
    /// placeholder stands for.
    pub shown_hash: u64,
    pub state: DiagramState,
    /// The last drawing that worked, this session, in this view — kept across
    /// a `Failed` so the open text can keep showing it, labelled.
    pub last_good: Option<Arc<PreparedDiagram>>,
    /// The hash of the text a request is in flight for, or `None`. Always
    /// either `None` or the hash of the row's current text: [`sync`] drops a
    /// request for text that is no longer there, and the answer to it is then
    /// ignored on arrival.
    pub requested: Option<u64>,
}

// A prepared drawing prints as its size: the layouts inside it are not
// something a test failure wants to read.
impl std::fmt::Debug for DiagramEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagramEntry")
            .field("shown_hash", &self.shown_hash)
            .field("state", &self.state)
            .field(
                "last_good",
                &self.last_good.as_ref().map(|p| p.natural_size()),
            )
            .field("requested", &self.requested)
            .finish()
    }
}

impl DiagramEntry {
    /// A fresh entry for a text that has just been asked for.
    fn asked(hash: u64) -> Self {
        Self {
            shown_hash: hash,
            state: DiagramState::Pending,
            last_good: None,
            requested: Some(hash),
        }
    }
}

/// The controls a drawn diagram wears at its panel's top-right corner, in
/// the order they read from left to right: *Open* (the magnifier), *Copy
/// image*, *Save as…* and *Edit text* (the pencil). One control, one
/// gesture each; the pencil keeps the corner it always had, and the others
/// join beside it. `rowlayout::diagram_control_rect` places them, the
/// painter draws them from the same rects, and `hover_at` and `mouse_down`
/// ask the same rects — so a control is clicked exactly where it is seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Open,
    Copy,
    Save,
    Edit,
}

impl Control {
    /// Every control, left to right.
    pub const ALL: [Self; 4] = [Self::Open, Self::Copy, Self::Save, Self::Edit];

    /// How many controls sit between this one and the panel's corner: the
    /// pencil is at the corner, the magnifier three slots in.
    pub fn slots_from_corner(self) -> usize {
        match self {
            Self::Edit => 0,
            Self::Save => 1,
            Self::Copy => 2,
            Self::Open => 3,
        }
    }
}

/// Whether `row` is a diagram row: a code block whose language is exactly
/// [`LANGUAGE`]. Everything else about it is the code block it already is.
pub fn is_diagram_row(row: &Paragraph) -> bool {
    row.kind == Kind::Code && row.language.as_deref() == Some(LANGUAGE)
}

/// What one frame needs to know about a diagram row, mirrored from the view's
/// entry into the row before the rows are positioned.
///
/// A projection of [`DiagramEntry`] through *is the text open in this view*:
/// `position_rows` reads it for the row's height and `draw_document` for what
/// to paint, so the two cannot disagree about which of the states the block
/// is in. `Paragraph::adopt` leaves it alone; [`sync`] rewrites it.
#[derive(Clone)]
pub struct DiagramRowState {
    /// The text is open for editing in this view.
    pub open: bool,
    /// Asked, not yet answered.
    pub pending: bool,
    /// The text is empty or whitespace: the hint takes the place of the text,
    /// and no reason line is drawn.
    pub empty: bool,
    /// Why the current text is not drawn, when it is not.
    pub reason: Option<Status>,
    /// The drawing to paint: the current text's, or — only while the text is
    /// open — the last one that worked.
    pub drawing: Option<Arc<PreparedDiagram>>,
    /// `drawing` is the last good one, not the current text's: it wears the
    /// label that says so.
    pub last_good: bool,
    /// The hash of the text this state describes — the entry's
    /// [`shown_hash`](DiagramEntry::shown_hash) — so a reader of the row can
    /// tell *which* text's drawing or reason this is without looking inside
    /// either. What the accessibility projection keys a diagram's name on.
    pub shown: u64,
}

impl std::fmt::Debug for DiagramRowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiagramRowState")
            .field("open", &self.open)
            .field("pending", &self.pending)
            .field("empty", &self.empty)
            .field("reason", &self.reason)
            .field("drawing", &self.drawing.as_ref().map(|p| p.natural_size()))
            .field("last_good", &self.last_good)
            .field("shown", &self.shown)
            .finish()
    }
}

impl DiagramRowState {
    fn of(entry: &DiagramEntry, open: bool) -> Self {
        let (pending, reason, current) = match &entry.state {
            DiagramState::Pending => (true, None, None),
            DiagramState::Drawn(prepared) => (false, None, Some(prepared.clone())),
            DiagramState::Failed(status) => (false, Some(status.clone()), None),
        };
        let empty = matches!(reason, Some(Status::Empty));
        // The hint stands in for the text of an empty block; a reason line
        // under it would say the same thing twice.
        let reason = if empty { None } else { reason };
        // The last good drawing is only ever shown under the open text, where
        // the label can say what it is. Closed, a block that does not draw
        // shows its text and the reason, never a drawing of other text. And an
        // emptied block shows nothing: there is no text for a drawing to be of.
        let (drawing, last_good) = match current {
            Some(drawing) => (Some(drawing), false),
            None if open && !empty && entry.last_good.is_some() => (entry.last_good.clone(), true),
            None => (None, false),
        };
        Self {
            open,
            pending,
            empty,
            reason,
            drawing,
            last_good,
            shown: entry.shown_hash,
        }
    }

    /// The row is a picture — the drawing, or the placeholder waiting for one
    /// — rather than text: closed, and nothing to say about why it is not
    /// drawn. Selected like an image, opened with Enter, no caret.
    pub fn shows_picture(&self) -> bool {
        !self.open && self.reason.is_none() && !self.empty
    }
}

/// Mirror the view's diagram state into the rows and ask for what is missing.
///
/// Called by `Editing::reflow` before the rows are positioned, every frame.
/// Per diagram row: one hash, one lookup, and a request when the entry does
/// not describe the current text and none is in flight for it. The only
/// answer given here and now is for empty text, which needs no engine to be
/// called `Empty`. Entries whose node has left the body are dropped; the open
/// text closes when its row no longer holds the focus (a click elsewhere, a
/// deleted block).
pub(crate) fn sync(body: &mut Body, view: &mut ViewState) {
    // The startup table's one row about diagrams, with the counts as its
    // note. Nothing when the switch is off, and nothing for a document with
    // no diagram, which is most of them.
    let span = koru_core::timing::Span::start("diagrams");
    if let Some(open) = view.diagram_text_open
        && body.focused().map(|row| row.id) != Some(open)
    {
        view.diagram_text_open = None;
    }
    // The common document has no diagram: nothing below allocates for it.
    let mut seen: Vec<NodeId> = Vec::new();
    for row in &mut body.rows {
        if !is_diagram_row(row) {
            row.diagram = None;
            continue;
        }
        let hash = text_hash(&row.text);
        let open = view.diagram_text_open == Some(row.id);
        let empty = row.text.trim().is_empty();
        let (entry, fresh) = match view.diagrams.entry(row.id) {
            Entry::Occupied(entry) => (entry.into_mut(), false),
            Entry::Vacant(slot) => (slot.insert(DiagramEntry::asked(hash)), true),
        };
        if empty {
            // The one synchronous answer: nothing to lay out, nothing to ask.
            if entry.shown_hash != hash
                || !matches!(entry.state, DiagramState::Failed(Status::Empty))
            {
                entry.state = DiagramState::Failed(Status::Empty);
                entry.shown_hash = hash;
            }
            entry.requested = None;
        } else if fresh {
            view.diagram_requests.push(DiagramRequest {
                node: row.id,
                text_hash: hash,
                text: Arc::from(row.text.as_str()),
            });
        } else if entry.shown_hash == hash && !matches!(entry.state, DiagramState::Pending) {
            // The state already describes this text. A job still running for
            // some other text is no longer wanted; its answer is dropped by
            // the hash check when it lands.
            entry.requested = None;
        } else if entry.requested != Some(hash) {
            // A new text, or a placeholder whose request was superseded and
            // whose text has come back: ask, and keep showing what is shown
            // until the answer lands (no flicker while typing).
            view.diagram_requests.push(DiagramRequest {
                node: row.id,
                text_hash: hash,
                text: Arc::from(row.text.as_str()),
            });
            entry.requested = Some(hash);
        }
        row.diagram = Some(DiagramRowState::of(entry, open));
        seen.push(row.id);
    }
    if view.diagrams.len() != seen.len() {
        view.diagrams.retain(|id, _| seen.contains(id));
    }
    if !seen.is_empty() {
        span.stop_with(|| startup_note(&view.diagrams));
    }
}

/// The row `KORU_STARTUP_STATS` prints for a view's diagrams: how many rows,
/// and how many of them are waiting, drawn and not drawn. **The host's cache
/// hits are not here yet**: when the host reports how many answers came from
/// its session cache rather than a layout, that number joins this note.
pub(crate) fn startup_note(entries: &HashMap<NodeId, DiagramEntry>) -> String {
    let (mut pending, mut drawn, mut failed) = (0usize, 0usize, 0usize);
    for entry in entries.values() {
        match entry.state {
            DiagramState::Pending => pending += 1,
            DiagramState::Drawn(_) => drawn += 1,
            DiagramState::Failed(_) => failed += 1,
        }
    }
    format!(
        "{} rows, {pending} pending, {drawn} drawn, {failed} failed",
        entries.len()
    )
}

/// Land the host's answer for `node`'s text `text_hash` on the view's entry.
///
/// Ignored — `false` — unless that hash is the one in flight: an answer for
/// text the row no longer holds would put a drawing of other text on screen.
/// `Ok` becomes `Drawn` and the new last-good; `Err(status)` becomes `Failed`
/// and keeps the last-good. `true` means the row's height or picture changed
/// and a redraw is owed; the next frame's reflow re-positions the rows from
/// the entry, and no text is reshaped.
pub(crate) fn land(
    view: &mut ViewState,
    node: NodeId,
    text_hash: u64,
    result: Result<Arc<PreparedDiagram>, Status>,
) -> bool {
    let Some(entry) = view.diagrams.get_mut(&node) else {
        return false;
    };
    if entry.requested != Some(text_hash) {
        return false;
    }
    match result {
        Ok(prepared) => {
            entry.last_good = Some(prepared.clone());
            entry.state = DiagramState::Drawn(prepared);
        }
        Err(Status::Draws(_)) => {
            // The host's contract is a drawing for a text that draws. With
            // none to show, the placeholder stays and the request stays
            // marked in flight, so a broken host cannot make the editor ask
            // for the same text on every frame.
            debug_assert!(false, "a text that draws arrives as a drawing");
            return false;
        }
        Err(status) => entry.state = DiagramState::Failed(status),
    }
    entry.shown_hash = text_hash;
    entry.requested = None;
    true
}

/// The language chip's label: *Mermaid* for the diagram token, the raw token
/// for every other grammar, *Auto* for none. One function for the chip's
/// measure in `rowlayout::build_row` and its paint in `draw_document`, so the
/// hit-rect and the drawn label cannot be measured from different words.
pub fn chip_label(language: Option<&str>) -> &str {
    match language {
        Some(LANGUAGE) => koru_i18n::t(koru_i18n::current(), Key::DiagramChipMermaid),
        Some(token) => token,
        None => "Auto",
    }
}

/// The text a new diagram starts with: three boxes, left to right, drawn the
/// moment `/diagram` lands. Three lines, the smallest text that shows both
/// halves of the notation — a box and an arrow — and replaced in one
/// keystroke, since it arrives selected. The labels are `lang`'s; the
/// Mermaid around them is written here, so a translation can never be what
/// stops it drawing.
pub fn example(lang: Lang) -> String {
    format!(
        "flowchart LR\n    A[{}] --> B[{}]\n    B --> C[{}]",
        t(lang, Key::DiagramExampleStart),
        t(lang, Key::DiagramExampleNext),
        t(lang, Key::DiagramExampleDone),
    )
}

/// The line a block says when its text is not drawn, in Koru's words.
///
/// `None` for a text that draws. The engine's English detail is not here and
/// never on the page; the line names what a person can act on: the line to
/// fix, the kind Koru does not draw, how many boxes or messages were too
/// many — or, when the engine refused the text before counting anything,
/// that it is too large and nothing more.
pub fn reason_text(lang: Lang, status: &Status) -> Option<String> {
    let text = match status {
        Status::Draws(_) => return None,
        Status::NotParsed { line, .. } => koru_i18n::fill(
            koru_i18n::t(lang, Key::DiagramLineUnreadable),
            &[&koru_i18n::number::decimal(lang, u64::from(*line))],
        ),
        Status::KindNotDrawn { .. } => koru_i18n::t(lang, Key::DiagramKindNotDrawn).to_owned(),
        Status::TooLarge { count: 0, .. } => {
            koru_i18n::t(lang, Key::DiagramTooLargeUncounted).to_owned()
        }
        Status::TooLarge { kind, count } => {
            let key = match (kind, *count) {
                (DiagramKind::Sequence, 1) => Key::DiagramTooLargeMessagesOne,
                (DiagramKind::Sequence, _) => Key::DiagramTooLargeMessagesMany,
                (DiagramKind::Flowchart, 1) => Key::DiagramTooLargeBoxesOne,
                (DiagramKind::Flowchart, _) => Key::DiagramTooLargeBoxesMany,
                // Counted, but of a kind Koru does not draw: the number has
                // no unit a person would recognise.
                (DiagramKind::Other(_), _) => Key::DiagramTooLargeUncounted,
            };
            let template = koru_i18n::t(lang, key);
            if template.contains("{}") {
                koru_i18n::fill(
                    template,
                    &[&koru_i18n::number::decimal(lang, *count as u64)],
                )
            } else {
                template.to_owned()
            }
        }
        Status::Empty => koru_i18n::t(lang, Key::DiagramEmptyHint).to_owned(),
    };
    Some(text)
}

/// What a screen reader hears on a diagram row, or `None` when the row is
/// read as the code text it shows.
///
/// `None` while the text is open: the person is editing it, and the text is
/// what they hear. Closed, the block is one of three things and says which:
///
/// - **drawn** — `accTitle` and `accDescr` first when the text declares them,
///   then *Diagram, flowchart, 10 boxes, 12 arrows* or *Diagram, sequence, 4
///   participants: Person, Koru app, Server, Session store; 12 messages*,
///   then every arrow or message as prose in source order, each by the labels
///   a person sees and never by id ([`scene_narration`]);
/// - **drawing** — *Diagram, drawing*, the placeholder's name while the first
///   layout of this text is in flight;
/// - **not drawn** — *Diagram, not drawn: …* with the reason line the block
///   itself shows ([`reason_text`]); an emptied block names its hint. The
///   text is on the page under that name, so the name says why first.
///
/// Pure, and a walk over vectors the host has already prepared: a caller
/// builds it once when it publishes the row, never per frame.
pub fn narration_text(lang: Lang, state: &DiagramRowState) -> Option<String> {
    if state.open {
        return None;
    }
    if state.pending {
        return Some(t(lang, Key::A11yDiagramPending).to_owned());
    }
    if let Some(status) = &state.reason {
        let reason = reason_text(lang, status)?;
        return Some(fill(t(lang, Key::A11yDiagramNotDrawn), &[&reason]));
    }
    if state.empty {
        let hint = t(lang, Key::DiagramEmptyHint);
        return Some(fill(t(lang, Key::A11yDiagramNotDrawn), &[hint]));
    }
    let prepared = state.drawing.as_ref()?;
    Some(scene_narration(lang, prepared.scene()))
}

/// The narration of a scene that drew, in `lang`: the title and the
/// description the text declares, when it does; the summary with its counts;
/// then every arrow or message as prose, in source order, by the labels a
/// person sees. The pieces are sentences joined with `. `; a piece that
/// already ends a sentence — a description with its full stop, a box called
/// *Valid order?* — is not given a second, and the last gets none.
///
/// The counts are digits in `lang`, and a count's noun is the One or the
/// Many row for it. That is a composition, and it is right where a language
/// agrees inside the fragment; a dual, or a case the phrase would govern, is
/// approximated. The sentence joiner and the comma between participants are
/// punctuation, the same in every language here.
pub fn scene_narration(lang: Lang, scene: &DiagramScene) -> String {
    let narration = koru_diagram::narrate(scene);
    let mut sentences: Vec<String> = Vec::with_capacity(narration.items.len() + 3);
    sentences.extend(narration.title);
    sentences.extend(narration.description);
    sentences.push(summary(lang, scene, &narration.counts));
    sentences.extend(narration.items.iter().map(|item| item_text(lang, item)));
    join_sentences(&sentences)
}

/// *Diagram, flowchart, 10 boxes, 12 arrows*, or *Diagram, sequence, 4
/// participants: Person, Koru app, Server, Session store; 12 messages*.
fn summary(lang: Lang, scene: &DiagramScene, counts: &Counts) -> String {
    match counts {
        Counts::Flowchart { nodes, edges, .. } => fill(
            t(lang, Key::A11yDiagramFlowchart),
            &[
                &fragment(
                    lang,
                    Key::A11yDiagramBoxesOne,
                    Key::A11yDiagramBoxesMany,
                    *nodes,
                ),
                &fragment(
                    lang,
                    Key::A11yDiagramArrowsOne,
                    Key::A11yDiagramArrowsMany,
                    *edges,
                ),
            ],
        ),
        Counts::Sequence {
            participants,
            messages,
            ..
        } => {
            // The participants by the label a person sees, in the order they
            // stand — which the narration's items do not carry on their own.
            let names = match &scene.body {
                SceneBody::Sequence(sequence) => sequence
                    .participants
                    .iter()
                    .map(|participant| spoken(&participant.label.text))
                    .collect::<Vec<_>>()
                    .join(", "),
                SceneBody::Flowchart(_) => String::new(),
            };
            fill(
                t(lang, Key::A11yDiagramSequence),
                &[
                    &fragment(
                        lang,
                        Key::A11yDiagramParticipantsOne,
                        Key::A11yDiagramParticipantsMany,
                        *participants,
                    ),
                    &names,
                    &fragment(
                        lang,
                        Key::A11yDiagramMessagesOne,
                        Key::A11yDiagramMessagesMany,
                        *messages,
                    ),
                ],
            )
        }
    }
}

/// A count and its noun: the One row for exactly one, the Many row for
/// everything else, the number in `lang`'s digits.
fn fragment(lang: Lang, one: Key, many: Key, count: usize) -> String {
    let key = if count == 1 { one } else { many };
    fill(t(lang, key), &[&number::decimal(lang, count as u64)])
}

/// One arrow or message as prose.
fn item_text(lang: Lang, item: &NarrationItem) -> String {
    match item {
        NarrationItem::Arrow {
            from,
            label: Some(label),
            to,
        } => fill(t(lang, Key::A11yDiagramArrow), &[from, label, to]),
        NarrationItem::Arrow {
            from,
            label: None,
            to,
        } => fill(t(lang, Key::A11yDiagramArrowUnlabelled), &[from, to]),
        NarrationItem::Message { from, text, to } => {
            fill(t(lang, Key::A11yDiagramMessage), &[from, text, to])
        }
    }
}

/// A label's text as one spoken line: its line breaks are spaces.
fn spoken(text: &str) -> String {
    text.split('\n').collect::<Vec<_>>().join(" ")
}

/// `sentences` joined with `. `, without doubling a full stop (or a question
/// mark, or an exclamation) a sentence already ends in, and with none after
/// the last.
fn join_sentences(sentences: &[String]) -> String {
    let mut out = String::new();
    for (index, sentence) in sentences.iter().enumerate() {
        out.push_str(sentence);
        if index + 1 == sentences.len() {
            break;
        }
        if !sentence
            .trim_end()
            .ends_with(['.', '!', '?', '。', '！', '？'])
        {
            out.push('.');
        }
        out.push(' ');
    }
    out
}

/// The byte offset where 1-based `line` of `text` starts — where *Edit text*
/// puts the caret for a `NotParsed` status. A line past the end lands at the
/// end of the text, so a status naming a line that has since been deleted
/// still opens the block somewhere sensible.
pub fn line_start(text: &str, line: u32) -> usize {
    let wanted = line.max(1) as usize - 1;
    let mut at = 0usize;
    for (index, piece) in text.split_inclusive('\n').enumerate() {
        if index == wanted {
            return at;
        }
        at += piece.len();
    }
    text.len()
}
