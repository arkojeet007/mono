// SPDX-License-Identifier: MPL-2.0

//! The view model: rows on screen, derived from the document.
//!
//! This is the split that unblocks everything else. The old `Task` carried its
//! text *and* its `Layout`, `Selection`, `y`, `height` and `dirty` flag, so a
//! calendar or kanban could not read the same data without dragging parley
//! along. Now [`koru_core::Node`] is the truth and [`Row`] is a cache that any
//! view is free to build differently — or not at all.
//!
//! Nothing here is persisted. Every field on a `Row` can be recomputed from the
//! document, which is what makes a remote change safe to apply at any moment:
//! rebuild the rows and the view is correct again.
//!
//! The document itself lives in [`koru_workspace::Workspace`], not here: two
//! views cannot each own one `WorkspaceDoc`. Every method that needs it borrows
//! it for the length of the call.

use std::collections::{HashMap, HashSet};

use koru_core::Kind;
use koru_core::NodeId;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::tasks;
use koru_core::{MarkSet, TextStyle};

use crate::paragraph::Paragraph;
use crate::pillinfo::info_for_node;
use crate::tasks::{ActivitySink, TaskActivity};

/// Answer SPEC §5's concurrent residue for one row, from the document.
///
/// **Gated on the row already being a completed task**, and that gate is the
/// whole performance argument: a rebuild is O(rows) and this would make it
/// O(rows × children) if it ran on all of them. Nothing pays for it in a
/// document that has completed no tasks, and a document that has pays one
/// `children` plus a `read` per child — one level deep, no ancestry — for each
/// completed task row it re-read. It is never asked on a frame; the answer is
/// cached on the row.
fn note_unfinished_children(ws: &WorkspaceDoc, row: &mut Paragraph) {
    let Some(task) = row.task.as_mut() else {
        return;
    };
    task.unfinished_children =
        task.completed && tasks::has_unfinished_structural_child(ws, row.id).unwrap_or(false);
}

/// The rows derived from a document, and where the view is looking into it.
///
/// Not an outline: this is what *both* views scroll through, and the members
/// that are only an outline's business — hide-completed — leave with
/// `TaskOutline`. It does not own the document.
#[derive(Default)]
pub struct Body {
    pub rows: Vec<Paragraph>,
    pub focus: usize,
    /// Where a multi-row selection started, by node id.
    ///
    /// The selection is every row between this and the focused one. Deriving it
    /// rather than storing a list is what stops the two disagreeing: moving the
    /// focus *is* moving the selection, and there is no index to go stale when
    /// `rebuild` remakes the rows.
    pub anchor: Option<NodeId>,
    /// The text range the block selection **started as**, when it did: the
    /// anchor row's directed (anchor byte, focus byte) pair, captured at the
    /// moment a text selection escalated into a block one — the collapse that
    /// escalation performs would otherwise destroy it. Shrinking back to the
    /// anchor row restores it, and the move picker prefills from it.
    pub anchor_text: Option<(NodeId, usize, usize)>,
    /// Canonical members of the block selection, by stable node id.
    ///
    /// `anchor` describes a range while Shift or a drag is extending it; this
    /// set is the result. Ctrl+click can therefore toggle non-adjacent blocks
    /// without inventing a second selection path, and a rebuild can reapply the
    /// same members to its new row cache.
    selected: HashSet<NodeId>,
    pub hide_completed: bool,
    /// When set, only this node's descendants are shown. A document window sets
    /// it once, to the document it opens; an unrooted view leaves it `None`.
    pub root: Option<NodeId>,
    /// Show the body as one flat sequence, ignoring depth.
    ///
    /// A document is prose: its lines follow one another, and nesting one line
    /// under another is an outline idea that means nothing here. Structure in a
    /// document comes from headings, which are a property of a line rather than
    /// its position ([deferred/README.md](../../../docs/deferred/README.md)).
    ///
    /// This is a projection, so it also flattens nesting a document picked up
    /// before the rule existed — rather than rewriting anybody's document to
    /// enforce it.
    pub flat: bool,
    /// Show [`root`](Self::root)'s own text as the first row: the title.
    ///
    /// A `Kind::Document`'s text *is* its title (`docs/architecture/DATAMODEL.md`) and its
    /// children are the body, so the title was the one piece of a document with
    /// no row — it lived in the toolbar, and you renamed it by double-clicking
    /// chrome. As row 0 it inherits typing, selection, marks, undo and the
    /// caret from the rows around it instead of needing a second copy of each.
    ///
    /// What it does *not* inherit is structure: see
    /// [`is_title_row`](Self::is_title_row). Off for a view with no
    /// root, and for a `ListBlock` opened as its own view.
    pub title_row: bool,
    /// What has changed since this body last looked, fed by a Loro subscription.
    ///
    /// `adopt` drains this rather than rebuilding the whole body: typing in one
    /// window refreshes only the edited row in every other. `None` only for a
    /// `Body::default()` used outside a view, which falls back to `rebuild`.
    pub tracker: Option<koru_core::change::ChangeTracker>,
    /// Set by [`rebuild`](Self::rebuild), which runs on any structural change
    /// — a row created, deleted or moved, local or remote. The container drains
    /// it to know when the explore column (a projection of the library) is stale.
    structure_dirty: bool,
    /// The dictation being composed into one of these rows, or `None`.
    ///
    /// **It lives here rather than on `ViewState`** because the provisional run
    /// has to be re-spliced after every row re-read, and this is the only type
    /// that owns the rows *and* is handed the document at each of the three
    /// places one is re-read. See [`crate::dictation`], which is the only
    /// module allowed to open or close it.
    composition: Option<crate::dictation::Composition>,
    /// What has been drained from the tracker but not yet applied to the rows.
    ///
    /// A tab that is not on screen accumulates here and settles the whole lot
    /// in one go when it is next shown — the same idea as `Tab.sized`, one
    /// level down, and for the same reason: work for rows nobody is looking at
    /// is work that can wait.
    pending: Pending,
    /// The activity a task gesture in this view produced and its host has not
    /// drained yet ([ADR-286]).
    ///
    /// **The editor never emits.** A command answers a [`TaskOutcome`]
    /// carrying typed intents; they wait here, in outcome order, until the host
    /// takes them with [`take_task_activity`](Self::take_task_activity) and
    /// hands them to its own wrapper — which is what keeps this crate free of
    /// the events bus and the no-I/O claim provable.
    ///
    /// It lives on the body rather than on `ViewState` for one reason: the
    /// checkbox site is [`set_done`](Self::set_done), which is handed a body
    /// and no view.
    ///
    /// [`TaskOutcome`]: koru_core::tasks::TaskOutcome
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub task_activity: ActivitySink,
    /// The tasks whose **own text** a local gesture in this view has changed
    /// since the host last looked ([ADR-286]).
    ///
    /// Beside [`task_activity`](Self::task_activity) and deliberately not part
    /// of it: an entry here names no event and no reason, because
    /// `task.content.changed` belongs to no command and its boundary is the
    /// host's to decide.
    ///
    /// **Local gestures only.** Nothing on the `defer_pending` /
    /// [`ChangeTracker`] path writes here, and that is the whole rule rather
    /// than an implementation detail: that path drains *remote* changes too, so
    /// noting from it would mint this user's editing activity out of a peer's
    /// keystrokes. The note sites are the edit paths themselves — typing,
    /// deleting, splitting, merging, pasting — each of which is reached only by
    /// something this person did.
    ///
    /// [`ChangeTracker`]: koru_core::change::ChangeTracker
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub task_text_edits: crate::tasks::TaskTextEdits,
    /// Every callout box the rows are in, by the box's node id — the one
    /// place the box's look and extent are kept for drawing and hit-testing.
    /// Filled by [`rebuild`](Self::rebuild) from the walk, which reads every
    /// container's meta anyway; a box is never a row of its own, so nothing
    /// on a row could hold this without copying it onto every line inside.
    callout_boxes: HashMap<NodeId, CalloutBox>,
}

/// A callout box as the rows see it: a [`Kind::Callout`] container whose
/// children are the rows inside it. Derived by `Body::rebuild`, never stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalloutBox {
    pub id: NodeId,
    /// The box this one is inside, when callouts nest.
    pub parent: Option<NodeId>,
    /// `NodeMeta::icon`, read with [`koru_core::callout::icon_of`].
    pub icon: Option<String>,
    /// `NodeMeta::colour`, read with [`koru_core::callout::tint_of`].
    pub color: Option<String>,
    /// The rows inside the box, nested boxes' rows included: consecutive,
    /// because they are the container's subtree in a depth-first walk. Empty
    /// for a box with nothing visible in it.
    pub rows: std::ops::Range<usize>,
}

/// Changes owed to the rows.
///
/// `touched` is only meaningful while `incomplete` is false; see
/// [`Body::defer_pending`].
#[derive(Default)]
struct Pending {
    /// Nodes whose content or meta changed.
    touched: HashSet<NodeId>,
    /// The row *sequence* may have changed — something was created, moved or
    /// deleted where this view can see it.
    structural: bool,
    /// Set when something changed that `touched` does not account for, so a
    /// rebuild must re-read every row rather than trusting the set. The
    /// pessimistic direction: wrong here costs a slow frame, wrong the other way
    /// costs rows that quietly stop matching the document.
    incomplete: bool,
}

impl Pending {
    /// Everything may have changed and nothing can be trusted.
    fn everything() -> Self {
        Self {
            touched: HashSet::new(),
            structural: true,
            incomplete: true,
        }
    }
}

impl Body {
    pub fn new(ws: &WorkspaceDoc) -> Self {
        let mut outline = Self::empty_over(ws);
        outline.rebuild(ws);
        outline
    }

    /// Subscribed to the document, but with no rows in it yet.
    ///
    /// For a caller that is about to set `root` or `flat` and rebuild anyway:
    /// [`new`] would walk the whole outline realm first and the rebuild would
    /// discard all of it. The subscription is the part that must not be skipped
    /// — without it `apply_pending` has nothing to drain and the view goes deaf
    /// to the document.
    ///
    /// [`new`]: Self::new
    pub fn empty_over(ws: &WorkspaceDoc) -> Self {
        Self {
            tracker: Some(ws.track_changes()),
            ..Self::default()
        }
    }

    /// Rebuild every row from the document.
    ///
    /// Called after any change, local or remote. Selections are re-anchored by
    /// node id rather than row index, so a remote insert above the caret does
    /// not move it.
    pub fn rebuild(&mut self, ws: &WorkspaceDoc) {
        self.structure_dirty = true;
        self.rebuild_reusing(ws, None);
    }

