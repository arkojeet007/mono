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

use std::collections::hash_map::{DefaultHasher, Entry};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use koru_core::NodeId;
use koru_core::node::Kind;
use koru_diagram::Kind as DiagramKind;
use koru_draw::diagram::PreparedDiagram;
use koru_i18n::{Key, Lang};

/// The status a host answers with, re-exported so a view above this crate can
/// name it without linking the engine's crate itself.
pub use koru_diagram::Status;

use crate::body::Body;
use crate::paragraph::Paragraph;
use crate::view_state::ViewState;

#[cfg(test)]
mod tests;

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