    /// [`rebuild`], but re-reading only the nodes in `changed`.
    ///
    /// **The walk is cheap and the reading is not.** Establishing the row
    /// *order* is a `get_meta` per node; establishing a row's *content* is
    /// `ws.read` (which materialises the text into a fresh `String`), `ws.runs`
    /// (which walks the text again as a delta) and `info_for_node` (two more
    /// reads per reference pill). A remote `Enter` changes the order and one
    /// row's content, and used to pay the second cost for every row in the
    /// document.
    ///
    /// A row is reused untouched — parley layout and all — when the node is not
    /// named in `changed` **and** its depth has not moved. The depth check is
    /// not incidental: moving a subtree re-indents nodes that nothing has
    /// "changed", and they would otherwise keep a stale indent.
    ///
    /// `None` means re-read everything, which is what every caller outside
    /// [`catch_up`] wants: they are reacting to something the tracker never
    /// saw.
    ///
    /// [`rebuild`]: Self::rebuild
    /// [`catch_up`]: Self::catch_up
    fn rebuild_reusing(&mut self, ws: &WorkspaceDoc, changed: Option<&HashSet<NodeId>>) {
        let _watch = koru_core::timing::Watch::start("body.rebuild");
        let span = koru_core::timing::Span::start("body rebuild");
        let focused = self.focused_id();
        let mut previous: HashMap<NodeId, Paragraph> =
            self.rows.drain(..).map(|r| (r.id, r)).collect();
        self.callout_boxes.clear();
        let mut reread = 0usize;

        // The title is the root's own text, and `walk_under` starts at its
        // children — so it is prepended here rather than falling out of the
        // walk. It is reused from `previous` on the same terms as any other
        // row, so typing in the body does not re-shape the title.
        if let Some(node) = self
            .title_row
            .then_some(self.root)
            .flatten()
            .and_then(|root| ws.read(root).ok())
        {
            let root = node.id;
            let fresh = changed.is_some_and(|c| !c.contains(&root));
            let mut row = match previous.remove(&root) {
                Some(row) if fresh && row.depth == 0 => row,
                Some(mut row) => {
                    reread += 1;
                    let runs = ws.runs(root).unwrap_or_default();
                    let pills = info_for_node(ws, &node);
                    row.adopt(&node, 0, runs, pills, None, 0);
                    row
                }
                None => {
                    reread += 1;
                    let runs = ws.runs(root).unwrap_or_default();
                    let pills = info_for_node(ws, &node);
                    Paragraph::from_node(&node, 0, runs, pills, None, 0)
                }
            };
            row.is_title = true;
            note_unfinished_children(ws, &mut row);
            self.rows.push(row);
        }

        // `walk_under` descends from the root directly, which is both cheaper
        // than filtering a whole-tree walk and the only way to show a document:
        // it counts depth from the root and never yields the root itself.
        let walk = match self.root {
            Some(root) => ws.walk_under(root).unwrap_or_default(),
            None => ws.walk().unwrap_or_default(),
        };

        // `walk_under` is deliberately a complete model traversal: Explore,
        // blob retention, and generic zoomed views need every descendant. A
        // document body is a narrower projection. A child Document describes
        // library organization and owns a body of its own; neither its title
        // nor anything below it is an inline block of this document.
        let stops_at_child_documents = self
            .root
            .is_some_and(|root| ws.kind_of(root) == Some(Kind::Document));
        let mut skipped_document_depth = None;

        // The list-context stack: `(item_depth, style, counter)`, where
        // `item_depth` is the walk depth of the block's *direct items*. Pushed
        // when the walk enters a `Kind::ListBlock`, popped when it leaves.
        // `counter` tracks the 1-based index per depth-level within the list so
        // numbering (`1, 2, 2.1, 2.2`) is derived from the tree, never stored.
        //
        // The two rules above are on different roots and cannot both fire: a
        // document stops at child documents, a list block seeds a style. Nothing
        // is a `Document` and a `ListBlock` at once.
        let mut list_stack: Vec<(usize, koru_core::ListStyle, Vec<usize>)> = Vec::new();

        // The callout-box stack: `(child_depth, box)`, the walk depth of the
        // box's *direct* children and the box's id — the shape `list_stack`
        // has, for the same reason: a callout is a container like a list
        // block, never a row, and every row the walk yields at or below its
        // children's depth is inside it until the depth comes back up.
        // Innermost wins when callouts nest, which is what
        // `Paragraph::callout` records.
        let mut callout_stack: Vec<(usize, NodeId)> = Vec::new();

        // **A view rooted *at* a list block never walks the block**, because
        // `walk_under` starts below it — so the style would be lost and its
        // items would draw as prose, which is the whole thing `open_list`
        // exists to avoid. Seed the stack from the root instead. Its items are
        // at walk depth 0, which is why the stack holds the item depth rather
        // than the block's: a block at depth `d` would otherwise need `d - 1`
        // here, and that is not a `usize`.
        if let Some(style) = self
            .root
            .and_then(|root| ws.read(root).ok())
            .filter(|node| node.kind == koru_core::Kind::ListBlock)
            .map(|node| node.meta.list_style.unwrap_or(koru_core::ListStyle::Bullet))
        {
            list_stack.push((0, style, Vec::new()));
        }

        for (id, walk_depth) in walk {
            if let Some(boundary) = skipped_document_depth {
                if walk_depth > boundary {
                    continue;
                }
                skipped_document_depth = None;
            }
            let Ok(node) = ws.read(id) else { continue };
            if stops_at_child_documents && node.kind == Kind::Document {
                skipped_document_depth = Some(walk_depth);
                continue;
            }
            if self.hide_completed && node.meta.done {
                continue;
            }

            // Pop list contexts we have left before handling this node. Doing
            // this before the ListBlock arm is load-bearing: adjacent lists are
            // siblings, while a deeper ListBlock correctly retains its outer
            // context.
            //
            // **`<`, not `<=`**: the stack holds the *item* depth, not the
            // block's — see the `open_list` seed above, which has no block to
            // take a depth from. A direct item sits exactly at `item_depth`, so
            // `<=` pops the context of the very row about to use it, and every
            // list in the document draws as prose.
            while let Some((item_depth, _, _)) = list_stack.last() {
                if walk_depth < *item_depth {
                    list_stack.pop();
                } else {
                    break;
                }
            }
            // A box ends where the walk comes back above its children's
            // depth — a sibling of the box, or anything shallower, is outside
            // it. `<`, like the list stack: a direct child sits exactly at
            // `child_depth`. The box's extent closes here, at the first row
            // outside it.
            while let Some((child_depth, _)) = callout_stack.last() {
                if walk_depth < *child_depth {
                    self.close_callout_box(&mut callout_stack);
                } else {
                    break;
                }
            }
            let callout = callout_stack.last().map(|(_, id)| *id);
            if node.kind == Kind::Callout {
                // A callout is a container, not a row: the rows inside it
                // draw its box around themselves, the way a list block's
                // items carry its markers. Its look is kept here, once.
                let _ = previous.remove(&id);
                self.callout_boxes.insert(
                    id,
                    CalloutBox {
                        id,
                        parent: callout,
                        icon: node.meta.icon.clone(),
                        color: node.meta.colour.clone(),
                        rows: self.rows.len()..self.rows.len(),
                    },
                );
                callout_stack.push((walk_depth + 1, id));
                continue;
            }
            if node.kind == koru_core::Kind::ListBlock {
                // A ListBlock is a container, not a row. Push its style so its
                // visible children render markers and derive their ordinals.
                let _ = previous.remove(&id);
                let style = node.meta.list_style.unwrap_or(koru_core::ListStyle::Bullet);
                list_stack.push((walk_depth + 1, style, Vec::new()));
                continue;
            }

            // Determine the list context (if any) and compute the item's
            // 1-based index within its run of same-level siblings.
            let (list_style, list_index) =
                if let Some((item_depth, style, counter)) = list_stack.last_mut() {
                    let item_depth = *item_depth;
                    let rel = walk_depth - item_depth; // 0 for a direct item
                    if rel >= counter.len() {
                        counter.push(1);
                    } else {
                        counter.truncate(rel + 1);
                        counter[rel] += 1;
                    }
                    (Some(*style), counter[rel])
                } else {
                    (None, 0)
                };

            let runs = ws.runs(id).unwrap_or_default();
            let pills = info_for_node(ws, &node);
            // A flat view zeroes depth for prose, but a list subtree keeps its
            // tree depth so items indent and sub-items nest. The list stack
            // being non-empty is the ADR-23 exception.
            let depth = if self.flat && list_stack.is_empty() {
                0
            } else {
                // A nested source list is represented by a nested ListBlock so
                // each level can keep its own marker style. Those containers
                // do not draw rows; subtract all but the outer one so hiding a
                // container never adds a phantom indentation level. A callout
                // box draws no row either, and its inset is `text_x`'s, so
                // every box above the row comes off too — a list inside a box
                // indents exactly as the same list outside it.
                walk_depth
                    .saturating_sub(list_stack.len().saturating_sub(1))
                    .saturating_sub(callout_stack.len()) as u16
            };
            // The whole point: an untouched row at an unmoved depth with the
            // same list context is already correct, so it is moved across
            // without re-reading its content from the document.
            let mut row = previous.remove(&id);
            // Box membership is part of "unmoved": a row that left or joined a
            // callout box changes its inset and its backdrop without a byte
            // of its own text changing.
            if let Some(row) = row.take_if(|row| {
                changed.is_some_and(|c| !c.contains(&id))
                    && row.depth == depth
                    && row.list_style == list_style
                    && row.list_index == list_index
                    && row.callout == callout
            }) {
                self.rows.push(row);
                continue;
            }
            reread += 1;
            let mut row = match row {
                Some(mut row) => {
                    row.adopt(&node, depth, runs, pills, list_style, list_index);
                    row
                }
                None => Paragraph::from_node(&node, depth, runs, pills, list_style, list_index),
            };
            row.callout = callout;
            note_unfinished_children(ws, &mut row);
            self.rows.push(row);
        }
        // A box the walk ended inside closes at the last row.
        while !callout_stack.is_empty() {
            self.close_callout_box(&mut callout_stack);
        }

        // Keep the caret on the same node where possible; the row it lives on
        // may have moved, been indented, or been hidden.
        self.focus = focused
            .and_then(|id| self.rows.iter().position(|r| r.id == id))
            .unwrap_or_else(|| self.focus.min(self.rows.len().saturating_sub(1)));
        // The flags went with the old rows; stable selection ids survive.
        self.mark_selection();
        let n = self.rows.len();
        let unrooted = self.root.is_none();
        span.stop_with(|| {
            // Which walk it was matters more than the count: `root: None` walks
            // the whole document, and a tab that shows one node paying for that
            // is the bug, not the row count. `reread` is the other half: a
            // structural change should re-read a handful of rows, not all of
            // them.
            let what = if unrooted {
                "whole document"
            } else {
                "under root"
            };
            format!("{n} rows, {what}, {reread} re-read")
        });
        self.apply_composition(ws);
    }

    /// The open dictation, or `None`. See [`crate::dictation`].
    pub fn composition(&self) -> Option<&crate::dictation::Composition> {
        self.composition.as_ref()
    }

    pub(crate) fn open_composition(&mut self, composition: crate::dictation::Composition) {
        self.composition = Some(composition);
    }

    pub(crate) fn set_composition_text(&mut self, text: &str) {
        if let Some(open) = self.composition.as_mut() {
            open.set_text(text);
        }
    }

    pub(crate) fn close_composition(&mut self) {
        self.composition = None;
    }

    /// Put the open dictation's transcript back over the row it is composed
    /// into, and **discard it if either endpoint no longer resolves**.
    ///
    /// Called at the end of every path that re-reads a row, which is what makes
    /// the splice survive a rebuild without any caller having to know it
    /// exists. It is also where a remote edit is answered: `defer_pending`
    /// drains a Loro *subscription*, so an update is already in the document by
    /// the time the view hears of it — there is no "before" to commit in. The
    /// endpoints are cursors, so the honest policy is nearly free: resolve them
    /// against the converged document and carry on over the bytes the dictation
    /// meant to replace. A node that has been deleted stops resolving, and that
    /// is the discard.
    ///
    /// **Scoped to the composed node.** A remote edit anywhere else in the
    /// workspace resolves to the same range and re-splices the same bytes.
    fn apply_composition(&mut self, ws: &WorkspaceDoc) {
        let Some(open) = self.composition.as_ref() else {
            return;
        };
        let node = open.node();
        let Some(range) = open.range(ws) else {
            self.composition = None;
            return;
        };
        let text = open.text().to_owned();
        let Some(row) = self.rows.iter().find(|r| r.id == node) else {
            // The composed node is not a row of this body any more — it was
            // hidden, or the view was re-rooted out from under the dictation.
            self.composition = None;
            return;
        };
        match row.composition.as_ref() {
            // Already spliced with exactly this, which is the reused-row case.
            Some(run) if run.authored == range && run.text == text => return,
            // Spliced with something else: the row has to be authored again
            // before it can carry the new one.
            Some(_) => {
                if !self.reread_row(ws, node) {
                    self.composition = None;
                    return;
                }
            }
            None => {}
        }
        let Some(row) = self.rows.iter_mut().find(|r| r.id == node) else {
            self.composition = None;
            return;
        };
        let caret = range.start + text.len();
        if !row.compose(range, &text) {
            self.composition = None;
        } else {
            // The transcript is the live insertion point. A partial replaces
            // the previous partial, so retaining the authored selection would
            // leave the caret behind the words while they grow in front of it.
            // Queue this against the new layout for the same reason ordinary
            // text edits do: the row was just invalidated by `compose`.
            row.place_caret(caret);
        }
    }

    /// Re-read one row from the document, **without** re-applying the dictation
    /// splice. Answers `false` when the row cannot be re-read on its own and
    /// the caller owes a rebuild.
    fn reread_row(&mut self, ws: &WorkspaceDoc, id: NodeId) -> bool {
        let Ok(node) = ws.read(id) else {
            return false;
        };
        let runs = ws.runs(id).unwrap_or_default();
        let pills = info_for_node(ws, &node);
        let Some(row) = self.rows.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        let depth = row.depth;
        let list_style = row.list_style;
        let list_index = row.list_index;
        row.adopt(&node, depth, runs, pills, list_style, list_index);
        true
    }

    /// Whether a structural change happened since the last call. Drained by the
    /// container to refresh projections like the explore column.
    pub fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
    }

    /// Take what the tracker has seen and **remember it without acting on it**.
    ///
    /// For a tab that is not on screen. The drain itself is not optional — the
    /// Loro subscription queues into this tracker whether anyone reads it or
    /// not, so a background tab that never drained would grow a set of every
    /// change ever made. What is deferred is the *work*.
    ///
    /// That work used to happen here for every open tab, and it is why coming
    /// back from offline froze the window: one remote `Enter` set
    /// `structure_dirty`, and each tab answered it with a whole-document
    /// rebuild of its own document.
    pub fn defer_pending(&mut self, ws: &WorkspaceDoc) {
        let Some(tracker) = self.tracker.as_ref() else {
            // A `Body::default()` outside any view: no tracker, so be safe.
            self.pending = Pending::everything();
            return;
        };
        let batch = tracker.drain(ws);
        // **A callout box is no row, but it draws from its own meta.** Its
        // icon and color are a map diff that names only the box, and the
        // row filter below drops what it cannot find a row for — so an agent
        // or a peer restyling a box left it showing the old look until the
        // page was reopened. A touched box is a rebuild; the rows keep the
        // paragraphs they have, because the walk re-reads only the boxes.
        if batch
            .touched
            .iter()
            .any(|id| self.callout_boxes.contains_key(id))
        {
            self.pending.structural = true;
        }
        // Scoped: a change under another document is not this view's business.
        // `structural_touches` answers conservatively — an unnameable change
        // counts as ours.
        if batch.structural_touches(self.root, |id| self.index_of(id).is_some()) {
            self.pending.structural = true;
        }
        // **`touched` needs scoping too, and for the same reason.** Creating a
        // node writes its meta, which arrives as a change to that node — so a
        // paragraph added to another document named a node this view has never
        // heard of, and `refresh` answers a node it cannot find with a whole
        // rebuild. Filtering to rows we actually hold is what makes the
        // structural scoping above worth anything.
        //
        // A node created in *this* document is not a row yet and is dropped
        // here, which is correct: `structural` is set, and the rebuild reads it
        // as a new row regardless.
        let mine: Vec<NodeId> = batch
            .touched
            .into_iter()
            .filter(|id| self.index_of(*id).is_some())
            .collect();
        self.pending.touched.extend(mine);
    }

    /// Apply whatever [`defer_pending`] has accumulated. A no-op when nothing
    /// has changed, which is the common case on any given frame.
    ///
    /// **The rows, and only the rows.** A view is more than its row cache — an
    /// open picker is anchored to a Loro range that a peer's edit can move or
    /// invalidate, and that anchor is view state, not body state. Settling
    /// *that* too is [`Editing::catch_up`], which wraps this call; a host
    /// reaching past it to here re-reads the rows and leaves the picker hanging
    /// from wherever it used to be. That is precisely what the tab container did
    /// until Q-B4 ([ADR-343-overlaycatchup](../../../docs/decisions/README.md)).
    ///
    /// [`defer_pending`]: Self::defer_pending
    /// [`Editing::catch_up`]: crate::Editing::catch_up
    pub fn catch_up(&mut self, ws: &WorkspaceDoc) {
        let pending = std::mem::take(&mut self.pending);
        if pending.structural {
            // Only the nodes actually named get re-read; every other row keeps
            // the `Paragraph` it already had, its parley layout included. When
            // the account is incomplete we cannot trust that, and re-read all.
            let reuse = (!pending.incomplete).then_some(&pending.touched);
            self.rebuild_reusing(ws, reuse);
            return;
        }
        for id in &pending.touched {
            // `refresh` re-reads one row; if the node is not in this body (it is
            // hidden, or belongs to a realm this view does not show) it rebuilds
            // internally, which is the correct fallback.
            self.refresh(ws, *id);
        }
    }

    /// Whether this body is behind the document.
    pub fn is_stale(&self) -> bool {
        self.pending.structural || !self.pending.touched.is_empty()
    }

    /// Mark every row as needing a fresh read, without touching the tracker.
    /// For a caller that changed something the tracker cannot see — the theme,
    /// `hide_completed`, the zoom root.
    pub fn mark_everything_pending(&mut self) {
        self.pending = Pending::everything();
    }

    /// Subscribe to a newly loaded document and rebuild from it.
    ///
    /// The previous tracker was bound to a `WorkspaceDoc` the window no longer
    /// holds — a Store swap. Draining it would see nothing, and rebuilding
    /// without a new subscription would leave the view deaf. The row cache is
    /// kept so a later [`catch_up`](Self::catch_up) can re-anchor the caret by
    /// node id.
    pub fn resubscribe(&mut self, ws: &WorkspaceDoc) {
        self.tracker = Some(ws.track_changes());
        self.pending = Pending::everything();
    }

    /// What the node this view is rooted at is called.
    ///
    /// A document's name. Not the toolbar's
    /// question even though the toolbar draws it — which is why it is here and
    /// not in `docview`, and why the editor no longer depends on the document's
    /// chrome to ask it.
    pub fn root_name(&self, ws: &WorkspaceDoc) -> String {
        self.root
            .and_then(|id| ws.read(id).ok())
            .and_then(|n| n.content.as_text().map(str::to_owned))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Untitled".into())
    }

    /// Whether row `i` is the document's title.
    ///
    /// **The one question every structural operation has to ask.** The title is
    /// an ordinary row for typing, selecting, marking and undoing — that is the
    /// whole reason it is a row — but it is not one for *structure*: it is the
    /// node the body is rooted at, so splitting it would give the document a
    /// sibling, merging it would consume the document, and deleting it would
    /// delete everything shown below it. Indenting, reordering and dragging are
    /// meaningless on it for the same reason.
    ///
    /// Asked by index rather than by id because that is what the call sites
    /// have, and because `rows[0]` is the only row it can ever be.
    pub fn is_title_row(&self, i: usize) -> bool {
        self.title_row && i == 0 && self.rows.first().is_some_and(|r| r.is_title)
    }

    /// Whether the caret currently sits in the title.
    pub fn focus_is_title(&self) -> bool {
        self.is_title_row(self.focus)
    }

    /// The first row that is not the title — where the body proper begins.
    pub fn first_body_row(&self) -> usize {
        usize::from(self.title_row && !self.rows.is_empty())
    }

    /// Empty first body line under the title, caret at its start — Backspace
    /// should collapse it onto the title rather than no-op at the seam.
    pub fn empty_first_body_at_start(&self) -> bool {
        let i = self.focus;
        if i == 0 || !self.is_title_row(i - 1) {
            return false;
        }
        self.focused().is_some_and(|r| {
            r.text.is_empty()
                && r.range().is_empty()
                && r.at_start()
                && r.image.is_none()
                && r.kind != koru_core::node::Kind::Divider
                && r.kind != koru_core::node::Kind::Image
        })
    }

    /// Trim ends, turn tabs into spaces, drop newlines — a document name is a
    /// single-line label. Tabs become spaces so a pasted multi-column name keeps
    /// its word gaps rather than gluing them together.
    pub fn sanitize_title(s: &str) -> String {
        s.chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| *c != '\n' && *c != '\r')
            .collect::<String>()
            .trim()
            .to_owned()
    }

    /// Write a cleaned title back if the live row still has dirt (spaces, tabs).
    pub fn sanitize_title_row(&mut self, ws: &mut WorkspaceDoc) {
        if !self.title_row {
            return;
        }
        let Some(row) = self.rows.first() else {
            return;
        };
        if !row.is_title {
            return;
        }
        let clean = Self::sanitize_title(&row.text);
        if clean == row.text {
            return;
        }
        let id = row.id;
        let caret = row.caret().min(clean.len());
        if ws.set_text(id, &clean).is_ok() {
            self.rebuild(ws);
            if let Some(row) = self.rows.first_mut() {
                row.place_caret(caret);
            }
        }
    }

    /// Move the focus to `target`, taking the rows in between.
    ///
    /// It moves the focus *itself* rather than leaving that to the caller,
    /// because the anchor has to be read before the move: a selection started
    /// after the focus has already gone is anchored one row too far, which is
    /// exactly the bug this signature exists to make impossible.
    ///
    /// Block selection and text selection are exclusive: every marked row has
    /// its text range collapsed so the purple highlight cannot sit on the
    /// block chip.
    pub fn extend_selection_to(&mut self, target: usize) {
        let escalating = self.anchor.is_none();
        if escalating {
            self.anchor = self.focused_id();
            // What the selection *started as*: the anchor row's live text
            // range, read before the collapse below destroys it. It is what
            // shrinking back restores, and what the move picker prefills
            // from. Directed (anchor byte, focus byte), because a restore
            // that anchored at the wrong end would grow backwards on the
            // next extension.
            self.anchor_text = self.focused().and_then(|row| {
                row.directed_range()
                    .map(|(anchor, focus)| (row.id, anchor, focus))
            });
        }
        self.focus = target;
        self.mark_selection();
        // Shrunk back to the row the selection started in, and it started as
        // text: turn the one-row chip back into that text selection.
        if !escalating
            && let Some((id, anchor, focus)) = self.anchor_text
            && self.anchor == Some(id)
            && self.selected.len() == 1
            && self.selected.contains(&id)
        {
            self.clear_selection();
            if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                row.select_range_now(anchor, focus);
            }
            return;
        }
        self.collapse_text_on_selected();
    }

    /// Drop text ranges on every block-selected row.
    pub fn collapse_text_on_selected(&mut self) {
        for row in &mut self.rows {
            if row.selected {
                row.collapse_text_selection();
            }
        }
    }

    /// Drop the selection back to the focused row alone.
    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.anchor_text = None;
        self.selected.clear();
        for row in &mut self.rows {
            row.selected = false;
        }
    }

    /// Promote the selection to every visible body row in this projection.
    ///
    /// A document title is row zero for editing, but it is not part of the
    /// document body. Keeping it out here gives copy, cut, deletion, and the
    /// selection highlight one shared answer instead of filtering it later in
    /// only the structural paths.
    pub fn select_all_rows(&mut self) {
        self.anchor = None;
        self.anchor_text = None;
        let first = self.first_body_row();
        self.selected = self.rows[first..].iter().map(|row| row.id).collect();
        for (index, row) in self.rows.iter_mut().enumerate() {
            row.selected = index >= first;
        }
        self.collapse_text_on_selected();
    }

    /// Toggle one block without disturbing the other Ctrl-selected blocks.
    pub fn toggle_selection(&mut self, target: usize) {
        let Some(id) = self.rows.get(target).map(|row| row.id) else {
            return;
        };
        self.anchor = None;
        self.anchor_text = None;
        self.focus = target;
        if !self.selected.insert(id) {
            self.selected.remove(&id);
        }
        self.mark_selection();
        self.collapse_text_on_selected();
    }

    /// Re-derive `selected` from the anchor and the focus. Called after any
    /// rebuild, because the rows — and their flags — are thrown away by it.
    pub fn mark_selection(&mut self) {
        if let Some(anchor) = self.anchor {
            let Some(from) = self.index_of(anchor) else {
                // The anchor row is gone: a delete, or a remote change.
                self.anchor = None;
                self.selected.clear();
                for row in &mut self.rows {
                    row.selected = false;
                }
                return;
            };
            let last = self.rows.len().saturating_sub(1);
            let (lo, hi) = (from.min(self.focus), from.max(self.focus).min(last));
            self.selected = self.rows[lo..=hi].iter().map(|row| row.id).collect();
        } else {
            let live: HashSet<_> = self.rows.iter().map(|row| row.id).collect();
            self.selected.retain(|id| live.contains(id));
        }
        for row in &mut self.rows {
            row.selected = self.selected.contains(&row.id);
        }
    }

    /// What the next keystroke would carry — see [`Paragraph::armed`].
    ///
    /// Here rather than on the editor because it is a question about the
    /// *focused* row, and the focus is this layer's. That is what lets the
    /// document toolbar light up its bold button without naming an editor.
    pub fn armed(&self) -> (Option<MarkSet>, Option<TextStyle>) {
        self.focused().map(Paragraph::armed).unwrap_or((None, None))
    }

    /// Whether rows are actually *marked* — a real multi-row selection, as
    /// opposed to the focused row that [`selected_ids`] falls back to.
    ///
    /// The distinction matters wherever the fallback would be wrong. A paste
    /// replaces marked rows, and must not replace the row the caret merely
    /// happens to be sitting in.
    ///
    /// [`selected_ids`]: Self::selected_ids
    pub fn has_row_selection(&self) -> bool {
        self.rows.iter().any(|r| r.selected)
    }

    /// The selected rows, or just the focused one when nothing is selected.
    ///
    /// Callers get a list either way, so "delete the selection" and "delete this
    /// row" are the same code path.
    pub fn selected_ids(&self) -> Vec<NodeId> {
        let ids: Vec<NodeId> = self
            .rows
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.id)
            .collect();
        if ids.is_empty() {
            self.focused_id().into_iter().collect()
        } else {
            ids
        }
    }

    /// The selected rows whose parent is *not* also selected.
    ///
    /// Structural operations act on these and these only: a row that moves or
    /// dies with its parent must not be operated on twice, and deleting one
    /// after its parent has gone hits a node that no longer exists.
    ///
    /// **The title is never one of them.** `select_all_rows` already leaves it
    /// out; this filter is still required because `selected_ids` falls back to
    /// the focused row when there is no explicit row selection. A caret in the
    /// title must never make "delete this block" delete the document itself.
    pub fn selection_roots(&self, ws: &WorkspaceDoc) -> Vec<NodeId> {
        let title = self.title_row.then_some(self.root).flatten();
        let ids: Vec<NodeId> = self
            .selected_ids()
            .into_iter()
            .filter(|id| Some(*id) != title)
            .collect();
        ids.iter()
            .copied()
            .filter(|&id| {
                !ws.ancestors(id)
                    .unwrap_or_default()
                    .iter()
                    .any(|a| ids.contains(a))
            })
            .collect()
    }

    /// The row a point in document space lands on, if any.
    ///
    /// A row owns the space a heading opened above it, so a click in that gap
    /// puts the caret in the heading rather than dropping it — which is what
    /// `Editing::mouse_down` does with a `None`. Without this, a tall H1 would
    /// grow a 28px band across the page where clicking deselects.
    ///
    /// The title's gap *below* is the mirror case and belongs to the **next**
    /// row: it is 40px of air between the name and the first paragraph, and
    /// leaving it unclaimed put the caret on the title whenever a click aimed
    /// just above a body pill drifted up into that band — alternating with a
    /// click that hit the pill row itself.
    pub fn row_at_y(&self, doc_y: f32) -> Option<usize> {
        if let Some(i) = self
            .rows
            .iter()
            .position(|r| doc_y >= r.y - r.space_above && doc_y <= r.y + r.height)
        {
            return Some(i);
        }
        // Title gap: past the title's own box, before the next row's top.
        for (i, row) in self.rows.iter().enumerate() {
            if !row.is_title {
                continue;
            }
            let Some(next) = self.rows.get(i + 1) else {
                break;
            };
            let gap_start = row.y + row.height;
            if doc_y > gap_start && doc_y < next.y - next.space_above {
                return Some(i + 1);
            }
        }
        None
    }

    /// Whether a point in document space is past the end of the rows — the
    /// empty page below the last one. No rows at all is also "below the last".
    ///
    /// Both views treat that space as belonging to the last row, so the test is
    /// here rather than in either of them.
    pub fn below_last_row(&self, doc_y: f32) -> bool {
        match self.rows.last() {
            Some(last) => doc_y > last.y + last.height,
            None => true,
        }
    }

    /// The row nearest a point, clamped to the ends. Used while dragging, where
    /// the pointer is often in the gap between two rows and "nowhere" would
    /// make the drop target flicker.
    pub fn row_near_y(&self, doc_y: f32) -> usize {
        if let Some(i) = self.row_at_y(doc_y) {
            return i;
        }
        let mut best = 0;
        for (i, row) in self.rows.iter().enumerate() {
            // Fall back to the last row whose top (with heading air) we have
            // passed, so a drag past the end still names something.
            if doc_y >= row.y - row.space_above {
                best = i;
            }
        }
        best
    }

    /// Split the focused row at the caret, landing on the new one.
    ///
    /// A list operation, not a line one: it makes a second node, and a
    /// paragraph has no way to reach its neighbours.
    ///
    /// **The tail is a creation, so it classifies** — see
    /// [`classify_new_row`](Self::classify_new_row).
    pub fn split_focused(&mut self, ws: &mut WorkspaceDoc) -> bool {
        ws.checkpoint();
        // Enter in the title moves into the body instead of splitting: a split
        // would give the *document* a sibling, since the title is the node this
        // body is rooted at. Falling into the first line is what every editor
        // with a title field does, and it is the only reading of Enter here
        // that is not destructive.
        if self.focus_is_title() {
            return self.leave_title(ws);
        }
        let Some(row) = self.focused() else {
            return false;
        };
        let (id, range) = (row.id, row.range());
        let at = range.start;
        if !range.is_empty() {
            let _ = ws.delete_text(id, range);
        }
        // An empty quote ends the quotation instead of creating an endless
        // run of empty quoted rows. A non-empty quote still goes through the
        // ordinary split below, which preserves its kind.
        if at == 0 && row.kind == koru_core::node::Kind::Quote && row.text.is_empty() {
            let _ = ws.set_kind(id, koru_core::node::Kind::Paragraph);
            self.rebuild(ws);
            self.focus_on(id);
            if let Some(row) = self.focused_mut() {
                row.place_caret(0);
                row.disarm();
            }
            return true;
        }
        // Enter on the empty last line of a callout box steps out of it — the
        // rule lists have for their empty last item. Only a *direct* child
        // paragraph of the box leaves; an empty list item inside the box
        // first takes the list exit below, which makes it exactly that. The
        // only line of a box stepping out would leave an empty box nothing
        // can reach — no row draws it — so the box goes with it.
        if at == 0
            && row.kind == Kind::Paragraph
            && row.list_style.is_none()
            && row.text.is_empty()
            && let Some(callout) = row.callout
            && let Ok(children) = ws.children(Parent::Node(callout))
            && children.last() == Some(&id)
            && let Ok(parent) = ws.parent(callout)
            && let Ok(index) = ws.sibling_index(callout)
        {
            let moved = if children.len() == 1 {
                ws.dissolve_callout(callout).is_ok()
            } else {
                ws.move_node(id, parent, index + 1).is_ok()
            };
            if !moved {
                return false;
            }
            self.rebuild(ws);
            self.focus_on(id);
            if let Some(row) = self.focused_mut() {
                row.place_caret(0);
                row.disarm();
            }
            return true;
        }
        // Enter on an empty list item steps out one level rather than
        // continuing the list — the same path as Shift+Tab / Backspace-at-0, so
        // the row stays in sequence (following siblings come with it as
        // children) and an outermost item extracts in place instead of jumping
        // past the rest of the list.
        let exit_list = at == 0 && row.list_style.is_some() && row.text.is_empty();
        if exit_list {
            return self.outdent_list_item(ws, id);
        }
        let Ok(tail) = ws.split(id, at) else {
            return false;
        };
        // Splitting takes text *out* of the row that was there, so the row that
        // stays is one whose own text this gesture changed. The tail is a new
        // node; whatever it becomes is a creation, not an editing burst.
        self.note_text_edit(ws, id);
        self.classify_new_row(ws, tail);
        self.rebuild(ws);
        self.focus_on(tail);
        if let Some(row) = self.focused_mut() {
            row.place_caret(0);
            row.disarm();
        }
        true
    }

    /// Move the caret out of the title and into the first line of the body,
    /// making one when the document has none yet.
    ///
    /// Always reports `true`: the caret moved, so the caller has to redraw even
    /// in the case where no node was created.
    fn leave_title(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.sanitize_title_row(ws);
        if self.rows.len() <= 1 {
            let under = match self.root {
                Some(root) => koru_core::schema::Parent::Node(root),
                None => koru_core::schema::Parent::Root,
            };
            if let Ok(node) = koru_core::node::Node::paragraph("") {
                let _ = ws.insert(under, node);
            }
            self.rebuild(ws);
        }
        self.focus = 1.min(self.rows.len().saturating_sub(1));
        if let Some(row) = self.focused_mut() {
            row.place_caret(0);
            row.disarm();
        }
        true
    }

    /// Indent or outdent selected list-item roots as one edit. Moving inward is
    /// top-first so every selected sibling lands under the same row above the
    /// selection. Moving outward is bottom-first so repeated insertion after
    /// the old parent does not reverse the selected rows.
    ///
    /// Nested outdent keeps document order: following siblings under the old
    /// parent become children of the moved row. Outermost items extract in place
    /// (`extract_list_item`) and split a numbered suffix so it restarts at one.
    ///
    /// **Tab and Shift+Tab are structural user commands, so they classify**
    /// (SPEC §5, [ADR-285]) — the same seam a drop and a paste already go
    /// through. Indenting an unchecked item under a completed task reopens it,
    /// reason `UNFINISHED_CHILD_ADDED`, and it is told as a *move*
    /// (`task.child.moved`) because that is the gesture that produced it.
    ///
    /// Asked **after** the move has landed and **before** the rebuild, so the
    /// rows are built from the reopened state rather than the completed one;
    /// and after this method's own trailing `checkpoint`, so the reopen is its
    /// **own undo step** exactly as a drop's is. Shift+Tab *out* of a completed
    /// task still opens no group: an outdent's ancestors are a prefix of the
    /// ones it had, so the classification finds no completed ancestor — which
    /// is also every ordinary reorder.
    ///
    /// **The departure half is asked before the move and spent after it**
    /// (T-0c). `task.child.removed` is the one task question an operation
    /// cannot answer afterwards, because the ancestry that answers it is what
    /// the move takes away. It reads and decides nothing — no group, no write —
    /// so a Tab that leaves no task costs one classification per selected row.
    ///
    /// [ADR-285]: ../../../docs/decisions/ADR-276-300.md
    pub fn indent_list_items(&mut self, ws: &mut WorkspaceDoc, ids: &[NodeId], dir: i32) -> bool {
        let focused = self.focused_id();
        let caret = self.focused().map(Paragraph::caret).unwrap_or(0);
        let mut ids: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| self.row(*id).is_some_and(|row| row.list_style.is_some()))
            .collect();
        if ids.is_empty() || dir == 0 {
            return false;
        }
        if dir < 0 {
            ids.reverse();
        }
        // **What each row would leave, asked before it moves.** The arrival
        // question below can be asked afterwards because the ancestry a row
        // landed in is still there to read; the departure question cannot, and
        // this is the chain the move is about to take away.
        let departing = koru_core::tasks::departing_all(ws, &ids);

        ws.checkpoint();
        ws.begin_group();
        let mut landed: Vec<NodeId> = Vec::new();
        for &id in &ids {
            let one = if dir > 0 {
                ws.indent(id).unwrap_or(false)
            } else {
                let direct_list_block = matches!(ws.parent(id), Ok(Parent::Node(parent))
                    if ws.read(parent).is_ok_and(|node| node.kind == Kind::ListBlock));
                if direct_list_block {
                    ws.extract_list_item(id).unwrap_or(false)
                } else {
                    self.root
                        .and_then(|root| ws.outdent_within_keeping_order(id, root).ok())
                        .unwrap_or(false)
                }
            };
            if one {
                landed.push(id);
            }
        }
        ws.checkpoint();

        // Every row that actually moved asks the one classification function
        // whether it reopened the task it landed in. A row that refused to move
        // is not a move and is never asked.
        for &id in &landed {
            let _ = crate::tasks::run(&mut self.task_activity, ws, |ws| {
                koru_core::tasks::reclassify_moved(ws, id)
            });
        }
        // And the other half: a row that left the task it was recorded under.
        // Outdenting inside one task answers nothing, which is every ordinary
        // Shift+Tab; Tab across a task boundary reports the removal here and
        // the arrival above.
        self.note_departures(ws, &departing);

        let moved = !landed.is_empty();
        if moved {
            self.rebuild(ws);
            if let Some(id) = focused {
                self.focus_on(id);
            }
            if let Some(row) = self.focused_mut() {
                row.place_caret(caret.min(row.text.len()));
                row.disarm();
            }
        }
        moved
    }

    /// Single-row compatibility for Backspace at the beginning of a list item.
    pub fn outdent_list_item(&mut self, ws: &mut WorkspaceDoc, id: NodeId) -> bool {
        self.indent_list_items(ws, &[id], -1)
    }

    /// Join the focused row into the one above, or pull the next one up into it.
    ///
    /// The counterpart of [`split_focused`](Self::split_focused), and list-level
    /// for the same reason: it touches two nodes. Returns the seam offset so the
    /// caller can put the caret where the join happened, which is the only place
    /// that reads as "nothing moved".
    pub fn merge(&mut self, ws: &mut WorkspaceDoc, backwards: bool) -> Option<usize> {
        let index = self.focus;
        let id = self.focused_id()?;
        // This must precede the title-seam guard below: the first body row may
        // stop being a list item even though it may never merge into the title.
        if backwards
            && self
                .rows
                .get(index)
                .is_some_and(|row| row.list_style.is_some() && row.caret() == 0)
            && self.outdent_list_item(ws, id)
        {
            return Some(0);
        }
        // Backspace at the start of a callout box's first row takes the box
        // away and keeps everything in it, the way it takes a list marker
        // away before it joins anything: the lines become siblings where the
        // box stood, in order, and the caret stays on this one.
        if backwards
            && let Some(row) = self.rows.get(index)
            && row.caret() == 0
            && let Some(callout) = row.callout
            && self.callout_span(callout).map(|span| *span.start()) == Some(index)
        {
            return self.dissolve_callout(ws, callout);
        }
        // Neither direction across the title seam. Backspace *in* the title has
        // nothing above it to join; Delete at its end would pull the first
        // paragraph into the document's own name; and Backspace at the start of
        // that first paragraph would do the same from the other side. All three
        // are one question: does this join cross row 0?
        if self.is_title_row(index) || (backwards && index > 0 && self.is_title_row(index - 1)) {
            return None;
        }
        let (keep, gone) = if backwards {
            (self.rows.get(index.checked_sub(1)?)?.id, id)
        } else {
            (id, self.rows.get(index + 1)?.id)
        };
        // The box the vanishing row leaves, if it was its last line: a box
        // with nothing inside has no row to reach it from.
        let emptied = ws.parent(gone).ok();
        let seam = ws.merge_into(keep, gone).ok()?;
        if let Some(parent) = emptied {
            let _ = ws.prune_empty_callout(parent);
        }
        // The surviving row gained the other's text. The row that went is a
        // deletion and owes a `task.removed` if it was one, which is the
        // command's business rather than a burst's.
        self.note_text_edit(ws, keep);
        self.rebuild(ws);
        self.focus_on(keep);
        if let Some(row) = self.focused_mut() {
            row.place_caret(seam);
        }
        Some(seam)
    }

    /// Put the caret on whatever a step through history brought back.
    ///
    /// `rebuild` keeps the caret on the node it was already on, which is right
    /// for an undone *edit* and wrong for an undone *delete*: the row that took
    /// the deleted one's place still exists, so nothing moves and the restored
    /// row appears somewhere else with no caret in it.
    ///
    /// A row present now that was not before is what the step brought back.
    /// Undoing a delete restores the node under a new `TreeID` but with its uuid
    /// intact, which is why this compares ids and not positions.
    ///
    /// Returns the row it landed on, so the caller can offer an offset.
    pub fn follow_history(&mut self, before: &[NodeId]) -> Option<NodeId> {
        let back = self
            .rows
            .iter()
            .map(|r| r.id)
            .find(|id| !before.contains(id))?;
        self.focus_on(back);
        Some(back)
    }

    /// Every row's id, for comparing against after a rebuild.
    pub fn ids(&self) -> Vec<NodeId> {
        self.rows.iter().map(|r| r.id).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Where to draw the "just keep typing" hint: on the only row, when that
    /// row is still blank. Returns its text-column origin in document space.
    pub fn placeholder_at(&self) -> Option<(f32, f32)> {
        match &self.rows[self.first_body_row().min(self.rows.len())..] {
            [row] if row.text.is_empty() => Some((row.text_x(), row.y)),
            _ => None,
        }
    }

    /// Where the *title's* hint belongs, when the document has no name yet.
    ///
    /// Separate from [`placeholder_at`](Self::placeholder_at) because the two
    /// answer different questions and are true independently: a document can be
    /// untitled with a page of prose under it, or named with nothing written.
    /// It is also drawn at the title's own step on the type scale rather than
    /// the body's, so it cannot share that function's return.
    pub fn title_placeholder_at(&self) -> Option<(f32, f32)> {
        self.rows
            .first()
            .filter(|_| self.title_row)
            .filter(|row| row.is_title && row.text.is_empty())
            .map(|row| (row.text_x(), row.y))
    }

    pub fn focused(&self) -> Option<&Paragraph> {
        self.rows.get(self.focus)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Paragraph> {
        self.rows.get_mut(self.focus)
    }

    pub fn focused_id(&self) -> Option<NodeId> {
        self.rows.get(self.focus).map(|r| r.id)
    }

    pub fn row(&self, id: NodeId) -> Option<&Paragraph> {
        self.rows.iter().find(|r| r.id == id)
    }

    pub fn row_mut(&mut self, id: NodeId) -> Option<&mut Paragraph> {
        self.rows.iter_mut().find(|r| r.id == id)
    }

    /// Close the innermost open box at the current end of the rows: its
    /// extent runs up to, not including, the next row pushed.
    fn close_callout_box(&mut self, callout_stack: &mut Vec<(usize, NodeId)>) {
        if let Some((_, id)) = callout_stack.pop()
            && let Some(callout) = self.callout_boxes.get_mut(&id)
        {
            callout.rows.end = self.rows.len();
        }
    }

    /// The callout box `id` names, when its rows are in this body.
    pub fn callout_box(&self, id: NodeId) -> Option<&CalloutBox> {
        self.callout_boxes.get(&id)
    }

    /// Every callout box with rows in this body, in no particular order.
    pub fn callout_boxes(&self) -> impl Iterator<Item = &CalloutBox> {
        self.callout_boxes.values()
    }

    /// The row indices a callout box covers, from its first row through its
    /// last, the rows of a box nested inside included. `None` when `id` is
    /// not a box in this body or nothing in it is visible.
    ///
    /// The rows are consecutive — they are the container's subtree in a
    /// depth-first walk — and the walk recorded them, so this costs nothing
    /// per row.
    pub fn callout_span(&self, id: NodeId) -> Option<std::ops::RangeInclusive<usize>> {
        let rows = &self.callout_box(id)?.rows;
        (!rows.is_empty()).then(|| rows.start..=rows.end - 1)
    }

    /// Every row directly inside the callout box `id`, top to bottom. Rows of
    /// a box nested inside belong to that box, not this one. Empty when `id`
    /// is not a box in this body.
    pub fn callout_rows(&self, id: NodeId) -> Vec<NodeId> {
        self.callout_span(id)
            .map(|span| {
                self.rows[span]
                    .iter()
                    .filter(|row| row.callout == Some(id))
                    .map(|row| row.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether row `i` is the first row of the box it is in — the row the
    /// box's icon sits beside, and the one Backspace-at-start dissolves from.
    pub fn is_callout_first_row(&self, i: usize) -> bool {
        self.rows
            .get(i)
            .and_then(|row| row.callout)
            .and_then(|callout| self.callout_span(callout))
            .is_some_and(|span| *span.start() == i)
    }

    /// The outermost box whose rows begin at row `i`, if any — what a drag of
    /// that row's grip moves: the whole container, with everything inside.
    pub fn callout_starting_at(&self, i: usize) -> Option<NodeId> {
        let mut callout = self.rows.get(i)?.callout?;
        let mut outermost = None;
        while let Some(found) = self.callout_box(callout) {
            if found.rows.start != i {
                break;
            }
            outermost = Some(found.id);
            match found.parent {
                Some(parent) => callout = parent,
                None => break,
            }
        }
        outermost
    }

    /// Take the callout box `id` away and keep its lines: they become
    /// siblings where the box stood, in order. One undo step. The caret stays
    /// on the row it was on, at its start — the seam of nothing having moved.
    pub fn dissolve_callout(&mut self, ws: &mut WorkspaceDoc, id: NodeId) -> Option<usize> {
        let focused = self.focused_id();
        ws.checkpoint();
        ws.begin_group();
        let lines = ws.dissolve_callout(id);
        ws.checkpoint();
        let lines = lines.ok()?;
        self.rebuild(ws);
        if let Some(back) = focused.or(lines.first().copied()) {
            self.focus_on(back);
        }
        if let Some(row) = self.focused_mut() {
            row.place_caret(0);
        }
        Some(0)
    }

    pub fn index_of(&self, id: NodeId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    /// Put the caret on `id` if it is visible.
    pub fn focus_on(&mut self, id: NodeId) {
        if let Some(i) = self.index_of(id) {
            self.focus = i;
        }
    }

    /// The parent a new sibling of `row` should be created under.
    pub fn parent_of(&self, ws: &WorkspaceDoc, id: NodeId) -> Parent {
        ws.parent(id).unwrap_or(match self.root {
            Some(r) => Parent::Node(r),
            None => Parent::Root,
        })
    }

    /// Zoom into a node, or back out to the whole outline.
    pub fn zoom_to(&mut self, ws: &WorkspaceDoc, node: Option<NodeId>) {
        self.root = node;
        self.rebuild(ws);
    }

    /// Vertical space the breadcrumb takes, which the rows start below.
    /// Space the body itself needs above the rows. None today — the breadcrumb
    /// that used to live here went with zoom. A view's own chrome is
    /// `Editor::chrome_h`.
    pub fn top_inset(&self) -> f32 {
        0.0
    }

    // -------------------------------------------------------------- editing

    /// Re-read one row after a text edit.
    ///
    /// Cheaper than [`rebuild`] and, more importantly, it cannot move the
    /// caret: typing must never reorder or renumber rows.
    ///
    /// [`rebuild`]: Self::rebuild
    pub fn refresh(&mut self, ws: &WorkspaceDoc, id: NodeId) {
        // The node went away underneath us — a remote delete, or a merge — or
        // it is not a row here. `rebuild` re-applies the dictation splice on
        // its own, so this returns rather than falling through to it.
        if !self.reread_row(ws, id) {
            self.rebuild(ws);
            return;
        }
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            note_unfinished_children(ws, row);
        }
        // A checkbox ticked, or a lifecycle written, on *this* node is a map
        // diff — and a map diff names only the node it happened on, unlike a
        // tree diff, which names the parent too. So nothing else would bring the
        // row above it up to date, and the completed task holding it would go on
        // claiming it has nothing unfinished.
        //
        // One scan of the rows, with the cheap test first: a row that is not a
        // completed task can never carry the flag, so almost every row is a
        // field read and the `contains` is paid only by the handful that could
        // be an answer. Asking `find` per ancestor instead would be that scan
        // once per level of depth.
        let ancestors = ws.ancestors(id).unwrap_or_default();
        for row in self.rows.iter_mut() {
            if row.task.is_some_and(|t| t.completed) && ancestors.contains(&row.id) {
                note_unfinished_children(ws, row);
            }
        }
        self.apply_composition(ws);
    }

    /// Take the activity this view's task gestures owe, leaving none behind.
    ///
    /// The host calls this after dispatching an event and hands what it gets to
    /// its wrapper; a host that never asks simply never appends, which is what
    /// a unit test and a headless view are.
    pub fn take_task_activity(&mut self) -> Vec<TaskActivity> {
        std::mem::take(&mut self.task_activity)
    }

    /// **Rows recorded by `departing_all` were deleted.** Records one
    /// `task.child.removed` for each ([ADR-286], the departure half D-2 and D-5
    /// left unbuilt).
    ///
    /// A delete is unconditional — there is no chain left to re-read — so this
    /// takes no document.
    pub fn note_deletions(&mut self, departing: &[koru_core::tasks::Departing]) {
        self.owe_departures(departing.iter().map(|d| d.deleted()).collect());
    }

    /// **Rows recorded by `departing_all` have moved.** Records
    /// `task.child.removed` only for the ones that actually left the task they
    /// were recorded under — an indent deeper inside the same task, or a
    /// reorder among its siblings, owes nothing and asks nothing of the host.
    ///
    /// Called **after** the move has landed, beside the `reclassify_moved` that
    /// asks the arrival question, so a row carried from one task to another
    /// reports both halves: the removal from the task it left and the arrival
    /// under the task it joined.
    pub fn note_departures(
        &mut self,
        ws: &WorkspaceDoc,
        departing: &[koru_core::tasks::Departing],
    ) {
        let intents = departing
            .iter()
            .filter_map(|d| d.moved(ws).ok().flatten())
            .collect();
        self.owe_departures(intents);
    }

    /// The shared tail: one bounded sink entry, under **one** causation id.
    ///
    /// One gesture is one chain. Deleting three rows out of a task is a single
    /// thing a person did, so the three removals share a correlation the way a
    /// command's cascade does — which is why this builds a [`TaskActivity`]
    /// rather than calling `TaskActivity::alone` per intent, and why an empty
    /// list mints no causation at all.
    fn owe_departures(&mut self, intents: Vec<koru_core::tasks::ActivityIntent>) {
        if intents.is_empty() {
            return;
        }
        crate::tasks::remember(
            &mut self.task_activity,
            TaskActivity {
                causation: koru_core::tasks::CausationId::new(),
                intents,
            },
        );
    }

    /// **One local edit landed on `id`'s own text.** Records nothing unless
    /// `id` is a task ([ADR-286]).
    ///
    /// The one door the edit paths use, so "a local gesture changed a task's
    /// text" is a call rather than four copies of a predicate. It is called
    /// with a node the caller has *just written to*, which is why the task test
    /// is asked here and now: by the time the burst is flushed the node may be
    /// gone, and a reference to it would name nothing.
    ///
    /// The focused-row check in front is only a cost guard. `is_task_node`
    /// reads the node — which materialises its text — and typing is the
    /// hottest path in the app, so a row this view already knows is not a task
    /// answers from the cache it refreshed on the last rebuild. Anything else
    /// falls through to the authoritative read, including a row this body
    /// cannot see.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn note_text_edit(&mut self, ws: &WorkspaceDoc, id: NodeId) {
        if self
            .focused()
            .is_some_and(|row| row.id == id && row.task.is_none())
        {
            return;
        }
        self.task_text_edits.note(ws, id);
    }

    /// Take the tasks this view's local edits have touched, leaving none
    /// behind. The host coalesces them; this view decides no boundary.
    pub fn take_task_text_edits(&mut self) -> Vec<koru_core::EntityRef> {
        self.task_text_edits.take()
    }

    /// **The task the caret is sitting in**, if the focused row is one.
    ///
    /// The other half of a burst boundary: the host compares this against the
    /// task it last saw a caret on, and a change means the caret left. Read
    /// from the row's own cache first, so a window with no task in it pays a
    /// field test per event rather than a document read.
    pub fn caret_task(&self, ws: &WorkspaceDoc) -> Option<koru_core::EntityRef> {
        let row = self.focused()?;
        row.task.as_ref()?;
        crate::tasks::task_ref(ws, row.id)
    }

    /// Force every row to re-shape — a resize, a DPI change, a theme flip.
    pub fn invalidate(&mut self) {
        for row in &mut self.rows {
            row.dirty = true;
        }
    }

    /// Tick or untick a row. **It stays where it is.**
    ///
    /// A completed task used to sink to the end of its sibling group. The order
    /// of a list is something the user arranged, and ticking something off is
    /// not a request to rearrange it — the row jumping out from under the
    /// pointer is the whole cost, and the benefit was already covered by `F4`,
    /// which hides completed tasks outright.
    ///
    /// **What the click means is decided in the domain**, never here
    /// ([ADR-286]). [`tasks::checkbox_completes`] answers whether ticking this
    /// box *is* completion — a task with no structural children — and this
    /// runs `complete_task`/`reopen_task` when it is and
    /// [`WorkspaceDoc::set_checkbox`] when it is not. The second case is both
    /// the plain checklist item, which Tasks does not touch and which never
    /// gains a task, and the rich task's box, which SPEC §5 says is only the
    /// box.
    ///
    /// Either way it is **its own undo step**: both doors `checkpoint()` and
    /// then open one `atomic`, so a click no longer folds into whatever was
    /// typed a moment earlier.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn set_done(&mut self, ws: &mut WorkspaceDoc, id: NodeId, done: bool) {
        // A node that will not resolve answers `false` and takes the ordinary
        // door, which refuses it in turn — the same nothing-happens as before.
        let completes = tasks::checkbox_completes(ws, id).unwrap_or(false);
        let outcome = if completes {
            let reason = if done {
                tasks::LifecycleReason::CheckboxCompleted
            } else {
                tasks::LifecycleReason::CheckboxReopened
            };
            crate::tasks::run(&mut self.task_activity, ws, |ws| {
                if done {
                    tasks::complete_task(ws, id, reason, crate::tasks::today()).map(Some)
                } else {
                    tasks::reopen_task(ws, id, reason).map(Some)
                }
            })
            .map(|_| ())
        } else {
            ws.set_checkbox(id, done).map(|_| ())
        };
        if outcome.is_err() {
            return;
        }
        self.rebuild(ws);
    }

    /// **Complete or reopen a task outright** — SPEC §5's explicit action, the
    /// one a rich task's checkbox is deliberately not.
    ///
    /// A task with structural children has a box that is only a box, so there
    /// has to be a second gesture that means *this whole thing is done*: it
    /// checks the box, cascades through everything checkable below it, and
    /// completes nested tasks, as one atomic change under one causation id.
    /// Reopen never cascades. Both are the canonical commands with
    /// [`LifecycleReason::ExplicitComplete`] / [`ExplicitReopen`] — the editor
    /// decides nothing beyond which of the two the caller asked for
    /// ([ADR-286]).
    ///
    /// A node that is not a task refuses in the domain (`TaskRefusal::NotATask`)
    /// and this leaves the document alone, which is why the offering surface may
    /// dim its row rather than having to hide it.
    ///
    /// [`LifecycleReason::ExplicitComplete`]: koru_core::tasks::LifecycleReason::ExplicitComplete
    /// [`ExplicitReopen`]: koru_core::tasks::LifecycleReason::ExplicitReopen
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn set_task_completed(&mut self, ws: &mut WorkspaceDoc, id: NodeId, completed: bool) {
        let outcome = crate::tasks::run(&mut self.task_activity, ws, |ws| {
            if completed {
                tasks::complete_task(
                    ws,
                    id,
                    tasks::LifecycleReason::ExplicitComplete,
                    crate::tasks::today(),
                )
                .map(Some)
            } else {
                tasks::reopen_task(ws, id, tasks::LifecycleReason::ExplicitReopen).map(Some)
            }
        });
        if outcome.is_err() {
            return;
        }
        self.rebuild(ws);
    }

    /// Add a row below `after`, or at the end when there is nothing to follow.
    ///
    /// The new row takes the **kind of the one above it**, so Enter in a
    /// document makes a paragraph and Enter in a task list makes a task —
    /// without this function knowing which view it is in.
    ///
    /// **The second creation seam**, and it classifies for the same reason
    /// [`split_focused`](Self::split_focused) does — clicking below the last row
    /// of a completed task's checklist makes an unchecked item exactly as
    /// pressing Enter inside it does.
    pub fn append_row(&mut self, ws: &mut WorkspaceDoc, after: Option<NodeId>) -> Option<NodeId> {
        // After a prose line (task/paragraph/document) continue the same kind;
        // after a block of any other kind (code, image, table, canvas, divider,
        // contact) start a fresh paragraph. Clicking past the end of a code block
        // is "continue writing prose", not "start another code block".
        let kind = match after.and_then(|prev| ws.read(prev).ok()).map(|n| n.kind) {
            Some(k) if k.accepts_children() => k,
            _ => Kind::Paragraph,
        };
        let node = Node::new(kind, koru_core::Content::text("")).ok()?;
        let id = match after {
            Some(prev) => {
                let parent = self.parent_of(ws, prev);
                let at = ws.sibling_index(prev).ok()? + 1;
                let id = ws.insert(parent, node).ok()?;
                ws.move_node(id, parent, at).ok()?;
                id
            }
            None => ws.insert(Parent::Root, node).ok()?,
        };
        self.classify_new_row(ws, id);
        self.rebuild(ws);
        self.focus_on(id);
        Some(id)
    }

    /// **A row that has just been created asks what it landed in** (SPEC §5(d),
    /// [ADR-285]) — the creation half of the classification the move gestures
    /// and the paste already run.
    ///
    /// SPEC names it as *"a new unchecked checklist item"*: pressing Enter
    /// inside a completed task's checklist makes work that is not done, and the
    /// task stops being finished the moment it exists. Everything else this is
    /// asked about answers `None` and costs one `classify_child` — a plain
    /// paragraph is passive whatever it is under, and a row with no completed
    /// task above it has no chain to walk. **A fresh row is never checked**, so
    /// there is no "created already finished" case to gate on: `ws.split` makes
    /// an empty node and `append_row` makes one, and neither copies `done`.
    ///
    /// Called **after** the node exists and **before** the rebuild, so the rows
    /// are built from the reopened state rather than the completed one; the
    /// classification's own `checkpoint` makes the reopen its **own undo step**,
    /// which is the answer every sibling gesture gives (D-2's drop, D-5's Tab,
    /// the paste commit). One Ctrl+Z re-completes the task, a second takes the
    /// row back out.
    ///
    /// [ADR-285]: ../../../docs/decisions/ADR-276-300.md
    fn classify_new_row(&mut self, ws: &mut WorkspaceDoc, id: NodeId) {
        let _ = crate::tasks::run(&mut self.task_activity, ws, |ws| {
            koru_core::tasks::reclassify(ws, id)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::WorkspaceId;
    use koru_core::node::NodeMeta;

    fn outline_with(rows: &[(&str, u16)]) -> (WorkspaceDoc, Body) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let mut stack: Vec<NodeId> = Vec::new();
        for (text, depth) in rows {
            let parent = if *depth == 0 {
                Parent::Root
            } else {
                Parent::Node(stack[*depth as usize - 1])
            };
            let id = ws.insert(parent, Node::task(*text).unwrap()).unwrap();
            stack.truncate(*depth as usize);
            stack.push(id);
        }
        let outline = Body::new(&ws);
        (ws, outline)
    }

    fn shown(o: &Body) -> Vec<(String, u16)> {
        o.rows.iter().map(|r| (r.text.clone(), r.depth)).collect()
    }

    /// A callout is a container: it draws no row, every row the walk yields
    /// under it answers to it — a list inside included — and its look and
    /// extent are kept on the body. A callout nested inside is a box of its
    /// own without ending the outer one, and a row moved out of the box stops
    /// answering to it.
    #[test]
    fn a_callout_box_is_a_container_whose_rows_answer_to_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let before = ws
            .insert(Parent::Node(doc), Node::paragraph("before").unwrap())
            .unwrap();
        let outer = ws
            .insert(
                Parent::Node(doc),
                Node::callout(Some("⚠️"), Some("red")).unwrap(),
            )
            .unwrap();
        let line = ws
            .insert(Parent::Node(outer), Node::paragraph("line").unwrap())
            .unwrap();
        let list = ws
            .insert(Parent::Node(outer), Node::list_block().unwrap())
            .unwrap();
        let item = ws
            .insert(Parent::Node(list), Node::paragraph("item").unwrap())
            .unwrap();
        let inner = ws
            .insert(Parent::Node(outer), Node::callout(None, None).unwrap())
            .unwrap();
        let deep = ws
            .insert(Parent::Node(inner), Node::paragraph("deep").unwrap())
            .unwrap();
        let last = ws
            .insert(Parent::Node(outer), Node::paragraph("last").unwrap())
            .unwrap();
        let after = ws
            .insert(Parent::Node(doc), Node::paragraph("after").unwrap())
            .unwrap();
        let mut body = Body::empty_over(&ws);
        body.flat = true;
        body.zoom_to(&ws, Some(doc));

        assert!(body.row(outer).is_none(), "a box is not a row");
        assert!(body.row(inner).is_none());
        let boxed = |body: &Body, id| body.row(id).unwrap().callout;
        assert_eq!(boxed(&body, before), None);
        assert_eq!(boxed(&body, line), Some(outer));
        assert_eq!(boxed(&body, item), Some(outer));
        assert_eq!(boxed(&body, deep), Some(inner));
        assert_eq!(boxed(&body, last), Some(outer));
        assert_eq!(boxed(&body, after), None);
        // The look is the container's, kept once on the body.
        let found = body.callout_box(outer).expect("the box");
        assert_eq!(found.icon.as_deref(), Some("⚠️"));
        assert_eq!(found.color.as_deref(), Some("red"));
        assert_eq!(found.parent, None);
        assert_eq!(body.callout_box(inner).unwrap().parent, Some(outer));
        assert!(body.callout_box(line).is_none(), "a row is not a box");
        // The nested box's row is in the outer box's span but not among its
        // own rows; the outer box reaches past it to `last`.
        assert_eq!(body.callout_rows(outer), vec![line, item, last]);
        assert_eq!(body.callout_rows(inner), vec![deep]);
        assert_eq!(
            body.callout_span(outer),
            Some(body.index_of(line).unwrap()..=body.index_of(last).unwrap())
        );
        assert_eq!(
            body.callout_span(inner),
            Some(body.index_of(deep).unwrap()..=body.index_of(deep).unwrap())
        );
        assert!(body.is_callout_first_row(body.index_of(line).unwrap()));
        assert!(body.is_callout_first_row(body.index_of(deep).unwrap()));
        assert!(!body.is_callout_first_row(body.index_of(item).unwrap()));
        assert_eq!(
            body.callout_starting_at(body.index_of(line).unwrap()),
            Some(outer)
        );
        assert_eq!(
            body.callout_starting_at(body.index_of(deep).unwrap()),
            Some(inner),
            "the inner box starts there, the outer one does not"
        );
        // Prose inside keeps the flat body's depth, a list inside indents as
        // a list outside does, and the box insets every row in it.
        assert_eq!(body.row(line).unwrap().depth, 0);
        assert_eq!(
            body.row(item).unwrap().list_style,
            Some(koru_core::ListStyle::Bullet)
        );
        assert_eq!(body.row(item).unwrap().depth, 1);
        assert!(body.row(line).unwrap().text_x() > body.row(after).unwrap().text_x());

        ws.move_node(last, Parent::Node(doc), 2).unwrap();
        body.rebuild(&ws);
        assert_eq!(boxed(&body, last), None);
        assert_eq!(body.callout_rows(outer), vec![line, item]);
        assert_eq!(
            body.callout_span(outer),
            Some(body.index_of(line).unwrap()..=body.index_of(deep).unwrap()),
            "the span still covers the nested box at its end"
        );
    }

    /// Clicking past a code block continues prose — `append_row` makes a
    /// paragraph after a block, not another code block.
    #[test]
    fn append_row_after_a_code_block_makes_a_paragraph() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let code = ws
            .insert(Parent::Root, Node::code("fn main() {}").unwrap())
            .unwrap();
        let mut body = Body::new(&ws);
        let new = body.append_row(&mut ws, Some(code)).expect("appended");
        assert_eq!(
            ws.read(new).unwrap().kind,
            Kind::Paragraph,
            "clicking past a code block continues prose, not another code block"
        );
    }

    // ------------------------------------------------------ the checkbox
    //
    // What a click on the box *means* is decided in the domain (ADR-273), and
    // these pin the three answers plus the undo boundary the rewiring bought.

    /// **A checkbox click is its own undo step.** It used to fold into whatever
    /// had just been typed — `set_done` wrote raw, with no checkpoint and no
    /// group — so one Ctrl+Z after ticking a row took the tick *and* the last
    /// word back. Both doors the click can take now `checkpoint()` first.
    #[test]
    fn a_checkbox_click_is_its_own_undo_step() {
        let (mut ws, mut body) = outline_with(&[("write it", 0)]);
        ws.enable_undo();
        let row = body.rows[0].id;
        ws.begin_group();
        ws.insert_text(row, 8, " up").unwrap();

        body.set_done(&mut ws, row, true);
        assert!(ws.read(row).unwrap().meta.done, "the box moved");

        assert!(ws.undo().unwrap());
        assert!(!ws.read(row).unwrap().meta.done, "one Ctrl+Z untick it");
        assert_eq!(
            ws.read(row).unwrap().content.as_text(),
            Some("write it up"),
            "and left the typing alone"
        );
    }

    /// A **task with no structural children**: the box and the lifecycle are one
    /// synced state, so ticking it runs `complete_task` and the outcome's
    /// activity is left for the host.
    #[test]
    fn ticking_a_childless_task_completes_it() {
        let (mut ws, mut body) = outline_with(&[("post the form", 0)]);
        let row = body.rows[0].id;
        tasks::ensure_task(&mut ws, row, koru_core::attributes::TaskReason::Explicit).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        body.set_done(&mut ws, row, true);
        let node = ws.read(row).unwrap();
        assert!(node.meta.done, "the box");
        assert_eq!(
            tasks::TaskFields::of(&node).lifecycle,
            koru_core::attributes::Lifecycle::Completed,
            "and the canonical lifecycle, together"
        );
        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1, "one command, one thing owed");
        assert!(matches!(
            owed[0].intents.first(),
            Some(tasks::ActivityIntent::Completed {
                reason: tasks::LifecycleReason::CheckboxCompleted,
                ..
            })
        ));
        assert!(body.take_task_activity().is_empty(), "and it drains");
    }

    /// A **task with structural children**: SPEC §5 gives the box the smaller
    /// meaning, so the click moves the box and nothing else. The two are then
    /// allowed to disagree, and that is the intended state rather than a bug.
    #[test]
    fn ticking_a_task_with_children_moves_only_the_box() {
        let (mut ws, mut body) = outline_with(&[("ship it", 0), ("draft", 1)]);
        let parent = body.rows[0].id;
        let child = body.rows[1].id;
        tasks::ensure_task(&mut ws, parent, koru_core::attributes::TaskReason::Explicit).unwrap();
        tasks::ensure_task(&mut ws, child, koru_core::attributes::TaskReason::Explicit).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        body.set_done(&mut ws, parent, true);
        let node = ws.read(parent).unwrap();
        assert!(node.meta.done, "the box moved");
        assert_eq!(
            tasks::TaskFields::of(&node).lifecycle,
            koru_core::attributes::Lifecycle::Open,
            "the lifecycle did not"
        );
        assert!(!ws.read(child).unwrap().meta.done, "and nothing cascaded");
        assert!(
            body.take_task_activity().is_empty(),
            "the box is not an event"
        );
    }

    /// A **plain checklist item** keeps its ordinary check path and never gains
    /// a task — the whole of what Tasks does not touch.
    #[test]
    fn ticking_a_plain_row_gains_no_task() {
        let (mut ws, mut body) = outline_with(&[("milk", 0)]);
        let row = body.rows[0].id;
        body.set_done(&mut ws, row, true);
        assert!(ws.read(row).unwrap().meta.done);
        assert!(!tasks::is_task_node(&ws, row).unwrap());
        assert!(body.take_task_activity().is_empty());
    }

    // ------------------------------------------- Tab, Shift+Tab and the class
    //
    // A keyboard structural move classifies for the same reason a drop and a
    // paste do (SPEC §5, ADR-272): the four answers, and the undo boundary.

    /// A view rooted on a node holding one list — the shape Tab and Shift+Tab
    /// actually move rows through. Rooted rather than flat because outdenting
    /// is bounded by the view's root, and a body with none cannot do it at all.
    fn checklist(
        style: koru_core::ListStyle,
        items: &[&str],
    ) -> (WorkspaceDoc, Body, NodeId, Vec<NodeId>) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = ws.insert(Parent::Root, Node::task("doc").unwrap()).unwrap();
        let ids: Vec<NodeId> = items
            .iter()
            .map(|text| {
                ws.insert(Parent::Node(root), Node::paragraph(*text).unwrap())
                    .unwrap()
            })
            .collect();
        let block = ws.wrap_in_list_block_many(&ids, style).unwrap();
        let mut body = Body::new(&ws);
        body.root = Some(root);
        body.rebuild(&ws);
        body.task_activity.clear();
        (ws, body, block, ids)
    }

    fn complete(ws: &mut WorkspaceDoc, id: NodeId) {
        tasks::ensure_task(ws, id, koru_core::attributes::TaskReason::Explicit).unwrap();
        tasks::complete_task(
            ws,
            id,
            tasks::LifecycleReason::ExplicitComplete,
            crate::tasks::today(),
        )
        .unwrap();
    }

    fn open_task(ws: &mut WorkspaceDoc, id: NodeId) {
        tasks::ensure_task(ws, id, koru_core::attributes::TaskReason::Explicit).unwrap();
    }

    fn lifecycle(ws: &WorkspaceDoc, id: NodeId) -> koru_core::attributes::Lifecycle {
        tasks::TaskFields::of(&ws.read(id).unwrap()).lifecycle
    }

    /// **Tab is a structural move, so it classifies.** Indenting unfinished work
    /// under a completed task reopens it — the gap D-2 left when it wired the
    /// drop and the paste and nothing else.
    ///
    /// It is told as a **move**, so the feed reads `task.child.moved`, and the
    /// reopen is its **own undo step**: the first Ctrl+Z takes the reopen back
    /// and leaves the row where Tab put it, exactly as a drop's does.
    #[test]
    fn tab_indenting_unfinished_work_under_a_completed_task_reopens_it() {
        let (mut ws, mut body, block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "draft"]);
        complete(&mut ws, ids[0]);
        open_task(&mut ws, ids[1]);
        body.rebuild(&ws);
        body.task_activity.clear();
        ws.enable_undo();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], 1));
        assert_eq!(ws.parent(ids[1]).unwrap(), Parent::Node(ids[0]), "it moved");
        assert_eq!(
            lifecycle(&ws, ids[0]),
            koru_core::attributes::Lifecycle::Open,
            "and the task it landed in reopened"
        );

        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1, "one classification, one thing owed");
        assert!(
            matches!(
                owed[0].intents.first(),
                Some(tasks::ActivityIntent::Reopened {
                    reason: tasks::LifecycleReason::UnfinishedChildAdded,
                    ..
                })
            ),
            "with the reason SPEC §5 names"
        );
        assert!(
            owed[0].intents.iter().any(|intent| matches!(
                intent,
                tasks::ActivityIntent::ChildMoved {
                    class: tasks::ChildClass::UnfinishedStructural,
                    reopened: true,
                    ..
                }
            )),
            "told as a move, not an add"
        );

        assert!(ws.undo().unwrap());
        assert_eq!(
            lifecycle(&ws, ids[0]),
            koru_core::attributes::Lifecycle::Completed,
            "one Ctrl+Z takes the reopen back"
        );
        assert_eq!(
            ws.parent(ids[1]).unwrap(),
            Parent::Node(ids[0]),
            "and leaves the move alone — the reopen is its own step"
        );
        assert!(ws.undo().unwrap());
        assert_eq!(
            ws.parent(ids[1]).unwrap(),
            Parent::Node(block),
            "the second takes the move"
        );
    }

    /// A child that is **already finished** is not unfinished work: indenting a
    /// completed task under a completed task reopens nothing.
    #[test]
    fn indenting_a_completed_child_reopens_nothing() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "draft"]);
        complete(&mut ws, ids[0]);
        complete(&mut ws, ids[1]);
        body.rebuild(&ws);
        body.task_activity.clear();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], 1));
        assert_eq!(
            lifecycle(&ws, ids[0]),
            koru_core::attributes::Lifecycle::Completed,
            "finished work leaves it finished"
        );
        assert!(body.take_task_activity().is_empty(), "and owes nothing");
    }

    /// **Passive content reopens nothing.** A bullet is a note, not a checkable
    /// — it carries no lifecycle of its own and no box — so filing it under a
    /// completed task is filing, not unfinished work.
    #[test]
    fn indenting_passive_content_reopens_nothing() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Bullet, &["ship it", "see the memo"]);
        complete(&mut ws, ids[0]);
        body.rebuild(&ws);
        body.task_activity.clear();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], 1));
        assert_eq!(
            lifecycle(&ws, ids[0]),
            koru_core::attributes::Lifecycle::Completed
        );
        assert!(body.take_task_activity().is_empty());
    }

    /// **Moving out is not adding under.** Shift+Tab lifts a row *off* a
    /// completed task, so the classification finds no completed ancestor and
    /// the task it left stays completed.
    ///
    /// It is, though, exactly the gesture that owes a **departure** (T-0c): the
    /// row left a task, and `task.child.removed` is the only thing owed — no
    /// reopen, no arrival, and nothing that touches the lifecycle.
    ///
    /// The starting state — an open task inside a completed one — is SPEC §5's
    /// concurrent residue, and it is deliberately built with a raw `move_node`
    /// so this test is about the outdent rather than about how it got there.
    #[test]
    fn shift_tab_out_of_a_completed_task_reopens_nothing() {
        let (mut ws, mut body, block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "draft"]);
        complete(&mut ws, ids[0]);
        open_task(&mut ws, ids[1]);
        ws.move_node(ids[1], Parent::Node(ids[0]), 0).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], -1));
        assert_eq!(
            ws.parent(ids[1]).unwrap(),
            Parent::Node(block),
            "it left the task"
        );
        assert_eq!(
            lifecycle(&ws, ids[0]),
            koru_core::attributes::Lifecycle::Completed,
            "which is not a reason to reopen it"
        );

        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1, "one gesture, one chain");
        let [tasks::ActivityIntent::ChildRemoved { task, child }] = owed[0].intents[..] else {
            panic!("the departure, and only the departure: {:?}", owed[0]);
        };
        assert_eq!(child, ids[1]);
        assert_eq!(task.node_id(), ids[0], "filed under the task it left");
    }

    /// **An ordinary reorder owes nothing and costs no undo step.** A document
    /// with no completed task above the row classifies to `None`, which opens
    /// no group at all — so one Ctrl+Z still puts the row back.
    #[test]
    fn an_ordinary_indent_owes_nothing_and_adds_no_undo_step() {
        let (mut ws, mut body, block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["one", "two"]);
        ws.enable_undo();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], 1));
        assert!(body.take_task_activity().is_empty(), "no task, no activity");

        assert!(ws.undo().unwrap());
        assert_eq!(
            ws.parent(ids[1]).unwrap(),
            Parent::Node(block),
            "one Ctrl+Z is the whole gesture"
        );
    }

    // --------------------------------------------- creation (D-2c, SPEC §5(d))
    //
    // The gap D-2 and D-5 left between them: both wired *moves*, and SPEC's
    // first named case is "a new unchecked checklist item". A row that has just
    // been typed was nowhere before, so there is no departure half here — only
    // the arrival, told as `task.child.added`.

    /// A completed task holding a checkbox list whose one item is **checked** —
    /// the shape the creation case is about. Everything below is finished, so
    /// the task is legitimately complete until a new row appears under it.
    fn completed_task_over_a_finished_list() -> (WorkspaceDoc, Body, NodeId, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = ws.insert(Parent::Root, Node::task("doc").unwrap()).unwrap();
        let task = ws
            .insert(Parent::Node(root), Node::paragraph("ship it").unwrap())
            .unwrap();
        let item = ws
            .insert(Parent::Node(task), Node::paragraph("step one").unwrap())
            .unwrap();
        ws.wrap_in_list_block_many(&[item], koru_core::ListStyle::Checkbox)
            .unwrap();
        complete(&mut ws, task);
        let mut body = Body::new(&ws);
        body.root = Some(root);
        body.rebuild(&ws);
        body.task_activity.clear();
        (ws, body, task, item)
    }

    fn caret_to_end(ws: &WorkspaceDoc, body: &mut Body, id: NodeId) {
        let end = ws.text_len(id).unwrap();
        body.focus_on(id);
        if let Some(row) = body.focused_mut() {
            row.place_caret(end);
        }
    }

    /// **Enter is a creation, so it classifies.** Pressing Enter at the end of
    /// the last checked step of a completed task makes a new *unchecked* item —
    /// work that is not done — and the task stops being finished the moment it
    /// exists.
    ///
    /// Told as `task.child.added`, not `moved`: nothing moved. And the reopen is
    /// its **own undo step**, which is the answer every sibling gesture gives
    /// (D-2's drop, D-5's Tab, the paste commit) — the first Ctrl+Z re-completes
    /// the task and leaves the new row alone.
    #[test]
    fn typing_a_new_checklist_item_under_a_completed_task_reopens_it() {
        let (mut ws, mut body, task, item) = completed_task_over_a_finished_list();
        ws.enable_undo();
        caret_to_end(&ws, &mut body, item);

        assert!(body.split_focused(&mut ws));
        let created = body.focused_id().expect("the caret is on the new row");
        assert_ne!(created, item, "a second item exists");
        assert!(
            !ws.read(created).unwrap().meta.done,
            "and a fresh row is never checked"
        );
        assert_eq!(
            lifecycle(&ws, task),
            koru_core::attributes::Lifecycle::Open,
            "so the task it was typed into reopened"
        );

        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1, "one classification, one thing owed");
        assert!(
            matches!(
                owed[0].intents.first(),
                Some(tasks::ActivityIntent::Reopened {
                    reason: tasks::LifecycleReason::UnfinishedChildAdded,
                    ..
                })
            ),
            "with the reason SPEC §5 names"
        );
        assert!(
            owed[0].intents.iter().any(|intent| matches!(
                intent,
                tasks::ActivityIntent::ChildAdded {
                    class: tasks::ChildClass::UnfinishedStructural,
                    reopened: true,
                    ..
                }
            )),
            "told as an add — nothing moved"
        );

        assert!(ws.undo().unwrap());
        assert_eq!(
            lifecycle(&ws, task),
            koru_core::attributes::Lifecycle::Completed,
            "one Ctrl+Z takes the reopen back"
        );
        assert!(
            ws.read(created).is_ok(),
            "and leaves the row — the reopen is its own step"
        );
    }

    /// The same seam from the other direction: **clicking below the last row**
    /// appends one, and an appended checklist item is as unfinished as a typed
    /// one. This is the path `Leaf::append_point` takes.
    #[test]
    fn appending_a_row_under_a_completed_task_reopens_it() {
        let (mut ws, mut body, task, item) = completed_task_over_a_finished_list();

        let created = body.append_row(&mut ws, Some(item)).expect("appended");
        assert_eq!(
            lifecycle(&ws, task),
            koru_core::attributes::Lifecycle::Open,
            "the appended item is unfinished work"
        );
        assert!(!ws.read(created).unwrap().meta.done);
        assert_eq!(body.take_task_activity().len(), 1);
    }

    /// **Plain typing owes nothing.** A paragraph is passive whatever it is
    /// under (SPEC §5's fourth rule), so Enter inside a completed task's prose
    /// leaves the task completed and files no activity — and costs one
    /// `classify_child`, never a group.
    #[test]
    fn typing_a_plain_paragraph_under_a_completed_task_reopens_nothing() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = ws.insert(Parent::Root, Node::task("doc").unwrap()).unwrap();
        let task = ws
            .insert(Parent::Node(root), Node::paragraph("ship it").unwrap())
            .unwrap();
        let note = ws
            .insert(Parent::Node(task), Node::paragraph("a note").unwrap())
            .unwrap();
        complete(&mut ws, task);
        let mut body = Body::new(&ws);
        body.root = Some(root);
        body.rebuild(&ws);
        body.task_activity.clear();
        ws.enable_undo();
        caret_to_end(&ws, &mut body, note);

        assert!(body.split_focused(&mut ws));
        assert_eq!(
            lifecycle(&ws, task),
            koru_core::attributes::Lifecycle::Completed,
            "passive content never reopens"
        );
        assert!(body.take_task_activity().is_empty(), "and owes nothing");

        assert!(ws.undo().unwrap());
        assert_eq!(
            ws.children(Parent::Node(task)).unwrap().len(),
            1,
            "one Ctrl+Z is the whole gesture — no group was opened"
        );
    }

    /// And a new row in a task that is **open** owes nothing either: there is no
    /// completed ancestor to reopen, which is every ordinary Enter in the app.
    #[test]
    fn typing_in_an_open_task_owes_nothing() {
        let (mut ws, mut body, _block, ids) = checklist(koru_core::ListStyle::Checkbox, &["one"]);
        let root = body.root.unwrap();
        open_task(&mut ws, root);
        body.rebuild(&ws);
        body.task_activity.clear();
        caret_to_end(&ws, &mut body, ids[0]);

        assert!(body.split_focused(&mut ws));
        assert!(body.take_task_activity().is_empty());
    }

    // -------------------------------------------------- departures (T-0c)
    //
    // The other half of the four answers above. `task.child.removed` is the one
    // task question a structural move cannot answer afterwards, so these prove
    // the recording happens before the move and is spent after it.

    /// **Indenting deeper inside one task is not a departure.** The row is still
    /// under the task it was recorded against, so nothing left it.
    #[test]
    fn indenting_inside_one_task_owes_no_departure() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "draft"]);
        // The view's root is a task, so both rows are already under one.
        let root = body.root.unwrap();
        open_task(&mut ws, root);
        body.rebuild(&ws);
        body.task_activity.clear();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], 1));
        assert!(
            body.take_task_activity().is_empty(),
            "it moved within the task, not out of it"
        );
    }

    /// **Passive content leaving a task owes nothing** — the reading T-0c took
    /// from ADR-273's row, and the mirror of
    /// `indenting_passive_content_reopens_nothing`.
    #[test]
    fn passive_content_leaving_a_task_owes_no_departure() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Bullet, &["ship it", "see the memo"]);
        complete(&mut ws, ids[0]);
        ws.move_node(ids[1], Parent::Node(ids[0]), 0).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        assert!(body.indent_list_items(&mut ws, &[ids[1]], -1));
        assert!(
            body.take_task_activity().is_empty(),
            "a bullet is filing, not work"
        );
    }

    /// **A deleted structural child is a departure**, filed under the task it
    /// was under, carrying that child's id.
    #[test]
    fn deleting_a_structural_child_removes_it_from_its_task() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "draft"]);
        open_task(&mut ws, ids[0]);
        open_task(&mut ws, ids[1]);
        ws.move_node(ids[1], Parent::Node(ids[0]), 0).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        let departing = koru_core::tasks::departing_all(&ws, &[ids[1]]);
        ws.delete(ids[1]).unwrap();
        body.note_deletions(&departing);

        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1);
        let [tasks::ActivityIntent::ChildRemoved { task, child }] = owed[0].intents[..] else {
            panic!("a delete owes exactly the departure: {:?}", owed[0]);
        };
        assert_eq!(child, ids[1]);
        assert_eq!(task.node_id(), ids[0]);
    }

    /// **A plain row owes nothing** — no task above it, so no task lost
    /// anything.
    #[test]
    fn deleting_a_plain_row_owes_no_departure() {
        let (ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["one", "two"]);
        assert!(koru_core::tasks::departing_all(&ws, &ids).is_empty());
        body.note_deletions(&[]);
        assert!(body.take_task_activity().is_empty());
    }

    /// **Several rows out of one task are one chain.** A gesture is one thing a
    /// person did, so its departures share a causation id the way a command's
    /// cascade does.
    #[test]
    fn several_departures_from_one_gesture_share_a_causation() {
        let (mut ws, mut body, _block, ids) =
            checklist(koru_core::ListStyle::Checkbox, &["ship it", "one", "two"]);
        open_task(&mut ws, ids[0]);
        open_task(&mut ws, ids[1]);
        open_task(&mut ws, ids[2]);
        ws.move_node(ids[1], Parent::Node(ids[0]), 0).unwrap();
        ws.move_node(ids[2], Parent::Node(ids[0]), 1).unwrap();
        body.rebuild(&ws);
        body.task_activity.clear();

        let departing = koru_core::tasks::departing_all(&ws, &[ids[1], ids[2]]);
        ws.delete(ids[1]).unwrap();
        ws.delete(ids[2]).unwrap();
        body.note_deletions(&departing);

        let owed = body.take_task_activity();
        assert_eq!(owed.len(), 1, "one sink entry");
        assert_eq!(owed[0].intents.len(), 2, "two removals, one chain");
    }

    /// `take_structure_dirty` is set by `rebuild` — the signal the container
    /// drains to refresh projections like the explore column.
    #[test]
    fn rebuild_marks_the_body_structure_dirty() {
        let (mut ws, mut body) = outline_with(&[("one", 0)]);
        let _ = body.take_structure_dirty(); // drain the initial rebuild.
        assert!(!body.take_structure_dirty(), "drained");
        ws.insert(Parent::Root, Node::task("two").unwrap()).unwrap();
        body.rebuild(&ws);
        assert!(
            body.take_structure_dirty(),
            "a structural change marks the body dirty"
        );
    }

    #[test]
    fn rows_carry_depth_from_the_tree() {
        let (_ws, o) = outline_with(&[("Phase 1", 0), ("Reward", 1), ("Invite", 1), ("Legal", 0)]);
        assert_eq!(
            shown(&o),
            vec![
                ("Phase 1".into(), 0),
                ("Reward".into(), 1),
                ("Invite".into(), 1),
                ("Legal".into(), 0),
            ]
        );
    }

    #[test]
    fn hiding_completed_removes_rows_without_deleting_them() {
        let (mut ws, mut o) = outline_with(&[("one", 0), ("two", 0)]);
        let two = o.rows[1].id;
        ws.set_done(two, true).unwrap();
        o.hide_completed = true;
        o.rebuild(&ws);

        assert_eq!(shown(&o), vec![("one".into(), 0)]);
        assert!(ws.read(two).is_ok(), "still in the document");
    }

    /// A remote insert above the caret must not move it.
    #[test]
    fn the_caret_follows_its_node_not_its_index() {
        let (mut ws, mut o) = outline_with(&[("first", 0), ("second", 0)]);
        o.focus = 1;
        let focused = o.focused_id().unwrap();

        ws.insert(Parent::Root, Node::task("inserted above").unwrap())
            .unwrap();
        // Move it to the top, as a remote peer might.
        let inserted = ws.walk().unwrap().last().unwrap().0;
        ws.move_node(inserted, Parent::Root, 0).unwrap();
        o.rebuild(&ws);

        assert_eq!(o.focused_id(), Some(focused), "caret stayed on its node");
        assert_eq!(o.focused().unwrap().text, "second");
    }

    #[test]
    fn rooting_at_a_node_shows_only_its_subtree_and_rebases_depth() {
        let (ws, mut o) =
            outline_with(&[("Phase 1", 0), ("Reward", 1), ("Detail", 2), ("Legal", 0)]);
        let phase = o.rows[0].id;

        o.zoom_to(&ws, Some(phase));
        assert_eq!(
            shown(&o),
            vec![("Reward".into(), 0), ("Detail".into(), 1)],
            "the root itself is excluded and depth restarts at zero"
        );

        o.zoom_to(&ws, None);
        assert_eq!(o.rows.len(), 4, "and `None` restores the whole outline");
    }

    #[test]
    fn a_new_sibling_goes_under_the_same_parent() {
        let (ws, o) = outline_with(&[("Phase 1", 0), ("Reward", 1)]);
        let reward = o.rows[1].id;
        let phase = o.rows[0].id;
        assert!(matches!(o.parent_of(&ws, reward), Parent::Node(p) if p == phase));
        assert!(matches!(o.parent_of(&ws, phase), Parent::Root));
    }

    /// A view rooted at a document shows **that document's** body and not the
    /// tree around it.
    ///
    /// It used to make a sharper point: a document lived in the library, `walk`
    /// only ever saw the outline, and so a rooted view *had* to descend
    /// directly. One tree now, so the unrooted body sees everything — which
    /// makes the rooted case the thing worth asserting rather than a
    /// consequence of the realms.
    #[test]
    fn a_view_can_be_rooted_at_a_document() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(
                Parent::Root,
                Node::new(
                    koru_core::Kind::Document,
                    koru_core::Content::text("spec.md"),
                )
                .unwrap(),
            )
            .unwrap();
        ws.insert(Parent::Node(doc), Node::task("Overview").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::task("Details").unwrap())
            .unwrap();
        ws.insert(Parent::Root, Node::task("a task").unwrap())
            .unwrap();

        let mut o = Body::new(&ws);
        assert_eq!(
            shown(&o),
            vec![
                ("spec.md".into(), 0),
                ("Overview".into(), 1),
                ("Details".into(), 1),
                ("a task".into(), 0)
            ],
            "unrooted, the whole tree"
        );

        o.zoom_to(&ws, Some(doc));
        assert_eq!(
            shown(&o),
            vec![("Overview".into(), 0), ("Details".into(), 0)],
            "the document's body, with the document itself excluded"
        );
    }

    /// A nested document is an Explore relationship, not an inline block. The
    /// generic tree walk sees its whole subtree, but the parent document's body
    /// must stop at that document boundary and resume with the next parent row.
    #[test]
    fn a_document_body_does_not_render_a_child_documents_contents() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let parent = ws
            .insert(Parent::Root, Node::document("Parent").unwrap())
            .unwrap();
        let before = ws
            .insert(Parent::Node(parent), Node::paragraph("Before").unwrap())
            .unwrap();
        let child = ws
            .insert(Parent::Node(parent), Node::document("Child").unwrap())
            .unwrap();
        ws.insert(
            Parent::Node(child),
            Node::paragraph("Child contents").unwrap(),
        )
        .unwrap();
        let after = ws
            .insert(Parent::Node(parent), Node::paragraph("After").unwrap())
            .unwrap();

        let mut body = Body::empty_over(&ws);
        body.root = Some(parent);
        body.flat = true;
        body.title_row = true;
        body.rebuild(&ws);

        assert_eq!(
            body.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![parent, before, after],
            "the child document and everything inside it belong to their own view"
        );
    }

    #[test]
    fn group_metadata_reaches_the_row() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(
            Parent::Root,
            Node::task("Phase 1").unwrap().with_meta(NodeMeta {
                level_name: Some("Phase".into()),
                icon: Some("🚀".into()),
                ..Default::default()
            }),
        )
        .unwrap();

        let o = Body::new(&ws);
        // `level_name` is still in the schema; nothing in the view reads it
        // since grouping was removed, but a row must keep carrying it or the
        // replacement will have to re-plumb it.
        assert_eq!(o.rows[0].level_name.as_deref(), Some("Phase"));
        assert_eq!(o.rows[0].icon.as_deref(), Some("🚀"));
    }

    /// Two rows, laid out, the second carrying `space_above`. Set directly
    /// rather than through a reflow: what is under test is the hit band, not
    /// how the number was arrived at.
    fn two_rows_with_space(space_above: f32) -> Body {
        let (_ws, mut o) = outline_with(&[("one", 0), ("two", 0)]);
        o.rows[0].y = 24.0;
        o.rows[0].height = 24.0;
        o.rows[1].y = 24.0 + 24.0 + 2.0 + space_above;
        o.rows[1].height = 40.0;
        o.rows[1].space_above = space_above;
        o
    }

    /// The regression this change is most likely to introduce: `mouse_down`
    /// *drops* the caret when `row_at_y` answers `None`, so the air a heading
    /// opens above itself would otherwise be a band where clicking deselects.
    #[test]
    fn a_click_in_the_space_above_a_heading_lands_on_the_heading() {
        let o = two_rows_with_space(28.0);
        let heading_top = o.rows[1].y;
        assert_eq!(o.row_at_y(heading_top - 1.0), Some(1));
        assert_eq!(o.row_at_y(heading_top - 27.0), Some(1));
        assert_eq!(
            o.row_near_y(heading_top - 1.0),
            1,
            "a drag reads it the same way a click does"
        );
    }

    /// ...and no further. The ordinary gap between two body rows still belongs
    /// to neither, or every click would snap somewhere.
    #[test]
    fn the_gap_between_two_body_rows_still_belongs_to_neither() {
        let o = two_rows_with_space(0.0);
        let between = o.rows[0].y + o.rows[0].height + 1.0;
        assert_eq!(o.row_at_y(between), None);
    }

    /// The title's 40px of air below is not a dead band and not the title: a
    /// click there is aiming at the first body row (often a lone document pill).
    #[test]
    fn a_click_in_the_title_gap_lands_on_the_first_body_row() {
        use koru_draw::text::TextEngine;
        use koru_draw::theme;

        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Title").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("body").unwrap())
            .unwrap();
        let mut o = Body::new(&ws);
        o.flat = true;
        o.title_row = true;
        o.zoom_to(&ws, Some(doc));
        crate::rowlayout::reflow_all(&mut engine, &mut o, 900.0, 0.0);

        assert!(o.rows[0].is_title);
        assert_eq!(o.rows.len(), 2);
        let title_bot = o.rows[0].y + o.rows[0].height;
        let body_top = o.rows[1].y;
        assert!(
            body_top - title_bot >= theme::TITLE_GAP_BELOW,
            "title gap is present: title_bot={title_bot} body_top={body_top}"
        );
        let in_gap = title_bot + theme::TITLE_GAP_BELOW * 0.5;
        assert_eq!(
            o.row_at_y(in_gap),
            Some(1),
            "mid-gap click is the body, not the title and not nowhere"
        );
        assert_eq!(o.row_near_y(in_gap), 1);
        // The title's own box is still the title.
        assert_eq!(o.row_at_y(o.rows[0].y + 1.0), Some(0));
    }
}

#[cfg(test)]
mod sync_tests {
    //! What a body does about somebody else's changes: which ones it decides
    //! concern it, and how much of the document it re-reads to answer them.
    //!
    //! These pin the two halves of the reconnect-freeze fix. Both are easy to
    //! break by accident and neither shows up as a crash — the failure mode is
    //! rows that quietly stop matching the document, or a window that stalls.

    use super::*;
    use koru_core::WorkspaceId;

    /// A workspace holding two documents, each with `each` paragraphs.
    fn two_documents(each: usize) -> (WorkspaceDoc, NodeId, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let mut docs = Vec::new();
        for name in ["One", "Two"] {
            let doc = ws
                .insert(Parent::Root, Node::document(name).unwrap())
                .unwrap();
            for i in 0..each {
                ws.insert(
                    Parent::Node(doc),
                    Node::paragraph(&format!("{name} line {i}")).unwrap(),
                )
                .unwrap();
            }
            docs.push(doc);
        }
        (ws, docs[0], docs[1])
    }

    fn body_over(ws: &WorkspaceDoc, doc: NodeId) -> Body {
        let mut body = Body::empty_over(ws);
        body.flat = true;
        body.zoom_to(ws, Some(doc));
        body
    }

    /// **A structural change inside another document is not this view's
    /// business.**
    ///
    /// One flag said "a tree changed" and a workspace holds every open tab's
    /// content, so a paragraph created in one document rebuilt the rows of
    /// every other. With several tabs open on a large workspace that was the
    /// whole cost of a peer pressing Enter, multiplied by the tab count.
    #[test]
    fn a_change_in_another_document_leaves_this_body_alone() {
        let (mut ws, one, two) = two_documents(4);
        let mut body = body_over(&ws, one);
        assert!(!body.is_stale());

        ws.insert(Parent::Node(two), Node::paragraph("elsewhere").unwrap())
            .unwrap();
        body.defer_pending(&ws);

        assert!(
            !body.is_stale(),
            "a create inside the other document should not concern this view"
        );
    }

    /// …but one inside *this* document certainly is.
    #[test]
    fn a_change_in_this_document_makes_the_body_stale() {
        let (mut ws, one, _two) = two_documents(4);
        let mut body = body_over(&ws, one);

        ws.insert(Parent::Node(one), Node::paragraph("mine").unwrap())
            .unwrap();
        body.defer_pending(&ws);

        assert!(body.is_stale());
        body.catch_up(&ws);
        assert!(body.rows.iter().any(|r| r.text == "mine"));
    }

    /// **A box restyled by a peer or an agent shows its new look.** The look
    /// is the box's own meta, a map diff that names a node with no row, so the
    /// row filter used to drop it and the page kept the old icon and tint until
    /// it was reopened.
    #[test]
    fn a_remote_restyle_of_a_callout_box_reaches_the_body_showing_it() {
        let (mut ws, one, _two) = two_documents(4);
        let lines = ws.children(Parent::Node(one)).unwrap();
        let callout = ws.wrap_in_callout_many(&lines[1..3]).unwrap();
        let mut body = body_over(&ws, one);
        assert_eq!(body.callout_box(callout).unwrap().color, None);
        assert!(!body.is_stale());

        ws.set_icon(callout, Some("🚀")).unwrap();
        ws.set_color(callout, Some("red")).unwrap();
        body.defer_pending(&ws);
        assert!(
            body.is_stale(),
            "the restyle must reach the view showing it"
        );

        body.catch_up(&ws);
        let found = body.callout_box(callout).unwrap();
        assert_eq!(found.icon.as_deref(), Some("🚀"));
        assert_eq!(found.color.as_deref(), Some("red"));
    }

    /// **A delete is the case the parent recording exists for.** By the time
    /// anyone asks, the deleted node's own id no longer resolves — so if only
    /// targets were recorded, deleting a row would look like a change belonging
    /// to no document at all and the row would stay on screen.
    #[test]
    fn deleting_a_row_still_reaches_the_body_showing_it() {
        let (mut ws, one, _two) = two_documents(4);
        let mut body = body_over(&ws, one);
        let doomed = ws.children(Parent::Node(one)).unwrap()[1];

        ws.delete(doomed).unwrap();
        body.defer_pending(&ws);
        assert!(body.is_stale(), "the delete must reach the view showing it");

        body.catch_up(&ws);
        assert_eq!(body.rows.len(), 3);
        assert!(!body.rows.iter().any(|r| r.id == doomed));
    }

    /// **The rebuild re-reads what changed and nothing else.**
    ///
    /// Establishing the row order is a `get_meta` per node; establishing a
    /// row's content is a `read` that materialises the text, a `runs` that
    /// walks it again, and a pill resolution on top. A remote `Enter` changes
    /// the order and one row — and used to pay the second cost for every row in
    /// the document.
    #[test]
    fn a_structural_change_re_reads_only_the_rows_that_changed() {
        let (mut ws, one, _two) = two_documents(200);
        let mut body = body_over(&ws, one);
        let before: Vec<*const u8> = body.rows.iter().map(|r| r.text.as_ptr()).collect();

        ws.insert(Parent::Node(one), Node::paragraph("appended").unwrap())
            .unwrap();
        body.defer_pending(&ws);
        body.catch_up(&ws);

        assert_eq!(body.rows.len(), 201);
        // The 200 rows that did not change kept the *same allocation*, which is
        // only true if nothing re-read them from the document.
        let after: Vec<*const u8> = body
            .rows
            .iter()
            .take(200)
            .map(|r| r.text.as_ptr())
            .collect();
        assert_eq!(
            before, after,
            "untouched rows were re-read from the document"
        );
    }

    /// …and it still notices a row whose text changed underneath it. The reuse
    /// is keyed on what the tracker reports, so a missed report would show as
    /// text that never updates.
    #[test]
    fn a_row_whose_text_changed_is_re_read() {
        let (mut ws, one, _two) = two_documents(20);
        let mut body = body_over(&ws, one);
        let row = ws.children(Parent::Node(one)).unwrap()[5];

        // A structural change *and* a content change in one batch, which is
        // what a peer catching up delivers.
        ws.set_text(row, "changed underneath").unwrap();
        ws.insert(Parent::Node(one), Node::paragraph("and a new one").unwrap())
            .unwrap();
        body.defer_pending(&ws);
        body.catch_up(&ws);

        assert!(
            body.rows.iter().any(|r| r.text == "changed underneath"),
            "the edited row kept its old text through the rebuild"
        );
        assert!(body.rows.iter().any(|r| r.text == "and a new one"));
    }

    /// A move re-indents rows that nothing "changed", so depth is checked as
    /// well as identity. Without that a moved subtree keeps its old indent.
    #[test]
    fn a_moved_row_picks_up_its_new_depth() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let a = ws.insert(Parent::Root, Node::task("a").unwrap()).unwrap();
        let b = ws.insert(Parent::Root, Node::task("b").unwrap()).unwrap();
        let mut body = Body::new(&ws);
        assert_eq!(shown(&body), vec![("a".into(), 0), ("b".into(), 0)]);

        ws.move_node(b, Parent::Node(a), 0).unwrap();
        body.defer_pending(&ws);
        body.catch_up(&ws);

        assert_eq!(
            shown(&body),
            vec![("a".into(), 0), ("b".into(), 1)],
            "the moved row kept its old depth"
        );
    }

    fn shown(o: &Body) -> Vec<(String, u16)> {
        o.rows.iter().map(|r| (r.text.clone(), r.depth)).collect()
    }

    /// A document opened with a title row shows its own name first, and the
    /// name is that row's node — not a copy of the text.
    #[test]
    fn a_title_row_is_the_root_node_itself() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("one").unwrap())
            .unwrap();

        let mut body = Body::empty_over(&ws);
        body.root = Some(doc);
        body.flat = true;
        body.title_row = true;
        body.rebuild(&ws);

        assert_eq!(shown(&body), vec![("Spec".into(), 0), ("one".into(), 0)]);
        assert!(body.is_title_row(0));
        assert!(!body.is_title_row(1));
        assert_eq!(body.rows[0].id, doc);
        assert!(body.rows[0].is_title);
        assert_eq!(body.first_body_row(), 1);
    }

    /// Without the flag there is no title row at all — a bare list and a
    /// `ListBlock` view are unchanged by any of this.
    #[test]
    fn without_the_flag_the_root_is_not_a_row() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("one").unwrap())
            .unwrap();

        let mut body = Body::empty_over(&ws);
        body.root = Some(doc);
        body.flat = true;
        body.rebuild(&ws);

        assert_eq!(shown(&body), vec![("one".into(), 0)]);
        assert!(!body.is_title_row(0));
        assert_eq!(body.first_body_row(), 0);
    }

    /// Enter in the title moves into the body rather than splitting. A split
    /// would give the *document* a sibling, since the title is the node this
    /// body is rooted at.
    #[test]
    fn enter_in_the_title_moves_into_the_body() {
        let (mut ws, mut body, doc) = titled_document(&["one"]);
        body.focus = 0;
        assert!(body.split_focused(&mut ws));

        assert_eq!(body.focus, 1, "the caret is in the body");
        assert_eq!(body.rows.len(), 2, "and nothing was created");
        assert_eq!(ws.read(doc).unwrap().content.as_text(), Some("Spec"));
    }

    /// On a document with nothing in it yet, Enter in the title has to make the
    /// line it moves to.
    #[test]
    fn enter_in_the_title_of_an_empty_document_makes_the_first_line() {
        let (mut ws, mut body, _doc) = titled_document(&[]);
        assert_eq!(body.rows.len(), 1, "the title alone");
        body.focus = 0;
        assert!(body.split_focused(&mut ws));
        assert_eq!(body.rows.len(), 2);
        assert_eq!(body.focus, 1);
        assert_eq!(body.rows[1].text, "");
    }

    /// Neither direction across the title seam. Backspace in it has nothing
    /// above to join; Delete at its end would pull the first paragraph into the
    /// document's own name; and Backspace at the start of that paragraph would
    /// do the same from the other side.
    #[test]
    fn nothing_merges_across_the_title() {
        let (mut ws, mut body, doc) = titled_document(&["one"]);
        body.focus = 0;
        assert_eq!(body.merge(&mut ws, true), None);
        assert_eq!(body.merge(&mut ws, false), None);
        body.focus = 1;
        assert_eq!(body.merge(&mut ws, true), None);

        assert_eq!(ws.read(doc).unwrap().content.as_text(), Some("Spec"));
        assert_eq!(shown(&body), vec![("Spec".into(), 0), ("one".into(), 0)]);
    }

    #[test]
    fn empty_first_body_at_start_is_detected() {
        let (_ws, mut body, _) = titled_document(&[""]);
        body.focus = 1;
        body.rows[1].place_caret(0);
        assert!(body.empty_first_body_at_start());
        let (_ws, mut body, _) = titled_document(&["hello"]);
        body.focus = 1;
        body.rows[1].place_caret(0);
        assert!(!body.empty_first_body_at_start());
    }

    #[test]
    fn sanitize_title_strips_tabs_newlines_and_trims() {
        assert_eq!(Body::sanitize_title("  Hi\tthere\n  "), "Hi there");
        assert_eq!(Body::sanitize_title("Bugs"), "Bugs");
    }

    #[test]
    fn leave_title_writes_a_cleaned_name() {
        let (mut ws, mut body, doc) = titled_document(&[""]);
        ws.set_text(doc, "  Spec\t  ").unwrap();
        body.rebuild(&ws);
        body.focus = 0;
        body.leave_title(&mut ws);
        assert_eq!(ws.read(doc).unwrap().content.as_text(), Some("Spec"));
    }

    /// Select the document body: the title is neither highlighted nor doomed.
    #[test]
    fn selecting_all_rows_starts_after_the_title() {
        let (ws, mut body, doc) = titled_document(&["one", "two"]);
        body.select_all_rows();

        assert!(
            !body.selected_ids().contains(&doc),
            "the document title is not part of its body selection"
        );
        assert!(
            !body.selection_roots(&ws).contains(&doc),
            "but structure never touches it"
        );
        assert_eq!(body.selection_roots(&ws).len(), 2);
    }

    #[test]
    fn adjacent_list_blocks_are_siblings_not_phantom_nested_levels() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        for (style, labels) in [
            (koru_core::ListStyle::Bullet, ["b1", "b2"]),
            (koru_core::ListStyle::Numbered, ["n1", "n2"]),
            (koru_core::ListStyle::Bullet, ["c1", "c2"]),
        ] {
            let ids = labels.map(|label| {
                ws.insert(Parent::Node(doc), Node::paragraph(label).unwrap())
                    .unwrap()
            });
            ws.wrap_in_list_block_many(&ids, style).unwrap();
        }
        let mut body = Body::empty_over(&ws);
        body.root = Some(doc);
        body.flat = true;
        body.rebuild(&ws);

        let depths: Vec<_> = body.rows.iter().map(|row| row.depth).collect();
        assert!(
            depths.iter().all(|depth| *depth == depths[0]),
            "adjacent outer lists share one visual depth: {depths:?}"
        );
        assert_eq!(
            body.rows
                .iter()
                .map(|row| row.list_index)
                .collect::<Vec<_>>(),
            vec![1, 2, 1, 2, 1, 2],
            "each sibling list owns an independent counter"
        );
    }

    /// A document with a title row and `rows` under it.
    fn titled_document(rows: &[&str]) -> (WorkspaceDoc, Body, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        for text in rows {
            ws.insert(Parent::Node(doc), Node::paragraph(*text).unwrap())
                .unwrap();
        }
        let mut body = Body::empty_over(&ws);
        body.root = Some(doc);
        body.flat = true;
        body.title_row = true;
        body.rebuild(&ws);
        (ws, body, doc)
    }
}
