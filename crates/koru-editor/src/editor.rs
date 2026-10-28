// SPDX-License-Identifier: MPL-2.0

//! Input handling. Owns the outline, the overlay, and the text engine.
//!
//! All cursor motion, hit-testing and selection geometry comes from parley's
//! `Selection`, which works directly on a `Layout` — we only decide *which*
//! row's layout to ask, and what to do at a row's edges.
//!
//! Every mutation is an `koru-core` operation on the document, followed by a
//! refresh of the affected rows. Nothing here edits a `String` in place: the
//! CRDT is the truth and a [`Row`] is a cache, which is what makes a remote
//! change safe to apply between any two keystrokes.
//!
//! [`Row`]: crate::paragraph::Paragraph

use koru_core::node::{Align, Attr, AttrValue, Content, Heading, Kind, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::text::{ANCHOR, ANCHOR_LEN, ANCHOR_STR};
use koru_core::{AttrId, DocRef, ListStyle, Mark, NodeId, Ref, Script};
use parley::editing::{Cursor, Selection};
use parley::layout::Affinity;
use std::time::{Duration, Instant};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::body::Body;
use crate::components::command_menu;
use crate::components::emoji_picker::EmojiPicker;
use crate::components::entity_picker::EntityPicker;
use crate::components::{
    Outcome, Overlay, PanelPart, attr_menu, color_picker, date_picker, language_picker,
    link_prompt, list_picker, priority_picker, tag_picker,
};
use crate::input::action::{Action, Scope};
use crate::input::keymap;
use crate::paragraph::Paragraph;
use crate::rowlayout;
use crate::trigger::{self, Anchor, PillSpec, Trigger, TriggerKind};
use crate::view_state::{
    DragRow, Hover, ImageResize, LinkChipHit, OpenWhere, SaveDiagramAs, ScrollAnim, SearchFlash,
    ViewState, image_handle_at,
};

const SEARCH_FLASH_DURATION: Duration = Duration::from_millis(1200);
use koru_draw::geometry::Rect;
use koru_draw::pill::PillKind;
use koru_draw::theme;
use koru_draw::theme::ViewMode;
use koru_workspace::{self, Workspace};

/// How long a smooth page scroll takes to settle. Short enough that typing
/// after a page-down does not wait, long enough to read as motion rather than
/// a jump.
const SCROLL_ANIM_SECS: f32 = 0.15;
/// Selection drags begin scrolling before the pointer has to leave the window.
const DRAG_SCROLL_EDGE: f32 = 48.0;
/// Maximum edge-drag speed. Distance into the edge band scales up to this.
const DRAG_SCROLL_PX_PER_SEC: f32 = 900.0;

fn valid_overlay_span(ws: &WorkspaceDoc, trigger: &Trigger, start: usize, end: usize) -> bool {
    let Some(text) = ws
        .read(trigger.node)
        .ok()
        .and_then(|node| node.content.as_text().map(str::to_owned))
    else {
        return false;
    };
    let Some(span) = text.get(start..end) else {
        return false;
    };
    match trigger.anchor {
        Anchor::TriggerText => {
            span.starts_with(trigger.kind.ch())
                && (trigger.kind == TriggerKind::At
                    || !span[trigger.kind.ch().len_utf8()..]
                        .chars()
                        .any(char::is_whitespace))
        }
        Anchor::Pill => span == ANCHOR_STR,
        Anchor::Insert | Anchor::Chip | Anchor::Selection(_) | Anchor::NewBlock { .. } => true,
        Anchor::Rename(_) | Anchor::RenameImage(_) | Anchor::PageIcon(_) => false,
    }
}

/// The tags `node` already carries, for the chooser's checkmarks.
fn applied_tags(
    ws: &WorkspaceDoc,
    node: Option<NodeId>,
) -> std::collections::HashSet<koru_core::ids::TagId> {
    node.and_then(|n| ws.read(n).ok())
        .map(|n| n.tag_ids().collect())
        .unwrap_or_default()
}

/// The node's current `koru.color`, for the picker's checkmark and Clear row.
fn current_color(
    ws: &WorkspaceDoc,
    node: Option<NodeId>,
) -> Option<koru_core::attributes::AccentColor> {
    let node = node.and_then(|n| ws.read(n).ok())?;
    node.content
        .attrs()
        .iter()
        .find(|a| a.role == koru_core::attributes::KEY_COLOR)
        .and_then(|a| a.as_text())
        .and_then(koru_core::attributes::AccentColor::parse)
}

/// The node's current `koru.due`, for the date picker's seed and Clear row:
/// the calendar day the grid should open on, and whether *any* due is set.
///
/// **The day a picker opens on is a display question**, so a zoned deadline
/// seeds the day it reads as *here* — and that is why the conversion is
/// explicit rather than a `.date()` call. A floating value's day needs no zone
/// at all ([ADR-183]).
///
/// `KEY_DUE` only — not `Node::due`, whose legacy fallback also reads the bare
/// `"due"` role a date *pill* stores; a pill is not the attribute, and
/// offering Clear over one would clear nothing.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
fn current_due(ws: &WorkspaceDoc, node: Option<NodeId>) -> (Option<chrono::NaiveDate>, bool) {
    let Some(node) = node.and_then(|n| ws.read(n).ok()) else {
        return (None, false);
    };
    let value = node
        .content
        .attrs()
        .iter()
        .find(|a| a.role == koru_core::attributes::KEY_DUE)
        .map(|a| &a.value);
    match value.and_then(AttrValue::temporal) {
        Some(koru_core::time::Temporal::Date(d)) => (Some(d.date()), true),
        Some(koru_core::time::Temporal::Floating(wall)) => (Some(wall.date().date()), true),
        Some(koru_core::time::Temporal::Zoned(zoned)) => (
            Some(
                crate::utils::zone::SystemZone::detect()
                    .display_date(&zoned)
                    .date(),
            ),
            true,
        ),
        None => (None, false),
    }
}

/// The exact shared `koru.due` value a relative reminder may follow.
/// The node's current `koru.priority`, for the picker's checkmark and Clear.
fn current_priority(
    ws: &WorkspaceDoc,
    node: Option<NodeId>,
) -> Option<koru_core::attributes::Priority> {
    node.and_then(|n| ws.read(n).ok())?.priority()
}

/// The list a node is in, read the way the Tasks page reads it: the
/// `koru.list` value, a `ListId` in storage form.
fn current_list(ws: &WorkspaceDoc, node: Option<NodeId>) -> Option<koru_core::ids::ListId> {
    let read = node.and_then(|n| ws.read(n).ok())?;
    read.content
        .attrs()
        .iter()
        .find(|a| a.role == koru_core::attributes::KEY_LIST)
        .and_then(|a| a.as_text())
        .and_then(koru_core::ids::ListId::parse)
}

/// `high` — or an unambiguous prefix of one of the four levels — as typed
/// after `/priority`. The four first letters are distinct, so `u`, `h`, `m`
/// and `l` all resolve; an empty or ambiguous string does not.
fn priority_token(arg: &str) -> Option<koru_core::attributes::Priority> {
    let arg = arg.trim().to_lowercase();
    if arg.is_empty() {
        return None;
    }
    let hits: Vec<_> = koru_core::attributes::Priority::ALL
        .into_iter()
        .filter(|p| p.as_str().starts_with(&arg))
        .collect();
    match hits.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

// `set_attribute_routed` and `clear_attribute_routed` lived here until the
// Tasks page became the second surface to write a routed attribute. They are
// `koru_core::tasks::routed::{set_attribute, clear_attribute}` now, with the
// activity types beside them, so the desktop's host can reach them without
// depending on this crate — see that module's note.

/// A local wall-clock moment as epoch milliseconds, through the machine's
/// timezone. The two DST edge cases are decided rather than left to panic:
/// an ambiguous time (clocks fell back) takes its first occurrence, and a
/// nonexistent one (clocks sprang forward) takes the hour after — the moment
/// the wall clock actually shows next.
// The DST gap and fold used to be decided by a `local_epoch_ms` helper here.
// They belong to `utils::zone::SystemZone`, which is the one implementation of
// the timezone rules this build has — see ADR-183.

/// Which attribute picker a host gesture opens — the vocabulary the context
/// menus (document rows and the container's Attributes submenu) share with
/// [`Editing::open_attr_picker_over`]. A2 shipped the first two as `bool`
/// parameters; four choices earn a name.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AttrPicker {
    Tags,
    Color,
    Due,
    Priority,
}

/// The trigger a menu-opened attribute picker anchors at: the first target
/// row, with [`Anchor::Insert`] so committing deletes nothing.
fn attribute_menu_trigger(nodes: &[NodeId]) -> Option<Trigger> {
    let &first = nodes.first()?;
    Some(Trigger {
        kind: TriggerKind::Slash,
        start: 0,
        node: first,
        anchor: Anchor::Insert,
        stable: None,
    })
}

pub struct Editor {
    pub body: Body,
    /// Everything a window needs that is not the rows — see `view_state.rs`.
    pub view: ViewState,
}

/// Give a workspace a line to type on if it has none, and turn on undo.
///
/// Free rather than a constructor, because every view needs it and none of them
/// share a type any more. The line is a real edit and has to reach the transport
/// — a peer that has never seen this workspace would otherwise silently make its
/// own, and both would arrive after the merge.
pub fn prepare(workspace: &mut Workspace) {
    // The line you opened on is not something Ctrl+Z should be able to erase.
    workspace.enable_undo();
}

impl Editor {
    /// A view over a document that already exists — another window, or a test.
    /// It adds nothing to the document — pair it with [`prepare`] when the
    /// workspace may be empty.
    ///
    /// Named `over` rather than `view` because `Editing::view` is the accessor.
    pub fn over(ws: &WorkspaceDoc) -> Self {
        let mut e = Self {
            body: Body::new(ws),
            view: ViewState::default(),
        };
        e.reflow();
        e
    }

    /// A view over one node's subtree — a document — built **once**.
    ///
    /// [`over`] walks whatever `Body::new` defaults to, which is the entire
    /// outline realm, and lays all of it out. A caller that then calls
    /// `zoom_to(Some(doc))` throws every one of those rows and every parley
    /// `Layout` away and does the work again against the right root. That is
    /// what opening a document tab used to cost, once per tab, on the startup
    /// path: two body walks and two full reflows for one visible document.
    ///
    /// There is no reflow here at all. The viewport is not known yet — the
    /// default is 900x560 — and `Editing::set_viewport` invalidates every row on
    /// any width change, so laying out now would be a third pass thrown away by
    /// the first real one. The container already understands this: `Tab.sized`
    /// is `None` until a tab is shown.
    ///
    /// [`over`]: Self::over
    pub fn rooted_at(ws: &WorkspaceDoc, root: NodeId, flat: bool) -> Self {
        let mut body = Body::empty_over(ws);
        body.flat = flat;
        body.zoom_to(ws, Some(root));
        Self {
            body,
            view: ViewState::default(),
        }
    }

    /// Finish a cut once the shell has written the bitmap (see `pending_cut_image`).
    pub fn apply_pending_cut_image(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(id) = self.view.pending_cut_image.take() else {
            return false;
        };
        if self.body.index_of(id).is_none() {
            return false;
        }
        self.body.focus_on(id);
        self.delete_rows(ws, false);
        true
    }
}

impl Editing for Editor {
    fn parts(&mut self) -> (&mut Body, &mut ViewState) {
        (&mut self.body, &mut self.view)
    }
    fn body(&self) -> &Body {
        &self.body
    }
    fn body_mut(&mut self) -> &mut Body {
        &mut self.body
    }
    fn view(&self) -> &ViewState {
        &self.view
    }
    fn view_mut(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

/// The sentence the unfinished-items triangle carries.
///
/// Written once, here, because it is the only explanation the residue state ever
/// gets: a completed task still holding unfinished work is a merge outcome
/// nobody chose (SPEC §5), and the glyph on its own would read as an error. A
/// free constant rather than an associated one — an associated const would make
/// `Editing` not dyn compatible, and `draw` takes `&mut dyn Editing`.
pub const TASK_WARNING_TOOLTIP: &str = "Has unfinished items";
/// Dictation into a document, through the one contract a one-line field also
/// answers ([ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction)).
///
/// The behaviour is in [`crate::dictation`] and reached through [`Editing`], so
/// a view that owns its own `body`/`view` fields — `apps/koru-document`'s
/// `Leaf` — has it without wrapping an `Editor`. This impl exists so the
/// embedded editor answers the trait by name.
///
/// **`composition_rect` is in view space** — the same coordinates
/// `Editor::selection_rects` reports, content column and scroll already
/// applied. A chrome field answers along its own laid-out line instead, so the
/// two are not interchangeable and the host converts.
impl koru_ui::dictation::DictationTarget for Editor {
    type Cx<'a> = &'a mut WorkspaceDoc;

    fn accepts_dictation(&self) -> bool {
        crate::dictation::accepts(&self.view)
    }

    fn begin_composition(&mut self, ws: Self::Cx<'_>) -> bool {
        self.begin_dictation(ws)
    }

    fn update_composition(&mut self, ws: Self::Cx<'_>, text: &str) {
        self.update_dictation(ws, text);
    }

    fn commit_composition(&mut self, ws: Self::Cx<'_>, text: &str) {
        self.commit_dictation(ws, text);
    }

    fn cancel_composition(&mut self, ws: Self::Cx<'_>) {
        self.cancel_dictation(ws);
    }

    fn composition_rect(&self) -> Option<koru_draw::geometry::Rect> {
        crate::dictation::rect(&self.body, &self.view)
    }
}

/// Editing a list of paragraphs: what every view of one shares.
///
/// A trait rather than a struct to own, so a view holds `body` and `view` as its
/// own fields and gains the whole vocabulary — which is what lets the task
/// outline stop being wrapped around a document editor
/// ([decisions/README.md](../../../docs/decisions/README.md)).
///
/// Five required accessors, and everything else is a default. `parts` exists
/// because `reflow` needs the text engine and the rows *simultaneously*, and two
/// separate `&mut self` accessors cannot be held at once.
pub trait Editing {
    fn parts(&mut self) -> (&mut Body, &mut ViewState);
    fn body(&self) -> &Body;
    fn body_mut(&mut self) -> &mut Body;
    fn view(&self) -> &ViewState;
    fn view_mut(&mut self) -> &mut ViewState;

    /// Open a dictation over the focused row's selection, or at its caret.
    /// See [`crate::dictation`] — these five are the whole seam, and the
    /// controller above them (Plan 2.4) owns when each is called.
    ///
    /// **An ordinary local event commits an open composition and is then
    /// re-dispatched**, which is what a keyboard IME does; a *lifecycle* event
    /// — window closed, focus lost, machine slept — cancels instead.
    fn begin_dictation(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let (body, view) = self.parts();
        crate::dictation::begin(body, view, ws)
    }

    /// Replace the provisional transcript. **No Loro operation.**
    fn update_dictation(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        crate::dictation::update(self.body_mut(), ws, text);
    }

    /// Make the transcript real: one logical edit and one undo step. An empty
    /// final transcript cancels rather than deleting the selection.
    fn commit_dictation(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        crate::dictation::commit(self.body_mut(), ws, text);
    }

    /// Throw the composition away: no edit, no undo entry, no trace.
    fn cancel_dictation(&mut self, ws: &mut WorkspaceDoc) {
        crate::dictation::cancel(self.body_mut(), ws);
    }

    /// Where the provisional text is, in **view** coordinates. `None` when
    /// nothing is being composed, or while the transcript is still empty.
    fn dictation_rect(&self) -> Option<koru_draw::geometry::Rect> {
        crate::dictation::rect(self.body(), self.view())
    }

    /// Supply the machine/workspace context used by the headless entity search
    /// host. The editor keeps store access out of its dependency graph.
    fn set_entity_context(
        &mut self,
        current: koru_core::WorkspaceId,
        workspaces: Vec<crate::components::entity_picker::WorkspaceChoice>,
    ) -> bool {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => picker.set_context(current, workspaces),
            _ => false,
        }
    }

    fn take_entity_search_request(
        &mut self,
    ) -> Option<crate::components::entity_picker::EntitySearchRequest> {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => picker.take_request(),
            _ => None,
        }
    }

    /// Drain the oldest semantic `/remind` request.
    fn take_reminder_request(&mut self) -> Option<crate::reminders::Request> {
        self.view_mut().wants_reminder.pop_front()
    }

    /// Drain `/delegate`, or a document selection's *Delegate…* row.
    ///
    /// `Option` rather than a queue, unlike the reminder above: a delegation
    /// opens a modal that stands over the whole window, so a second request
    /// cannot be made before the first is drained.
    fn take_delegate_request(&mut self) -> Option<crate::view_state::DelegateRequest> {
        self.view_mut().wants_delegate.take()
    }

    /// Ask for the delegation modal from a non-text entry point — the
    /// document's right-click menu over a selection. Writes nothing, so
    /// cancelling has nothing to take back.
    fn request_delegation(&mut self, node: Option<NodeId>, objective: String) {
        self.view_mut().wants_delegate = Some(crate::view_state::DelegateRequest {
            node,
            from_selection: true,
            objective,
        });
    }

    /// Start the same reminder flow from a non-text entry point. Unlike the
    /// slash command this owns no document range, so Cancel and submission
    /// have no text to remove and cannot write the document.
    fn request_reminder(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        if self.body().row(node).is_none() {
            return false;
        }
        self.view_mut()
            .wants_reminder
            .push_back(crate::reminders::Request {
                identity: crate::reminders::RequestIdentity {
                    token: crate::reminders::RequestToken::new(),
                    node,
                },
                phrase: String::new(),
                due_anchor: crate::reminders::due_anchor(ws, node),
            });
        true
    }

    /// Cancel before submission. Person-authored slash text stays ordinary;
    /// only text Koru can prove it synthesized is removed.
    fn cancel_reminder(
        &mut self,
        ws: &mut WorkspaceDoc,
        identity: crate::reminders::RequestIdentity,
    ) -> bool {
        let Some(pending) = self
            .view_mut()
            .pending_reminder_text
            .remove(&identity.token)
            .filter(|pending| pending.identity == identity)
        else {
            return false;
        };
        if pending.origin == crate::reminders::RequestOrigin::PersonAuthored {
            return true;
        }
        self.remove_exact_reminder_text(ws, pending)
    }

    /// Remove exactly the command that has just been submitted. If
    /// collaboration or local edits changed the captured text, preserve it
    /// rather than guessing; the temporary pill still owns the request.
    fn reminder_submitted(
        &mut self,
        ws: &mut WorkspaceDoc,
        identity: crate::reminders::RequestIdentity,
    ) -> bool {
        let Some(pending) = self
            .view_mut()
            .pending_reminder_text
            .remove(&identity.token)
            .filter(|pending| pending.identity == identity)
        else {
            return false;
        };
        self.remove_exact_reminder_text(ws, pending)
    }

    fn remove_exact_reminder_text(
        &mut self,
        ws: &mut WorkspaceDoc,
        pending: crate::reminders::PendingText,
    ) -> bool {
        let Ok((anchor, focus)) = ws.resolve_text_range(&pending.range) else {
            return false;
        };
        let (start, end) = (anchor.min(focus), anchor.max(focus));
        let matches = ws
            .read(pending.identity.node)
            .ok()
            .and_then(|node| node.content.as_text().map(str::to_owned))
            .and_then(|text| text.get(start..end).map(str::to_owned))
            .is_some_and(|text| text == pending.text);
        if !matches || ws.delete_text(pending.identity.node, start..end).is_err() {
            return false;
        }
        self.body_mut().refresh(ws, pending.identity.node);
        self.body_mut().focus_on(pending.identity.node);
        self.place_caret(start);
        self.reflow();
        true
    }

    /// Replace one document's complete batched reminder pills. Missing keys are
    /// removals; unchanged maps do no row refresh or reflow.
    fn set_reminder_chips(
        &mut self,
        reminders: std::collections::HashMap<NodeId, Vec<crate::reminders::InlineChip>>,
    ) -> bool {
        if self.view().reminder_chips == reminders {
            return false;
        }
        let geometry_changed = self.view().reminder_chips.len() != reminders.len()
            || reminders.iter().any(|(node, next)| {
                self.view().reminder_chips.get(node).is_none_or(|old| {
                    old.len() != next.len()
                        || old.iter().zip(next).any(|(old, next)| {
                            old.reminder_id != next.reminder_id || old.label != next.label
                        })
                })
            });
        self.view_mut().reminder_chips = reminders;
        if geometry_changed {
            self.reflow();
        }
        true
    }

    fn take_reminder_management_request(&mut self) -> Option<(NodeId, String)> {
        self.view_mut().wants_reminder_management.take()
    }

    fn take_reminder_context_request(&mut self) -> Option<crate::reminders::ContextRequest> {
        self.view_mut().wants_reminder_context.take()
    }

    /// The exact inline reminder pill under a viewport-space point.
    /// Right-click, hover, left-click, and anchoring all reach the same geometry.
    fn reminder_at(&mut self, x: f32, y: f32) -> Option<(NodeId, crate::reminders::InlineChip)> {
        let cx = x - self.view().content_x;
        let doc_y = y + self.view().scroll;
        let row = self.body().row_at_y(doc_y)?;
        let reminder = self.reminder_at_point(row, cx, doc_y)?;
        let node = self.body().rows.get(row)?.id;
        let chip = self
            .view()
            .reminder_chips
            .get(&node)?
            .get(reminder)?
            .clone();
        Some((node, chip))
    }

    /// Viewport-space anchor for one exact inline reminder pill.
    fn reminder_anchor(
        &mut self,
        node: NodeId,
        reminder_id: Option<&str>,
    ) -> Option<koru_draw::geometry::Rect> {
        let (body, view) = self.parts();
        let row = body.row(node)?;
        let y = row.y - view.scroll;
        let rect = if let Some(reminder_id) = reminder_id {
            let reminders = view.reminder_chips.get(&node)?;
            let index = reminders
                .iter()
                .position(|chip| chip.reminder_id == reminder_id)?;
            crate::draw::reminder_indicator_rects(
                row,
                &mut view.engine,
                &view.tags,
                &view.lists,
                reminders,
                view.view_mode.effective_w(view.viewport.0),
                y,
            )
            .get(index)
            .copied()?
        } else {
            crate::draw::reminder_flow_anchor(
                row,
                &mut view.engine,
                &view.tags,
                &view.lists,
                view.view_mode.effective_w(view.viewport.0),
                y,
            )?
        };
        Some(koru_draw::geometry::Rect::new(
            view.content_x + rect.x,
            rect.y,
            rect.w,
            rect.h,
        ))
    }

    fn set_entity_search_results(
        &mut self,
        hits: Vec<crate::components::entity_picker::DocumentChoice>,
        unavailable: bool,
    ) -> bool {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => {
                picker.set_results(hits, unavailable);
                true
            }
            _ => false,
        }
    }

    /// Every document an open row deep-links into.
    ///
    /// Read back out of the caption keys rather than the attributes: the key is
    /// already the editor's own vocabulary for "which entity is this pill", and
    /// asking it needs no `WorkspaceDoc`.
    fn deep_link_documents(&mut self) -> Vec<koru_core::DocumentId> {
        let mut out = Vec::new();
        for row in &self.body().rows {
            for info in row.pills.values() {
                let Some(key) = info.entity.as_deref() else {
                    continue;
                };
                if !key.contains('#') {
                    continue;
                }
                if let Some(id) = koru_core::DocumentId::parse(crate::pillinfo::document_of(key))
                    && !out.contains(&id)
                {
                    out.push(id);
                }
            }
        }
        out
    }

    /// **Every delegated run an open row shows a pill for** (D8).
    ///
    /// Read out of the pill infos rather than the attributes, exactly as
    /// [`deep_link_documents`](Self::deep_link_documents) is and for the same
    /// reason: the caption key is already this layer's vocabulary for "which
    /// entity is this pill", and asking it needs no `WorkspaceDoc`. The
    /// container uses it to decide which runs are worth asking about — a run
    /// nobody can see is a run nobody fetches.
    fn run_pill_documents(&mut self) -> Vec<koru_core::DocumentId> {
        let mut out = Vec::new();
        for row in &self.body().rows {
            for info in row.pills.values() {
                if info.kind != koru_draw::pill::PillKind::Run {
                    continue;
                }
                let Some(key) = info.entity.as_deref() else {
                    continue;
                };
                if let Some(id) = koru_core::DocumentId::parse(key)
                    && !out.contains(&id)
                {
                    out.push(id);
                }
            }
        }
        out
    }

    /// The document whose headings the picker is waiting for, if any.
    fn take_heading_request(&mut self) -> Option<koru_core::DocumentId> {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => picker.take_heading_request(),
            _ => None,
        }
    }

    /// Hand the picker one document's headings.
    fn set_headings(
        &mut self,
        document: koru_core::DocumentId,
        headings: Vec<crate::components::entity_picker::HeadingChoice>,
        projected: bool,
    ) -> bool {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => picker.set_headings(document, headings, projected),
            _ => false,
        }
    }

    /// Name the document the window is in, so a bare `@#` can offer its own
    /// headings. Absent when there is nothing open to link into.
    fn set_entity_current_document(
        &mut self,
        current: Option<crate::components::entity_picker::DocumentChoice>,
    ) -> bool {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => {
                let changed = picker.current_document != current;
                picker.current_document = current;
                changed
            }
            _ => false,
        }
    }

    /// Refresh routed document-pill captions from manifest-derived titles.
    /// This changes only local layout caches; the reference itself remains the
    /// sole collaborative value and no rename writes through every backlink.
    ///
    /// `live` is every document id still in the workspace (including trash). A
    /// file pill whose entity is absent from it is gone permanently and draws
    /// broken; one in `trashed` draws the trash treatment.
    fn set_entity_titles(
        &mut self,
        titles: &std::collections::HashMap<String, String>,
        trashed: &std::collections::HashSet<String>,
        live: &std::collections::HashSet<String>,
    ) -> bool {
        let active = self.view().active;
        let mut changed = false;
        for row in &mut self.body_mut().rows {
            let mut row_changed = false;
            for info in row.pills.values_mut() {
                let Some(key) = info.entity.as_ref() else {
                    continue;
                };
                if let Some(title) = titles.get(key)
                    && info.label != *title
                {
                    info.label = title.clone();
                    row_changed = true;
                }
                // **Trashed belongs to the document, not to the block.** The
                // caption key may name both; the bin never does, so a deep
                // link into a binned document would have read as live.
                let doc = crate::pillinfo::document_of(key);
                let is_trashed = trashed.contains(doc);
                if info.trashed != is_trashed {
                    info.trashed = is_trashed;
                    row_changed = true;
                }
                // File pills: gone permanently when the entity is not live.
                // Document pills keep their previous dangling rules.
                if info.kind == koru_draw::pill::PillKind::File {
                    let is_dangling = !live.contains(doc);
                    if info.dangling != is_dangling {
                        info.dangling = is_dangling;
                        row_changed = true;
                    }
                }
            }
            if row_changed {
                row.dirty = true;
                changed = true;
            }
        }
        if changed {
            // The pill cache must *not* be cleared here: only the changed rows
            // are dirtied, and a cleared visual in an unchanged row is never
            // re-measured — reflow rebuilds dirty rows only, so its pill
            // silently stops drawing while the inline box keeps its width. A
            // renamed caption is a new cache key and measures fresh on its own.
            if active {
                self.reflow();
            }
        }
        changed
    }

    /// Refresh the live name and lifecycle icon of schedule pills from the
    /// already-watched schedule catalog. This mutates only retained layout
    /// state; the ScheduleId remains the sole collaborative identity.
    fn set_schedule_pills(
        &mut self,
        schedules: &std::collections::HashMap<String, (String, bool)>,
    ) -> bool {
        let active = self.view().active;
        let mut changed = false;
        for row in &mut self.body_mut().rows {
            let mut row_changed = false;
            for info in row.pills.values_mut() {
                if !matches!(
                    info.kind,
                    koru_draw::pill::PillKind::Schedule | koru_draw::pill::PillKind::SchedulePaused
                ) {
                    continue;
                }
                let Some(key) = info.entity.as_ref() else {
                    continue;
                };
                let Some((name, paused)) = schedules.get(key) else {
                    continue;
                };
                let kind = if *paused {
                    koru_draw::pill::PillKind::SchedulePaused
                } else {
                    koru_draw::pill::PillKind::Schedule
                };
                if info.label != *name {
                    info.label.clone_from(name);
                    row_changed = true;
                }
                if info.kind != kind {
                    info.kind = kind;
                    row_changed = true;
                }
            }
            if row_changed {
                row.dirty = true;
                changed = true;
            }
        }
        if changed && active {
            self.reflow();
        }
        changed
    }

    fn take_wants_new_page(&mut self) -> Option<String> {
        self.view_mut().wants_new_page.take()
    }

    /// Complete an entity-picker create after the shell returns the real
    /// document id. The query is left authored until this point: persisting a
    /// placeholder id would create a collaborative dangling reference.
    fn complete_new_page_link(
        &mut self,
        ws: &mut WorkspaceDoc,
        title: String,
        target: crate::trigger::DocumentTarget,
    ) -> bool {
        let Some(trigger) = self.view_mut().pending_new_page_trigger.take() else {
            return false;
        };
        self.commit_pill(
            ws,
            trigger,
            crate::trigger::PillSpec::document(title, target),
        );
        self.reflow();
        self.scroll_to_caret();
        true
    }

    fn take_wants_entity_open(&mut self) -> Option<(crate::trigger::DocumentTarget, OpenWhere)> {
        self.view_mut().wants_entity_open.take()
    }

    // ------------------------------------------- moving a selection out

    /// Whether there is a selection a move could take: marked rows, or a text
    /// range within one row.
    fn has_move_selection(&self) -> bool {
        self.body().has_row_selection()
            || self
                .body()
                .focused()
                .is_some_and(|row| !row.range().is_empty())
    }

    /// Open the document picker in move mode over the current selection.
    ///
    /// The payload is captured **now** — the ids of the selection roots, or
    /// the selected runs — so what moves is what was selected when the picker
    /// opened, not whatever the selection is by the time a destination is
    /// chosen. Nothing in the document changes here; the source is edited only
    /// in [`complete_selection_move`], after the destination write succeeded.
    ///
    /// [`complete_selection_move`]: Editing::complete_selection_move
    fn open_move_selection_picker(&mut self, ws: &WorkspaceDoc) -> bool {
        use crate::view_state::{MoveSelectionPayload, PendingSelectionMove};
        if self.view().overlay.is_open() {
            return false;
        }
        if self.body().has_row_selection() {
            let roots = self.body().selection_roots(ws);
            let Some(&first) = roots.first() else {
                return false;
            };
            // What the picker opens holding: the **first selected block's**
            // text. A partial-selection slice was tried first and read as a
            // torn-off fragment — the first block is a line somebody wrote as
            // a line, which is what a title wants to be.
            let default_title = move_default_title(
                ws.read(first)
                    .ok()
                    .and_then(|n| n.content.as_text().map(str::to_owned))
                    .unwrap_or_default(),
            );
            let prefill = if default_title == "Untitled" {
                // A blank starting row prefills nothing — an empty field
                // beats a literal "Untitled" the person has to delete.
                String::new()
            } else {
                default_title.clone()
            };
            let trigger = Trigger {
                kind: TriggerKind::At,
                start: 0,
                node: first,
                anchor: Anchor::Insert,
                stable: None,
            };
            self.view_mut().pending_selection_move = Some(PendingSelectionMove {
                payload: MoveSelectionPayload::Rows(roots),
                trigger: None,
                default_title,
            });
            let view = self.view_mut();
            view.overlay =
                Overlay::Entity(EntityPicker::for_move(&mut view.engine, trigger, &prefill));
            return true;
        }
        let Some((node, range)) = self.target() else {
            return false;
        };
        if range.is_empty() {
            return false;
        }
        let runs = crate::clipboard::runs_in(&ws.runs(node).unwrap_or_default(), &range);
        let attrs: Vec<koru_core::node::Attr> = ws
            .read(node)
            .map(|n| {
                n.content
                    .attrs()
                    .iter()
                    .filter(|attr| runs.iter().any(|run| run.pill == Some(attr.id)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let default_title = move_default_title(
            self.body()
                .focused()
                .map(|row| row.text[range.clone()].to_owned())
                .unwrap_or_default(),
        );
        let trigger = Trigger {
            kind: TriggerKind::At,
            start: range.start,
            node,
            anchor: Anchor::Selection(range.end),
            stable: ws.capture_text_range(node, range.start, range.end).ok(),
        };
        let prefill = if default_title == "Untitled" {
            String::new()
        } else {
            default_title.clone()
        };
        self.view_mut().pending_selection_move = Some(PendingSelectionMove {
            payload: MoveSelectionPayload::Inline { runs, attrs },
            trigger: Some(trigger.clone()),
            default_title,
        });
        let view = self.view_mut();
        view.overlay = Overlay::Entity(EntityPicker::for_move(&mut view.engine, trigger, &prefill));
        true
    }

    fn take_wants_move_selection(
        &mut self,
    ) -> Option<(
        crate::view_state::MoveSelectionDestination,
        crate::view_state::MoveSelectionPayload,
    )> {
        self.view_mut().wants_move_selection.take()
    }

    /// Whether the open picker is a move-mode one with nothing typed — the
    /// state whose result list is the window's **recents**, fed by the
    /// container. The store's search service only remembers search-driven
    /// navigations, so its empty-query answer is not the list a person means
    /// by "recent".
    fn entity_wants_recents(&self) -> bool {
        matches!(
            &self.view().overlay,
            Overlay::Entity(picker) if picker.move_mode && picker.document_query.trim().is_empty()
        )
    }

    /// Feed the window's recents into a move-mode picker with an empty query.
    /// `false` when nothing changed — the caller runs per gesture, and an
    /// unconditional "changed" would spin the redraw loop.
    fn set_entity_recents(
        &mut self,
        hits: Vec<crate::components::entity_picker::DocumentChoice>,
    ) -> bool {
        match &mut self.view_mut().overlay {
            Overlay::Entity(picker) => picker.set_recent_results(hits),
            _ => false,
        }
    }

    /// The destination write failed or was abandoned; nothing later may delete
    /// the selection whose copy never landed.
    fn cancel_selection_move(&mut self) -> bool {
        self.view_mut().pending_selection_move.take().is_some()
    }

    /// The destination write succeeded — replace the selection with its trail
    /// pill. `false` means the source was left alone (the pending move is
    /// gone, or the rows no longer resolve): the content is then duplicated
    /// rather than deleted, which is the only acceptable failure direction.
    fn complete_selection_move(
        &mut self,
        ws: &mut WorkspaceDoc,
        title: String,
        target: crate::trigger::DocumentTarget,
    ) -> bool {
        use crate::view_state::MoveSelectionPayload;
        let Some(pending) = self.view_mut().pending_selection_move.take() else {
            return false;
        };
        match pending.payload {
            MoveSelectionPayload::Rows(roots) => {
                let live: Vec<NodeId> = roots
                    .into_iter()
                    .filter(|id| ws.parent(*id).is_ok())
                    .collect();
                if live.is_empty() {
                    self.view_mut().notice = Some(
                        "that selection changed elsewhere; a copy was made, nothing moved".into(),
                    );
                    return false;
                }
                let attr = Attr::labelled(
                    "document",
                    AttrValue::Document(koru_core::DocumentReference {
                        document: target.document,
                        node: target.node,
                    }),
                    title,
                );
                ws.checkpoint();
                match ws.replace_subtrees_with_pill(&live, &attr) {
                    Ok(replacement) => {
                        self.body_mut().rebuild(ws);
                        self.body_mut().focus_on(replacement);
                        self.reflow();
                        self.settle();
                        true
                    }
                    Err(_) => {
                        self.view_mut().notice =
                            Some("couldn't leave the link; a copy was made, nothing moved".into());
                        false
                    }
                }
            }
            MoveSelectionPayload::Inline { .. } => {
                let Some(trigger) = pending.trigger else {
                    return false;
                };
                self.commit_pill(
                    ws,
                    trigger,
                    crate::trigger::PillSpec::document(title, target),
                );
                self.reflow();
                self.scroll_to_caret();
                true
            }
        }
    }

    /// Divert a move-mode picker's outcome to the move machinery — a commit
    /// there chooses a **destination**, never a pill to insert. `Ok(())` means
    /// handled; `Err` hands the outcome back for ordinary processing.
    fn route_move_outcome(&mut self, outcome: Outcome) -> Result<(), Outcome> {
        use crate::view_state::MoveSelectionDestination;
        let (follow, child) = match &self.view().overlay {
            Overlay::Entity(picker) if picker.move_mode => {
                (!picker.stay_on_commit, picker.child_on_commit)
            }
            _ => return Err(outcome),
        };
        match outcome {
            Outcome::Commit(spec) => {
                let (Some(target), Some(pending)) =
                    (spec.document, self.view().pending_selection_move.as_ref())
                else {
                    // A move-mode commit always carries a document target and a
                    // pending payload; anything else closes with nothing moved.
                    self.view_mut().overlay = Overlay::None;
                    self.view_mut().pending_selection_move = None;
                    return Ok(());
                };
                let payload = pending.payload.clone();
                // The trail pill's caption; an untitled destination still
                // gets a readable one, never an empty chip.
                let title = match spec.label.trim() {
                    "" => "Untitled".to_owned(),
                    label => label.to_owned(),
                };
                self.view_mut().overlay = Overlay::None;
                self.view_mut().wants_move_selection = Some((
                    MoveSelectionDestination::Existing { target, title },
                    payload,
                ));
                Ok(())
            }
            Outcome::NewPage(typed) => {
                let Some(pending) = self.view().pending_selection_move.as_ref() else {
                    self.view_mut().overlay = Overlay::None;
                    return Ok(());
                };
                let payload = pending.payload.clone();
                let title = match typed.trim() {
                    "" => pending.default_title.clone(),
                    typed => typed.to_owned(),
                };
                // `move_default_title` never answers an empty string, but the
                // pill's caption is worth guarding at the last door too.
                let title = if title.trim().is_empty() {
                    "Untitled".to_owned()
                } else {
                    title
                };
                self.view_mut().overlay = Overlay::None;
                self.view_mut().wants_move_selection = Some((
                    MoveSelectionDestination::NewPage {
                        title,
                        follow,
                        child,
                    },
                    payload,
                ));
                Ok(())
            }
            Outcome::Close => {
                self.view_mut().overlay = Overlay::None;
                self.view_mut().pending_selection_move = None;
                Ok(())
            }
            other => Err(other),
        }
    }

    /// Adopt a document that changed underneath us — a remote edit arriving, or
    /// another window's typing. Patching only the rows that changed is always
    /// enough: rows are a cache, the caret is re-anchored by node id, and a
    /// structural change falls back to a full rebuild internally.
    ///
    /// Two steps, and the split is the point: **draining** the change tracker is
    /// per *document* and every view of it owes it, while **settling** is per
    /// *view* and only the one about to be drawn owes it. A container drains all
    /// its tabs itself and settles one; a single-view window does both here.
    /// See [`catch_up`](Self::catch_up).
    fn adopt(&mut self, ws: &WorkspaceDoc) {
        self.body_mut().defer_pending(ws);
        self.catch_up(ws);
    }

    /// Settle a view whose **rows changed underneath it**: re-anchor an open
    /// overlay against the merged CRDT, patch the rows the tracker named, put
    /// the caret back, lay out, and re-filter the picker behind the caret.
    ///
    /// **This is the one place that behaviour lives, and both hosts reach it.**
    /// The single-view window (`koru-document`, and a popped-out `DocumentPane`)
    /// arrives from [`adopt`](Self::adopt); the tab container arrives from
    /// `TabView::catch_up`, for the tab about to be drawn. It used to live
    /// *inside* `adopt` alone, and the container does not call `adopt` — so a
    /// document open in a tab kept an overlay hanging from a node a peer had
    /// deleted, while the same document popped out into its own window closed
    /// it. Two hosts, one editor, two behaviours (Q-B4, ADR-343-overlaycatchup).
    ///
    /// **The order is load-bearing**, in both directions:
    ///
    /// 1. Resolve the overlay's `stable` endpoints **before** the row cache is
    ///    replaced. `Trigger::start` is a byte offset and therefore a rendering
    ///    cache; the Loro range is the authority, and a peer inserting in front
    ///    of it would otherwise leave the picker pointing at unrelated text.
    /// 2. Patch the rows the tracker named ([`Body::catch_up`]).
    /// 3. Put the caret where the resolved anchor says — or, when the anchor no
    ///    longer resolves at all, **close the overlay** with its notice. Closing
    ///    is the deliberately *visible* failure: an overlay that vanishes is
    ///    recoverable, an overlay whose anchor silently names other people's
    ///    text is not (`AGENTS.md`, "Losing content is the one unrecoverable
    ///    bug").
    /// 4. Lay out — [`place_caret`](crate::Paragraph::place_caret) only *queues*
    ///    a caret, and `reflow` is what applies it.
    /// 5. Re-filter the picker, which reads `Paragraph::caret()` and so cannot
    ///    run before step 4 without judging the query at the old offset.
    ///
    /// It does **not** drain the change tracker. That is [`Body::defer_pending`],
    /// which a container has already run for *every* tab by the time it settles
    /// one — see `MainView::adopt`. Nor does it ask whether anything is owed:
    /// the container guards with [`Body::is_stale`] before calling, so an idle
    /// frame costs a bool, and `adopt` deliberately does not, because a
    /// single-view window has no cheaper question to ask.
    ///
    /// [`Body::catch_up`]: Body::catch_up
    /// [`Body::defer_pending`]: Body::defer_pending
    /// [`Body::is_stale`]: Body::is_stale
    fn catch_up(&mut self, ws: &WorkspaceDoc) {
        // Resolve overlay endpoints against the merged CRDT before replacing
        // the row cache. A byte offset is only a rendering detail and would
        // otherwise point at unrelated text after a peer inserts before it.
        let anchored = self.view_mut().overlay.trigger_mut().and_then(|trigger| {
            let stable = trigger.stable.as_ref()?;
            let (anchor, focus) = ws.resolve_text_range(stable).ok()?;
            trigger.start = anchor.min(focus);
            if matches!(trigger.anchor, Anchor::Selection(_)) {
                trigger.anchor = Anchor::Selection(anchor.max(focus));
            }
            Some((trigger.node, focus, trigger.anchor))
        });
        self.body_mut().catch_up(ws);
        if let Some((node, focus, Anchor::TriggerText)) = anchored {
            self.body_mut().focus_on(node);
            if let Some(row) = self.body_mut().focused_mut() {
                row.place_caret(focus.min(row.text.len()));
            }
        } else if self
            .view()
            .overlay
            .trigger()
            .is_some_and(|trigger| trigger.stable.is_some() && anchored.is_none())
        {
            self.view_mut().overlay = Overlay::None;
            self.view_mut().notice = Some("that insertion point changed elsewhere".into());
        }
        self.reflow();
        let (body, view) = self.parts();
        view.overlay.refresh(body);
    }

    // ------------------------------------------------------------ layout

    fn set_viewport(&mut self, w: f32, h: f32) {
        let mode = self.view().view_mode;
        // Compare *effective* widths, not raw ones: in Page/Narrow the rows wrap
        // to the page width, so growing the window past it leaves the wrap
        // unchanged and there is nothing to invalidate. This is also what keeps
        // a live resize cheap in a page mode — only a change that actually
        // moves the wrap pays for a reflow.
        let old = mode.effective_w(self.view().viewport.0);
        let new = mode.effective_w(w);
        if (old - new).abs() > 0.5 {
            self.body_mut().invalidate();
        }
        self.view_mut().viewport = (w, h);
        self.view_mut().content_x = mode.content_x(w);
        self.reflow();
    }

    /// Switch the content layout mode. Acts like a width change: the rows are
    /// invalidated only when the *effective* wrap width moves, and `content_x`
    /// is recomputed so the column centres again. A no-op to call from the
    /// outline's editor — its mode stays `Full`.
    fn set_view_mode(&mut self, mode: ViewMode) {
        let vw = self.view().viewport.0;
        let old = self.view().view_mode.effective_w(vw);
        let new = mode.effective_w(vw);
        if (old - new).abs() > 0.5 {
            self.body_mut().invalidate();
        }
        self.view_mut().view_mode = mode;
        self.view_mut().content_x = mode.content_x(vw);
        self.reflow();
    }

    // -------------------------------------------------------- smooth scroll
    //
    // Page Up/Down animates the scroll rather than jumping. `view.scroll` *is*
    // the animated value: it starts at `from` and is advanced toward `target`
    // each frame by `advance_scroll_anim`, which the container calls from its
    // draw loop — the frame the shell asked for because `is_scroll_animating`
    // said so (ADR-26).

    /// Begin a smooth scroll toward `target`, clamped to the content. A scroll
    /// already in flight is replaced — the new animation starts from wherever
    /// the visual scroll currently is.
    fn begin_scroll_to(&mut self, target: f32) {
        let content_h = rowlayout::content_height(self.body());
        let max = (content_h - self.view().content_viewport_h()).max(0.0);
        let target = target.clamp(0.0, max);
        let from = self.view().scroll;
        if (target - from).abs() < 0.5 {
            self.view_mut().scroll_anim = None;
            return;
        }
        self.view_mut().scroll_anim = Some(ScrollAnim {
            from,
            target,
            started: Instant::now(),
        });
    }

    /// Advance the smooth scroll one frame. Returns whether it moved. When the
    /// animation reaches its deadline it snaps to the target and clears itself,
    /// so [`is_scroll_animating`](Self::is_scroll_animating) returns to `false`.
    fn advance_scroll_anim(&mut self, now: Instant) -> bool {
        let (body, view) = self.parts();
        let (from, target, started) = match &view.scroll_anim {
            Some(a) => (a.from, a.target, a.started),
            None => return false,
        };
        let t = (now.duration_since(started).as_secs_f32() / SCROLL_ANIM_SECS).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        view.scroll = from + (target - from) * eased;
        if t >= 1.0 {
            view.scroll = target;
            view.scroll_anim = None;
        }
        let content_h = rowlayout::content_height(body);
        view.clamp_scroll(content_h);
        true
    }

    /// Whether scrolling still needs frame ticks: either a smooth page movement
    /// or a live selection held at a document edge (ADR-26).
    fn is_scroll_animating(&self) -> bool {
        self.view().scroll_anim.is_some() || self.drag_autoscroll_velocity() != 0.0
    }

    /// Signed scroll velocity for a live selection drag, or a row drag, at a
    /// viewport edge. A block dragged to the bottom of the window used to
    /// stop there: the page did not scroll, so it could not be moved below
    /// what was in view.
    fn drag_autoscroll_velocity(&self) -> f32 {
        if !self.view().dragging && self.view().drag_row.is_none() {
            return 0.0;
        }
        let view = self.view();
        let top = view.chrome_h;
        let bottom = view.chrome_h + view.content_viewport_h();
        let y = view.pointer.1;
        let velocity = if y < top + DRAG_SCROLL_EDGE {
            -((top + DRAG_SCROLL_EDGE - y) / DRAG_SCROLL_EDGE).clamp(0.0, 1.5)
                * DRAG_SCROLL_PX_PER_SEC
        } else if y > bottom - DRAG_SCROLL_EDGE {
            ((y - (bottom - DRAG_SCROLL_EDGE)) / DRAG_SCROLL_EDGE).clamp(0.0, 1.5)
                * DRAG_SCROLL_PX_PER_SEC
        } else {
            0.0
        };
        let content_h = rowlayout::content_height(self.body());
        let max = (content_h - view.content_viewport_h()).max(0.0);
        if (velocity < 0.0 && view.scroll <= 0.0) || (velocity > 0.0 && view.scroll >= max) {
            0.0
        } else {
            velocity
        }
    }

    /// Advance direct-manipulation scrolling while a selection is dragged at
    /// an edge, then extend the selection under the stationary pointer in the
    /// newly exposed document space.
    fn advance_drag_autoscroll(&mut self, now: Instant) -> bool {
        let velocity = self.drag_autoscroll_velocity();
        if velocity == 0.0 {
            self.view_mut().drag_scroll_tick = None;
            return false;
        }
        let last = self.view_mut().drag_scroll_tick.replace(now);
        let dt = last
            .map(|then| now.saturating_duration_since(then).as_secs_f32())
            .unwrap_or(1.0 / 60.0)
            .min(0.05);
        let content_h = rowlayout::content_height(self.body());
        let old = self.view().scroll;
        {
            let view = self.view_mut();
            view.scroll_anim = None;
            view.scroll += velocity * dt;
            view.clamp_scroll(content_h);
        }
        if (self.view().scroll - old).abs() < f32::EPSILON {
            return false;
        }
        let (x, y) = self.view().pointer;
        let cx = x - self.view().content_x;
        let doc_y = y + self.view().scroll;
        // A row drag: the drop target follows the pointer into the newly
        // exposed rows, the same way `mouse_move` tracks it.
        if self.view().drag_row.is_some() {
            let to = self
                .body_mut()
                .row_near_y(doc_y)
                .max(self.body().first_body_row());
            let drag = self.view_mut().drag_row.as_mut().expect("checked above");
            let changed = drag.to != to;
            drag.to = to;
            return true || changed;
        }
        self.drag_selection_to(cx, doc_y)
    }

    /// Vertical space this view's chrome takes above the rows.
    ///
    /// A plain field rather than a question about which view this is: the view
    /// knows its own furniture, and the body only needs the number.
    fn top_inset(&self) -> f32 {
        let base = self.view().chrome_h + self.body().top_inset();
        // In Page mode the sheet sits a grey `PAGE_GAP` below the toolbar, and
        // the rows sit inside it — so the first line is `PAGE_GAP` (canvas) +
        // `MARGIN_TOP` (inside-page padding) below the chrome.
        match self.view().view_mode {
            ViewMode::Page => base + self.view().page_gap_top,
            ViewMode::Narrow | ViewMode::Full => base,
        }
    }

    fn reflow(&mut self) {
        let _watch = koru_core::timing::Watch::start("editor.reflow");
        let inset = self.top_inset();
        let (body, view) = self.parts();
        // See `ViewState::reflows`: the responsiveness rules are about how often
        // this is called, and nothing else this function leaves behind records
        // that.
        view.reflows = view.reflows.saturating_add(1);
        for row in &mut body.rows {
            row.table_source_visible = row.kind == Kind::Table
                && (view.table_source == Some(row.id)
                    || row.table.as_ref().is_some_and(|table| table.grid.is_none()));
        }
        // The same mirror for diagram rows — what each shows this frame — and
        // the requests for any text the view has no answer for yet. One hash
        // and one lookup per diagram row; nothing is laid out here.
        crate::diagram::sync(body, view);
        // Rows wrap to the page width in Page/Narrow, not the raw viewport —
        // see `set_viewport` for why this is the effective width.
        let layout_w = view.view_mode.effective_w(view.viewport.0);
        let (scroll, viewport_h) = (view.scroll, view.viewport.1);
        rowlayout::reflow(&mut view.engine, body, layout_w, inset, scroll, viewport_h);
        crate::draw::reserve_reminder_space(
            body,
            &mut view.engine,
            &view.tags,
            &view.lists,
            &view.reminder_chips,
            layout_w,
        );
        // Every row applies what it queued, not just the focused one — a row
        // that asked for a caret gets it even if the focus moved on.
        for row in &mut body.rows {
            row.apply_pending();
        }
        let content_h = rowlayout::content_height(body);
        view.clamp_scroll(content_h);
        // Cached so `cursor_icon` can settle the scrollbar without the body.
        view.content_h = content_h;
        // Cache the focused image's rect too, so the resize handles can be
        // hit-tested for the cursor without reaching into the rows.
        view.focused_image = body
            .rows
            .get(body.focus)
            .filter(|r| r.image.is_some())
            .map(|r| {
                let (x, w, _) = rowlayout::image_display_box(
                    r.image.as_ref().expect("image row"),
                    r.align,
                    layout_w,
                );
                (r.id, Rect::new(x, r.y, w, r.height))
            });
    }

    /// Shape row `i` if it is still dirty, then re-position. Navigation to a
    /// row outside the visible band needs its real height and caret geometry
    /// before it can compute where to scroll — the banded reflow leaves such a
    /// row as an estimate until it is actually seen.
    fn shape_row_now(&mut self, i: usize) {
        let inset = self.top_inset();
        let (body, view) = self.parts();
        let Some(row) = body.rows.get_mut(i) else {
            return;
        };
        if !row.dirty {
            return;
        }
        let layout_w = view.view_mode.effective_w(view.viewport.0);
        rowlayout::build_row(&mut view.engine, row, layout_w);
        let (scroll, viewport_h) = (view.scroll, view.viewport.1);
        rowlayout::reflow(&mut view.engine, body, layout_w, inset, scroll, viewport_h);
        crate::draw::reserve_reminder_space(
            body,
            &mut view.engine,
            &view.tags,
            &view.lists,
            &view.reminder_chips,
            layout_w,
        );
    }

    /// Scroll so the caret stays on screen.
    ///
    /// A no-op when no line is active: there is no caret to keep in view, and
    /// running it anyway would yank a page-pan (which is meant to leave the
    /// viewport where it is) back to wherever the last caret happened to be.
    ///
    /// While a smooth scroll is in flight, the caret's visibility is checked
    /// against the animation's **target** and the target is nudged rather than
    /// the animation cancelled — `move_page` leaves the caret a few pixels above
    /// the margin by design, and snapping for that would kill the scroll.
    fn scroll_to_caret(&mut self) {
        if !self.view().active {
            return;
        }
        self.shape_row_now(self.body().focus);
        let (body, view) = self.parts();
        let Some(row) = body.focused() else {
            return;
        };
        // Window-space band, same as `scroll_row_into_view`: a document's rows
        // sit under chrome, so measuring against `MARGIN_TOP` alone leaves the
        // caret "in view" while it is still under the caption — which is what
        // a large paste looked like before the end of the paste scrolled in.
        let band_top = view.chrome_h + theme::MARGIN_TOP;
        let band_bottom = view.viewport.1 - view.status_h - 8.0;
        let geo = rowlayout::caret_geometry(row, &view.engine.pills, 1.0);
        let top = row.y + geo.y0 as f32;
        let bottom = row.y + geo.y1 as f32;
        let content_h = rowlayout::content_height(body);
        let max = (content_h - view.content_viewport_h()).max(0.0);
        let scroll = match &view.scroll_anim {
            Some(a) => a.target,
            None => view.scroll,
        };
        if top - scroll < band_top {
            let new_scroll = (top - band_top).max(0.0).min(max);
            match &mut view.scroll_anim {
                Some(a) => a.target = new_scroll,
                None => view.scroll = new_scroll,
            }
        } else if bottom - scroll > band_bottom {
            let new_scroll = (bottom - band_bottom).clamp(0.0, max);
            match &mut view.scroll_anim {
                Some(a) => a.target = new_scroll,
                None => view.scroll = new_scroll,
            }
        }
    }

    /// Scroll so row `i` is on screen, leaving it where it is if it already is.
    ///
    /// Works from the **row's own box** rather than the caret's, which is what
    /// makes it correct for an image: `rowlayout::build_row` returns early for
    /// image rows, so their parley `Layout` is empty and the caret rect inside
    /// it is degenerate — [`scroll_to_caret`](Self::scroll_to_caret) would put a
    /// tall picture's top edge at the *bottom* of the band and show a sliver.
    ///
    /// A row taller than the band is aligned to its top, because the start of a
    /// tall picture is the part that was meant.
    fn scroll_row_into_view(&mut self, i: usize) {
        self.shape_row_now(i);
        let (body, view) = self.parts();
        let Some(row) = body.rows.get(i) else {
            return;
        };
        // Worked in **window** space, where the band actually is. A document's
        // rows begin `chrome_h` down, under its toolbar, so measuring the target
        // against `MARGIN_TOP` alone would scroll the row up behind the strip
        // and call it visible.
        let band_top = view.chrome_h + theme::MARGIN_TOP;
        let band_bottom = view.viewport.1 - view.status_h - 8.0;
        let content_h = rowlayout::content_height(body);
        let max = (content_h - view.content_viewport_h()).max(0.0);
        // Adjust the animation target rather than cancelling, as
        // `scroll_to_caret` does.
        let scroll = match &view.scroll_anim {
            Some(a) => a.target,
            None => view.scroll,
        };
        let top = row.y - scroll;
        let bottom = top + row.height;
        // Aligning a too-tall row to its top rather than its bottom: the start
        // of a tall picture is the part that was meant.
        let too_tall = row.height > band_bottom - band_top;
        if too_tall || top < band_top {
            let new_scroll = (row.y - band_top).max(0.0).min(max);
            match &mut view.scroll_anim {
                Some(a) => a.target = new_scroll,
                None => view.scroll = new_scroll,
            }
        } else if bottom > band_bottom {
            let new_scroll = (row.y + row.height - band_bottom).clamp(0.0, max);
            match &mut view.scroll_anim {
                Some(a) => a.target = new_scroll,
                None => view.scroll = new_scroll,
            }
        }
    }

    /// Scroll row `i` to the middle of the usable document band.
    ///
    /// Search uses this stronger landing rule: a match is easier to review
    /// with context both above and below it. Normal document/explore navigation
    /// continues to use [`scroll_row_into_view`](Self::scroll_row_into_view),
    /// which deliberately avoids moving an already-visible row.
    fn center_row_in_view(&mut self, i: usize) {
        self.shape_row_now(i);
        let (body, view) = self.parts();
        let Some(row) = body.rows.get(i) else {
            return;
        };
        let band_top = view.chrome_h + theme::MARGIN_TOP;
        let band_bottom = view.viewport.1 - theme::STATUS_H - 8.0;
        let band_height = (band_bottom - band_top).max(0.0);
        let content_h = rowlayout::content_height(body);
        let max = (content_h - view.content_viewport_h()).max(0.0);
        let target = if row.height >= band_height {
            row.y - band_top
        } else {
            row.y + row.height / 2.0 - (band_top + band_bottom) / 2.0
        };
        // Opening a search result is a discrete navigation, not an animated
        // page movement. An old animation must not pull it away next frame.
        view.scroll_anim = None;
        view.scroll = target.clamp(0.0, max);
    }

    /// Focus `id` and scroll it into view. `false` if it is not a row here.
    ///
    /// What the explore column needs: an image or a paragraph is a *place in a
    /// document*, so clicking one opens the document and comes here to land on
    /// it.
    ///
    /// The sequence is the one `BuiltInDocument::append_point` already uses, and
    /// `reflow` is not optional in the middle of it — `row.y` and `row.height`
    /// are only meaningful after it, and it is what applies the queued caret.
    fn reveal(&mut self, id: NodeId) -> bool {
        let Some(i) = self.body().index_of(id) else {
            return false;
        };
        self.body_mut().clear_selection();
        self.body_mut().focus = i;

        // Without this `scroll_to_caret`'s sibling checks would still run, but
        // more importantly a revealed row with no active line draws no caret and
        // reads as though nothing happened.
        self.view_mut().active = true;
        self.view_mut().desired_x = None;
        // An image is *selected*, not edited — the same distinction `mouse_down`
        // makes when a click lands on one. Placing a caret in its empty text
        // would put one nowhere and take the resize furniture away.
        if self.body().rows[i].image.is_none() {
            self.place_caret(0);
        }
        self.reflow();
        self.scroll_row_into_view(i);
        true
    }

    /// Focus `id` and place it in the middle of the document viewport.
    ///
    /// Kept separate from [`reveal`](Self::reveal) so callers opt into the
    /// search-result experience without changing ordinary navigation.
    fn reveal_centered(&mut self, id: NodeId) -> bool {
        if !self.reveal(id) {
            return false;
        }
        let i = self.body().focus;
        self.center_row_in_view(i);
        true
    }

    /// Land on authored search evidence without turning the cue into a text
    /// selection. Typing immediately therefore inserts before the match rather
    /// than replacing it, and nothing here touches the CRDT.
    fn reveal_search_match(&mut self, id: NodeId, match_text: &str) -> bool {
        if !self.reveal(id) {
            return false;
        }
        if match_text.is_empty() {
            let range = self
                .body()
                .row(id)
                .map(|row| 0..row.text.len())
                .unwrap_or(0..0);
            let i = self.body().focus;
            self.center_row_in_view(i);
            self.view_mut().search_flash = Some(SearchFlash {
                node: id,
                range,
                until: Instant::now() + SEARCH_FLASH_DURATION,
            });
            return true;
        }
        let Some(start) = self
            .body()
            .row(id)
            .and_then(|row| row.text.find(match_text))
        else {
            let i = self.body().focus;
            self.center_row_in_view(i);
            return true;
        };
        let range = start..start + match_text.len();
        self.place_caret(start);
        self.reflow();
        let i = self.body().focus;
        self.center_row_in_view(i);
        self.view_mut().search_flash = Some(SearchFlash {
            node: id,
            range,
            until: Instant::now() + SEARCH_FLASH_DURATION,
        });
        true
    }

    /// Reveal one exact local-search range without turning it into the
    /// document selection. The search field keeps keyboard focus while this
    /// moves the document underneath it.
    fn reveal_text_range(&mut self, id: NodeId, range: std::ops::Range<usize>) -> bool {
        let Some(i) = self.body().index_of(id) else {
            return false;
        };
        self.shape_row_now(i);
        let Some(row) = self.body().rows.get(i) else {
            return false;
        };
        if range.start >= range.end || range.end > row.text.len() {
            return false;
        }
        let selection =
            Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
            );
        let mut top = f32::MAX;
        let mut bottom = f32::MIN;
        selection.geometry_with(&row.layout, |bb, _| {
            top = top.min(row.y + bb.y0 as f32);
            bottom = bottom.max(row.y + bb.y1 as f32);
        });
        self.body_mut().focus = i;
        self.view_mut().desired_x = None;
        self.scroll_search_band_into_view(top, bottom);
        true
    }

    /// Reveal one rendered table-cell match, including horizontal scrolling
    /// for an Extended table wider than the document measure.
    fn reveal_table_range(&mut self, id: NodeId, table_row: usize, column: usize) -> bool {
        let Some(i) = self.body().index_of(id) else {
            return false;
        };
        self.shape_row_now(i);
        let Some(row) = self.body().rows.get(i) else {
            return false;
        };
        let Some(table) = row.table.as_ref().and_then(|visual| visual.grid.as_ref()) else {
            return false;
        };
        let Some(height) = table.row_heights.get(table_row).copied() else {
            return false;
        };
        let Some(width) = table.columns.get(column).copied() else {
            return false;
        };
        let top = row.y + table.row_heights.iter().take(table_row).sum::<f32>();
        let bottom = top + height;
        let left = table.columns.iter().take(column).sum::<f32>();
        let right = left + width;
        let viewport_width = table.viewport_width;
        let table_width = table.width;
        self.body_mut().focus = i;
        self.view_mut().desired_x = None;
        self.scroll_search_band_into_view(top, bottom);
        let current = self.view().table_scroll.get(&id).copied().unwrap_or(0.0);
        let next = if left < current {
            left
        } else if right > current + viewport_width {
            right - viewport_width
        } else {
            current
        };
        self.view_mut()
            .table_scroll
            .insert(id, next.clamp(0.0, (table_width - viewport_width).max(0.0)));
        true
    }

    /// Convert the active local-search result into the editor's real selection.
    fn select_text_range(
        &mut self,
        id: NodeId,
        range: std::ops::Range<usize>,
        table_cell: Option<(usize, usize)>,
    ) -> bool {
        let Some(i) = self.body().index_of(id) else {
            return false;
        };
        let Some(row) = self.body().rows.get(i) else {
            return false;
        };
        if range.start > range.end || range.end > row.text.len() {
            return false;
        }
        self.body_mut().clear_selection();
        self.body_mut().focus = i;
        if let Some(row) = self.body_mut().focused_mut() {
            row.place_selection(range.start, range.end);
        }
        let view = self.view_mut();
        view.active = true;
        view.desired_x = None;
        view.table_cell = table_cell.map(|(row, column)| (id, row, column));
        self.reflow();
        self.scroll_search_band_into_view(
            self.body().rows[i].y,
            self.body().rows[i].y + self.body().rows[i].height,
        );
        true
    }

    fn scroll_search_band_into_view(&mut self, top: f32, bottom: f32) {
        let content_h = rowlayout::content_height(self.body());
        let view = self.view_mut();
        let band_top = view.chrome_h + theme::MARGIN_TOP;
        let band_bottom = view.viewport.1 - view.status_h - 8.0;
        let max = (content_h - view.content_viewport_h()).max(0.0);
        let visible_top = top - view.scroll;
        let visible_bottom = bottom - view.scroll;
        view.scroll_anim = None;
        if visible_top < band_top {
            view.scroll = (top - band_top).clamp(0.0, max);
        } else if visible_bottom > band_bottom {
            view.scroll = (bottom - band_bottom).clamp(0.0, max);
        }
    }

    fn search_flash_deadline(&self) -> Option<Instant> {
        self.view().search_flash.as_ref().map(|flash| flash.until)
    }

    fn maintain_search_flash(&mut self, now: Instant) -> bool {
        if self
            .view()
            .search_flash
            .as_ref()
            .is_some_and(|flash| flash.until <= now)
        {
            self.view_mut().search_flash = None;
            return true;
        }
        false
    }

    /// Put **this document's own name** into the shared name prompt.
    ///
    /// The same prompt a double-click on the title in the toolbar opens, reached
    /// without having to find the title first — which is what `F2` is for, and
    /// what makes renaming reachable from the explore tree where the title is
    /// not on screen at all.
    ///
    /// `false` for a view with no root to name: the outline is a realm, not a
    /// node, and there is nothing there to rename.
    fn start_rename(&mut self, ws: &WorkspaceDoc) -> bool {
        let Some(node) = self.body_mut().root else {
            return false;
        };
        let (current, page_icon) = ws
            .read(node)
            .ok()
            .map(|n| {
                (
                    n.content.as_text().map(str::to_owned).unwrap_or_default(),
                    n.meta.icon,
                )
            })
            .unwrap_or_default();
        let view = self.view_mut();
        view.overlay = trigger::open_rename(&mut view.engine, node, &current, page_icon);
        view.active = true;
        true
    }

    /// Open the document-name prompt and carry an exact authored range into
    /// it. A standalone document window has no title row, so this is its
    /// equivalent of selecting a local-search match in the editor body.
    fn start_rename_selection(&mut self, ws: &WorkspaceDoc, range: std::ops::Range<usize>) -> bool {
        if !self.start_rename(ws) {
            return false;
        }
        let view = self.view_mut();
        let ViewState {
            engine, overlay, ..
        } = view;
        if let Overlay::Name(prompt) = overlay {
            prompt.input.select_range(engine, range);
            true
        } else {
            false
        }
    }

    /// Reveal an image and put its display name into the shared name prompt.
    ///
    /// Both the Explore click and a canvas double-click enter through here, so
    /// they use the same fallback label, selection, commit, and focus rules.
    fn start_image_rename(&mut self, ws: &WorkspaceDoc, id: NodeId) -> bool {
        let Some(i) = self.body().index_of(id) else {
            return false;
        };
        if self.body().rows[i].image.is_none() {
            return false;
        }
        let image_no = self.body().rows[..=i]
            .iter()
            .filter(|row| row.image.is_some())
            .count();
        let current = ws
            .read(id)
            .ok()
            .and_then(|node| node.meta.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("Image {image_no}"));

        if !self.reveal(id) {
            return false;
        }
        let view = self.view_mut();
        // A double-click's second press may have armed a direct image drag.
        // Rename takes ownership of the gesture, so no reorder survives it.
        view.drag_row = None;
        view.dragging = false;
        view.overlay = trigger::open_image_rename(&mut view.engine, id, &current);
        view.active = true;
        true
    }

    // ------------------------------------------------------------ helpers

    /// Caret x in document coordinates. Kept document-relative rather than
    /// row-relative so a vertical hop into a row at a different depth still
    /// lands under the same visual column.
    fn caret_x(&self) -> f32 {
        let pills = &self.view().engine.pills;
        self.body()
            .focused()
            .map(|row| row.caret_x_with_pills(pills))
            .unwrap_or(0.0)
    }

    /// The focused node and the byte range its selection covers.
    fn target(&self) -> Option<(NodeId, std::ops::Range<usize>)> {
        let row = self.body().focused()?;
        Some((row.id, row.range()))
    }

    /// Whether there is a run of selected *text* — as opposed to a bare caret,
    /// or a set of whole rows.
    ///
    /// A text selection stays within one node (`docs/deferred/`), so a
    /// multi-row selection has no byte range to mark and reports `false`. That
    /// is what stops the formatting bubble offering Bold for a selection the
    /// model cannot bold.
    fn has_text_selection(&self) -> bool {
        !self.body().has_row_selection()
            && self
                .body()
                .focused()
                .is_some_and(|row| !row.selection.is_collapsed())
    }

    /// The rects the focused row's text selection covers, in **window**
    /// coordinates — one per visual line, so a selection that wraps reports
    /// each piece.
    ///
    /// Rows lay out from local x = 0 inside the centred content column and are
    /// drawn under a `content_x` translate, so getting from a parley bounding
    /// box to the window means `+ content_x + row.text_x()` across and
    /// `- scroll` down. That arithmetic was written out inline in three places
    /// in `draw.rs` before anything outside the row loop needed it; this is the
    /// one copy chrome asks.
    fn selection_rects(&self) -> Vec<Rect> {
        let Some(row) = self.body().focused() else {
            return Vec::new();
        };
        let ox = self.view().content_x + row.text_x();
        let oy = row.y - self.view().scroll;
        let mut out = Vec::new();
        row.selection.geometry_with(&row.layout, |bb, _| {
            out.push(Rect::new(
                bb.x0 as f32 + ox,
                bb.y0 as f32 + oy,
                (bb.x1 - bb.x0) as f32,
                (bb.y1 - bb.y0) as f32,
            ));
        });
        out
    }

    /// Where the caret is, in the same **window** coordinates
    /// [`selection_rects`](Self::selection_rects) reports.
    ///
    /// The sibling of that call for the case it cannot answer: a collapsed
    /// caret covers no glyphs, so it yields no rects, and anything anchoring to
    /// the caret — the bubble summoned by `Ctrl+.` — has nothing to hang from.
    /// The same conversion, applied to the caret's own box.
    fn caret_screen_rect(&self) -> Option<Rect> {
        let row = self.body().focused()?;
        let ox = self.view().content_x + row.text_x();
        let oy = row.y - self.view().scroll;
        let bb = rowlayout::caret_geometry(row, &self.view().engine.pills, theme::caret_w());
        let (baseline, ascent, descent) = koru_draw::text::TextEngine::line_ink(&row.layout, bb);
        let r = theme::caret_rect(bb, baseline, ascent, descent, ox, oy);
        Some(Rect::new(
            r.x0 as f32,
            r.y0 as f32,
            (r.x1 - r.x0) as f32,
            (r.y1 - r.y0) as f32,
        ))
    }

    fn move_vertical(&mut self, ws: &mut WorkspaceDoc, dir: i32, extend: bool) {
        ws.checkpoint();
        if self.body_mut().is_empty() {
            return;
        }
        let x = self.view_mut().desired_x.unwrap_or_else(|| self.caret_x());
        self.view_mut().desired_x = Some(x);

        let i = self.body_mut().focus;
        let line = self.body_mut().rows[i].line_index();
        let lines = self.body_mut().rows[i].line_count();
        let step: isize = if dir < 0 { -1 } else { 1 };

        // Already in block selection: Shift+Up/Down always hops by whole row.
        if extend && self.body().has_row_selection() {
            let target = i
                .checked_add_signed(step)
                .filter(|&n| n < self.body().rows.len())
                .unwrap_or(i);
            self.body_mut().extend_selection_to(target);
            self.view_mut().overlay = Overlay::None;
            return;
        }

        let can_stay = if dir < 0 { line > 0 } else { line + 1 < lines };
        if can_stay {
            // Still inside this paragraph (or code block): extend/move by
            // visual line — Shift+Down through a tall block selects its text.
            let row = &mut self.body_mut().rows[i];
            let sel = row.selection;
            let next = if dir < 0 {
                sel.previous_line(&row.layout, extend)
            } else {
                sel.next_line(&row.layout, extend)
            };
            row.set_selection_from_layout(next);
            return;
        }

        // At the row's top or bottom line.
        if extend {
            // Leaving this block: promote to pure block selection (and clear
            // text ranges). Code blocks stay line-clamped instead of spilling.
            if self.body().rows[i].kind == Kind::Code {
                let row = &mut self.body_mut().rows[i];
                let sel = row.selection;
                let next = if dir < 0 {
                    sel.line_start(&row.layout, true)
                } else {
                    sel.line_end(&row.layout, true)
                };
                row.set_selection_from_layout(next);
                return;
            }
            let target = i
                .checked_add_signed(step)
                .filter(|&n| n < self.body().rows.len())
                .unwrap_or(i);
            self.body_mut().extend_selection_to(target);
            self.view_mut().overlay = Overlay::None;
            return;
        }

        let target = i
            .checked_add_signed(step)
            .filter(|&n| n < self.body_mut().rows.len());
        let Some(target) = target else {
            // Already at the document edge — go to the very start or end.
            let row = &mut self.body_mut().rows[i];
            let sel = row.selection;
            let next = if dir < 0 {
                sel.line_start(&row.layout, false)
            } else {
                sel.line_end(&row.layout, false)
            };
            row.set_selection_from_layout(next);
            return;
        };

        self.body_mut().focus = target;
        let row = &mut self.body_mut().rows[target];
        // Aim at the first line coming down, the last line going up.
        let y = if dir < 0 {
            (row.height - 1.0).max(0.0)
        } else {
            1.0
        };
        let local_x = (x - row.text_x()).max(0.0);
        row.selection = Selection::from_point(&row.layout, local_x, y);
        row.snap_selection_to_graphemes();
        self.view_mut().overlay = Overlay::None;
    }

    /// Ctrl+Up/Down travels by paragraph rather than by visual line. Unlike
    /// ordinary vertical motion it deliberately forgets the desired x: this is
    /// structural navigation, and typing must begin at the target paragraph's
    /// first character.
    fn move_paragraph(&mut self, ws: &mut WorkspaceDoc, dir: i32) {
        ws.checkpoint();
        if self.body_mut().is_empty() {
            return;
        }
        let current = self.body().focus;
        let target = current
            .checked_add_signed(dir.signum() as isize)
            .filter(|&i| i < self.body().rows.len())
            .unwrap_or(current);
        self.body_mut().focus = target;
        self.body_mut().rows[target].place_caret(0);
        self.view_mut().desired_x = None;
        self.view_mut().overlay = Overlay::None;
    }

    fn toggle_page_title(&mut self, ws: &mut WorkspaceDoc) {
        ws.checkpoint();
        if !self.body().title_row || self.body().rows.is_empty() {
            return;
        }
        let focus = self.body().focus;
        if self.body().is_title_row(focus) {
            let Some((node, caret)) = self.view_mut().page_body_caret.take() else {
                return;
            };
            self.body_mut().focus_on(node);
            if let Some(row) = self.body_mut().focused_mut() {
                row.place_caret(caret.min(row.text.len()));
            }
        } else {
            let row = &self.body().rows[focus];
            self.view_mut().page_body_caret = Some((row.id, row.caret()));
            self.body_mut().focus = 0;
        }
        self.view_mut().desired_x = None;
        self.view_mut().overlay = Overlay::None;
    }

    /// Move one viewport, the keyboard counterpart to clicking the scrollbar's
    /// track.
    ///
    /// The caret travels with it: it lands on the row a page away, on the line
    /// at the same screen height, so typing picks up where the eye does. With
    /// `extend` the row selection grows to that row instead. And with no active
    /// line it just pans — reading a long list should not need a caret.
    fn move_page(&mut self, ws: &mut WorkspaceDoc, dir: i32, extend: bool) {
        ws.checkpoint();
        let vh = self.view().content_viewport_h();
        let content_h = rowlayout::content_height(self.body());

        // No active caret to carry: pan a page and stop. `scroll_to_caret` is a
        // no-op while inactive, so settle leaves this pan in place.
        if !self.view().active || self.body().focused().is_none() {
            let target = self.view().scroll + dir as f32 * vh;
            self.begin_scroll_to(target);
            return;
        }

        let x = self.view_mut().desired_x.unwrap_or_else(|| self.caret_x());
        self.view_mut().desired_x = Some(x);

        // The caret's document y, then one viewport beyond it — clamped to the
        // body so the last page lands on the final row rather than off the end.
        let caret_y = {
            let row = self.body().focused().expect("active caret checked above");
            let geo = row.selection.focus().geometry(&row.layout, 1.0);
            row.y + geo.y0 as f32
        };
        let target_y = (caret_y + dir as f32 * vh).clamp(0.0, content_h);
        let target = self.body().row_near_y(target_y);

        if extend {
            self.body_mut().extend_selection_to(target);
        } else {
            self.body_mut().focus = target;
            let row = &mut self.body_mut().rows[target];
            // The line nearest the target y keeps the caret at the same screen
            // height in a multi-line row.
            let local_y = (target_y - row.y).clamp(0.0, row.height);
            let local_x = (x - row.text_x()).max(0.0);
            row.selection = Selection::from_point(&row.layout, local_x, local_y);
            row.snap_selection_to_graphemes();
        }
        self.view_mut().overlay = Overlay::None;

        // Scroll a page to match, so the caret stays at the same screen height.
        let target = self.view().scroll + dir as f32 * vh;
        self.begin_scroll_to(target);
    }

    fn split_line(&mut self, ws: &mut WorkspaceDoc) {
        if let Some(row) = self.body().focused()
            && row.kind == Kind::Table
            && self.view().table_source == Some(row.id)
        {
            self.insert_text(ws, "\n");
            return;
        }
        // A code block: Enter is a newline, not a split (with two ways to leave
        // the block instead). Checked first so a code row never reaches the
        // structural split, which would refuse its multi-line text anyway.
        if self.body().focused().is_some_and(|r| r.kind == Kind::Code) {
            self.code_block_enter(ws);
            return;
        }
        // A markdown fence on a prose line converts it to a code block, in lieu
        // of splitting. Document-only — the outline has no prose to fence.
        if self.view().code_enabled && self.try_fence(ws) {
            return;
        }
        // A URL ending at the caret links before the line splits, so typing a
        // link and pressing Enter leaves a link rather than plain text.
        let caret = self.body().focused().map(Paragraph::caret).unwrap_or(0);
        if self.autolink_at(ws, caret) {
            // `autolink_at` queued a caret; apply it so the split point is the
            // position after the new pill, not the stale pre-edit one.
            self.reflow();
        }
        // Split moves focus onto the new tail row. Undo deletes that tail and
        // `rebuild` can only keep a still-live focus, so without this the caret
        // stays on whatever index the tail occupied — often a different row.
        if let Some(row) = self.body().focused() {
            self.view_mut().deleted_caret = Some((row.id, row.range().start));
        }
        if self.body_mut().split_focused(ws) {
            self.view_mut().desired_x = None;
            self.view_mut().overlay = Overlay::None;
        }
    }

    /// Enter inside a code block. Inserts a newline — except when the caret sits
    /// past two trailing blank lines of a block that has content, in which case
    /// it leaves the block (the "type your code, then Enter on the second blank
    /// line" idiom). Two blank lines, not one, so a single stray Enter at the
    /// end does not eject. A fresh or content-less block never ejects.
    fn code_block_enter(&mut self, ws: &mut WorkspaceDoc) {
        let Some(row) = self.body().focused() else {
            return;
        };
        let (id, text, caret) = (row.id, row.text.clone(), row.caret());
        let lines: Vec<&str> = text.split('\n').collect();
        let trailing_empty = lines.iter().rev().take_while(|l| l.is_empty()).count();
        let has_content = lines.iter().any(|l| !l.is_empty());
        if caret == text.len() && trailing_empty >= 2 && has_content {
            // The blank lines were the exit gesture, not content — trim them so
            // the block does not keep a tail of empty lines behind.
            let trimmed = text.trim_end_matches('\n');
            if trimmed != text.as_str() {
                let _ = ws.set_text(id, trimmed);
            }
            self.exit_code_block(ws);
            return;
        }
        self.insert_text(ws, "\n");
        self.reflow();
    }

    /// Leave the focused code block: move into the following paragraph if there
    /// is one, otherwise create a fresh paragraph below. No-op when the focused
    /// row is not a code block. Reached via Ctrl+Enter, and via Enter on the
    /// trailing blank line of a block that has content.
    fn exit_code_block(&mut self, ws: &mut WorkspaceDoc) {
        let Some(code) = self
            .body()
            .focused()
            .filter(|r| r.kind == Kind::Code)
            .map(|r| r.id)
        else {
            return;
        };
        let index = self.body().focus;
        self.body_mut().clear_selection();
        self.view_mut().overlay = Overlay::None;
        self.view_mut().desired_x = None;
        // Leaving a diagram's text closes it: the block goes back to being a
        // picture (or its text and reason) as the caret lands below.
        if self.view().diagram_text_open == Some(code) {
            self.view_mut().diagram_text_open = None;
        }
        // Move into an existing following row if there is one; otherwise make
        // one. Creating only when needed keeps the document from accumulating
        // empty paragraphs every time you leave a block.
        let target = match self.body().rows.get(index + 1).map(|r| r.id) {
            Some(next) => next,
            None => {
                ws.checkpoint();
                let Ok(paragraph) = Node::paragraph("") else {
                    return;
                };
                match ws.insert_after(code, paragraph) {
                    Ok(new) => new,
                    Err(_) => return,
                }
            }
        };
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(target);
        self.place_caret(0);
        self.reflow();
    }

    /// If the focused prose line is a markdown fence, convert it to a code
    /// block — clearing the fence text and pinning the captured language — and
    /// return true. Only when the whole line is the fence and the caret is at
    /// its end, so a backtick typed mid-sentence is left alone. Enter and a
    /// space both come here: the fence waits for one of them, so its language
    /// can be typed first.
    fn try_fence(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(row) = self.body().focused() else {
            return false;
        };
        if row.kind == Kind::Code || row.image.is_some() {
            return false;
        }
        let (id, text, caret) = (row.id, row.text.clone(), row.caret());
        if caret != text.len() {
            return false;
        }
        let Some(lang) = koru_blocks::parse_fence(&text) else {
            return false;
        };
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.set_text(id, "");
        let _ = ws.set_kind(id, Kind::Code);
        if let Some(lang) = lang {
            let _ = ws.set_language(id, Some(&lang));
        }
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(0);
        self.reflow();
        true
    }

    /// Enter on a selected non-text block — an image, divider or rendered table
    /// — opens a fresh empty paragraph *below* it and lands the caret there.
    /// None can be split like a text row, so the block path always inserts a
    /// paragraph after the complete node. `false` means the focused row is text
    /// and the caller should split it. The mirror, for *above*, is
    /// [`enter_block_above`](Self::enter_block_above) (Shift+Enter).
    fn enter_on_block(&mut self, ws: &mut WorkspaceDoc) -> bool {
        // A drawn diagram is the one block Enter opens rather than steps past:
        // its text is what a person wants next, and it is one Enter away.
        // Shift+Enter above it stays the structural insert every block has.
        if let Some(diagram) = self
            .body()
            .focused()
            .filter(|row| row.diagram_shows_picture())
            .map(|row| row.id)
        {
            return self.open_diagram_text(diagram);
        }
        let Some(block_id) = self.focused_block_id() else {
            return false;
        };
        self.insert_paragraph_after_block(ws, block_id)
    }

    /// Create a fresh paragraph immediately after a block and put the caret at
    /// its start. Both Enter on a selected rendered table and Ctrl+Enter on a
    /// focused table use this one structural operation, so neither can split or
    /// rewrite the table's canonical Markdown source.
    fn insert_paragraph_after_block(&mut self, ws: &mut WorkspaceDoc, block_id: NodeId) -> bool {
        self.body_mut().clear_selection();
        self.view_mut().overlay = Overlay::None;
        self.view_mut().drag_row = None;
        self.view_mut().table_cell = None;
        self.view_mut().desired_x = None;
        self.view_mut().active = true;

        let Ok(paragraph) = Node::paragraph("") else {
            return true;
        };
        ws.checkpoint();
        let Ok(next) = ws.insert_after(block_id, paragraph) else {
            return true;
        };
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(next);
        self.place_caret(0);
        true
    }

    /// Alt+Enter's symmetric counterpart to
    /// [`insert_paragraph_after_block`](Self::insert_paragraph_after_block).
    /// The focused row remains intact and an ordinary paragraph is inserted at
    /// its sibling position. The title is the document root rather than a
    /// sibling row, so it deliberately refuses this structural operation.
    fn insert_paragraph_before_focused(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.title_focused() {
            return true;
        }
        let Some(id) = self.body().focused_id() else {
            return false;
        };
        self.insert_paragraph_before_block(ws, id)
    }

    fn insert_paragraph_before_block(&mut self, ws: &mut WorkspaceDoc, block_id: NodeId) -> bool {
        self.body_mut().clear_selection();
        self.view_mut().overlay = Overlay::None;
        self.view_mut().drag_row = None;
        self.view_mut().table_cell = None;
        self.view_mut().desired_x = None;
        self.view_mut().active = true;

        let parent = self.body().parent_of(ws, block_id);
        let Some(at) = ws.sibling_index(block_id).ok() else {
            return true;
        };
        let Ok(paragraph) = Node::paragraph("") else {
            return true;
        };
        ws.checkpoint();
        let Ok(new_id) = ws.insert(parent, paragraph) else {
            return true;
        };
        let _ = ws.move_node(new_id, parent, at);
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(new_id);
        self.place_caret(0);
        true
    }

    /// Shift+Enter on a selected non-text block opens a fresh empty paragraph
    /// *above* it, pushing the block down. The mirror of [`enter_on_block`];
    /// `false` for a text row, so the caller falls through to an ordinary split.
    fn enter_block_above(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(block_id) = self.focused_block_id() else {
            return false;
        };
        self.insert_paragraph_before_block(ws, block_id)
    }

    /// The focused row, if it is a non-text block (an image, divider or rendered
    /// table). A table whose Markdown source is open remains text: plain Enter
    /// inserts a source line there, while Escape validates and returns to the
    /// grid.
    /// This is the shared front half of [`enter_on_block`] and
    /// [`enter_block_above`].
    fn focused_block_id(&self) -> Option<NodeId> {
        let row = self.body().focused()?;
        let is_block = row.image.is_some()
            || row.kind == koru_core::node::Kind::Divider
            || (row.kind == Kind::Table && !row.table_source_visible)
            || row.diagram_shows_picture();
        is_block.then_some(row.id)
    }

    // ------------------------------------------------------------- diagrams
    //
    // A `mermaid` code block drawn in place (`crate::diagram`). The editor
    // keeps the state per view and asks the host for every layout; these are
    // the seam the host calls and the two gestures a person has.

    /// The layouts the editor is waiting for, in the order it asked, taken
    /// off the queue. The host lays each text out on a worker of its own and
    /// answers through [`diagram_result`](Self::diagram_result). Empty on
    /// almost every call: a request is pushed once per distinct text.
    fn take_diagram_requests(&mut self) -> Vec<crate::diagram::DiagramRequest> {
        std::mem::take(&mut self.view_mut().diagram_requests)
    }

    /// The host's answer for `node`'s text `text_hash`: the prepared drawing,
    /// or the status that says why there is none (every failure mapped to a
    /// `Status` by the host — a timeout as too large, an internal error as a
    /// line that could not be read).
    ///
    /// Ignored unless that hash is the one in flight for the node, so a stale
    /// answer never puts a drawing of other text on screen. `true` means the
    /// view changed and a redraw is owed: the host returns it from its
    /// `maintain`, and the next frame's reflow takes the new height from the
    /// entry without reshaping any text.
    fn diagram_result(
        &mut self,
        node: NodeId,
        text_hash: u64,
        result: Result<std::sync::Arc<koru_draw::diagram::PreparedDiagram>, koru_diagram::Status>,
    ) -> bool {
        crate::diagram::land(self.view_mut(), node, text_hash, result)
    }

    /// Open a diagram's text for editing in this view, with the caret on the
    /// line its status names — the unreadable line of a `NotParsed`, line 1
    /// otherwise. Enter on the selected drawing, the *Edit text* control and
    /// the menu row all come here. `false` when `node` is not a diagram row.
    ///
    /// On a block whose text is already showing (closed and not drawing) this
    /// is a caret placement: the text was never hidden, so there is nothing
    /// to reveal, and marking it open means the drawing returns *below* the
    /// text once the line is fixed rather than replacing it mid-edit.
    fn open_diagram_text(&mut self, node: NodeId) -> bool {
        let Some(row) = self
            .body()
            .row(node)
            .filter(|row| crate::diagram::is_diagram_row(row))
        else {
            return false;
        };
        let line = match self.view().diagrams.get(&node).map(|entry| &entry.state) {
            Some(crate::diagram::DiagramState::Failed(koru_diagram::Status::NotParsed {
                line,
                ..
            })) => *line,
            _ => 1,
        };
        let at = crate::diagram::line_start(&row.text, line);
        self.body_mut().clear_selection();
        self.body_mut().focus_on(node);
        let view = self.view_mut();
        view.diagram_text_open = Some(node);
        view.active = true;
        view.overlay = Overlay::None;
        view.drag_row = None;
        view.desired_x = None;
        // The control's job is done; the text under the pointer is the
        // answer now.
        if view.table_controls_node == Some(node) {
            view.table_controls.clear();
            view.table_controls_node = None;
        }
        self.place_caret(at);
        self.reflow();
        self.scroll_to_caret();
        true
    }

    /// Ask the document app to show `node`'s drawing in its viewer — the
    /// magnifier control, a double click on the drawing and the menu's
    /// *Open* row all come here. The block is selected first, the way a
    /// click on the drawing selects it, so what the viewer closes back onto
    /// is the block it opened from. `false` when `node` is not a diagram
    /// row showing its picture: there is nothing to open, and the gesture
    /// falls through unclaimed. The editor owns no modal, so this only sets
    /// `wants_open_diagram`; the app drains it after the press.
    fn ask_open_diagram(&mut self, node: NodeId) -> bool {
        if !self.select_drawing(node) {
            return false;
        }
        self.view_mut().wants_open_diagram = Some(node);
        true
    }

    /// Ask the document app to put `node`'s drawing on the clipboard — the
    /// *Copy image* control. The block is selected the way *Open* selects
    /// it, the shared copy glyph confirms at once (the shell writes the
    /// clipboard on its next pass, and a person is owed the check now, not
    /// then), and the toast every copy raises is asked for. `false` when
    /// `node` is not a diagram row showing its picture.
    fn ask_copy_diagram(&mut self, node: NodeId) -> bool {
        if !self.select_drawing(node) {
            return false;
        }
        let view = self.view_mut();
        view.copy.confirm(Instant::now());
        view.wants_copy_diagram = Some(node);
        view.wants_toast = Some(koru_ui::CopyAffordance::TOAST.to_string());
        true
    }

    /// Ask the document app to raise *Save as…* on `node`'s drawing — the
    /// control beside *Copy image*, on the route the menu row and the chord
    /// already take. `false` when `node` is not a diagram row showing its
    /// picture.
    fn ask_save_diagram(&mut self, node: NodeId) -> bool {
        if !self.select_drawing(node) {
            return false;
        }
        self.view_mut().wants_save_diagram_as = Some(SaveDiagramAs { node });
        true
    }

    /// Select the drawing a control was pressed on, the way a click on the
    /// drawing selects it: focus on the block, no selection, no overlay, no
    /// drag. `false` when `node` is not a diagram row showing its picture,
    /// so the gesture falls through unclaimed.
    fn select_drawing(&mut self, node: NodeId) -> bool {
        if !self
            .body()
            .row(node)
            .is_some_and(Paragraph::diagram_shows_picture)
        {
            return false;
        }
        self.body_mut().clear_selection();
        self.body_mut().focus_on(node);
        let view = self.view_mut();
        view.active = true;
        view.overlay = Overlay::None;
        view.drag_row = None;
        true
    }

    /// Close the open diagram text, if any, so the block shows its picture
    /// again (or its text and reason, when the text does not draw). The
    /// drawing of the last text that worked stays in the entry for the next
    /// time the text is opened. `true` when something was open.
    fn close_diagram_text(&mut self) -> bool {
        if self.view_mut().diagram_text_open.take().is_none() {
            return false;
        }
        self.reflow();
        true
    }

    /// Show a selected drawing's *Edit text* control the moment the keyboard
    /// lands on it, through the clock hover uses — `set_focus` neither waits
    /// nor fades in, because arrowing onto a block has already said what the
    /// person wants to know. Leaving the drawing by keyboard lets it fade; a
    /// control the pointer revealed on some other block is left alone.
    fn sync_diagram_controls_focus(&mut self, now: Instant) {
        let focused = self
            .body()
            .focused()
            .filter(|row| row.diagram_shows_picture() && self.view().active)
            .map(|row| (row.id, self.diagram_control_content(row)));
        let shown = self.view().table_controls_node;
        match focused {
            Some((id, content)) => {
                if shown != Some(id) || !self.view().table_controls.is_present() {
                    let view = self.view_mut();
                    view.table_controls_node = Some(id);
                    view.table_controls.set_focus(Some(content), now);
                }
            }
            None => {
                let on_diagram = shown.is_some_and(|id| {
                    self.body()
                        .row(id)
                        .is_some_and(Paragraph::diagram_shows_picture)
                });
                // The pointer resting on that very row still wants it.
                let pointer_on_it = self
                    .view()
                    .hover
                    .and_then(Hover::row)
                    .and_then(|index| self.body().rows.get(index))
                    .map(|row| row.id)
                    == shown;
                if on_diagram && !pointer_on_it {
                    let view = self.view_mut();
                    if view.table_controls.set_focus(None, now) && !view.table_controls.is_present()
                    {
                        view.table_controls_node = None;
                    }
                }
            }
        }
    }

    /// What the controls clock carries for a drawn diagram: its one control's
    /// name, anchored on the row. The key is the node, so moving between two
    /// drawings restarts the reveal rather than sliding one.
    fn diagram_control_content(&self, row: &Paragraph) -> koru_ui::TooltipContent {
        let raw = row.id.as_uuid().as_u128();
        let key = raw as u64 ^ (raw >> 64) as u64;
        let anchor = Rect::new(row.text_x(), row.y, self.view().viewport.0, row.height);
        koru_ui::TooltipContent::new(
            key,
            anchor,
            koru_i18n::t(koru_i18n::current(), koru_i18n::Key::DiagramEditText),
        )
    }

    /// Follow a step through history, offering the caret offset the edit took
    /// it from. The offset is the editor's to remember; finding the row is not.
    fn follow_history(&mut self, before: &[NodeId]) {
        // A row that came back takes the focus, and the caret goes where it was
        // inside that row when it was deleted.
        if let Some(back) = self.body_mut().follow_history(before) {
            let at = match self.view().deleted_caret {
                Some((was, at)) if was == back => at,
                _ => 0,
            };
            self.place_caret(at);
            return;
        }
        // Nothing came back. Either the edit was *within* a row (word delete,
        // typing) or a split whose tail just vanished. In both cases the caret
        // belongs where it was before that edit — undoing a word delete puts
        // you behind the word again, and undoing Enter puts you back at the
        // split point on the rejoined row rather than on a neighbour that
        // inherited the tail's index.
        if let Some((was, at)) = self.view().deleted_caret {
            let still_here = self.body().index_of(was).is_some();
            let focused_here = self.body().focused_id() == Some(was);
            let a_row_vanished = before.iter().any(|id| self.body().index_of(*id).is_none());
            if still_here && (focused_here || a_row_vanished) {
                self.body_mut().focus_on(was);
                self.place_caret(at);
            }
        }
    }

    /// Remove the focused row, or every selected row once a selection exists.
    ///
    /// `promote` decides what happens to a deleted row's children. A plain
    /// delete (`promote = true`) outdents them into the row's place — deleting
    /// a bullet keeps the bullets nested under it. A cut (`promote = false`)
    /// takes them with the row, because a cut is a move and the children must
    /// travel to wherever the paste lands. A child the user *also* selected is
    /// deleted outright either way, because an explicit selection means "these
    /// rows too", not "these rows' parents".
    ///
    /// A body with no body rows is a dead end: there is nothing to put a caret
    /// in, so typing does nothing and the only way out is another window. A
    /// document title does not satisfy that invariant. Deleting the last body
    /// row therefore leaves a fresh empty one of the same kind, the way
    /// `Editor::new` gives a fresh workspace its first line.
    ///
    /// **This is the structural delete seam, so it is where a task learns a
    /// child left it** (T-0c, [ADR-286]'s `task.child.removed`). Both callers
    /// come through here — the explicit row delete and the cut of whole rows —
    /// and a cut is deliberately included: a cut is a delete followed by a
    /// paste somewhere else, and the paste's own hook reports the arrival. The
    /// row-merge path is *not* here and must not be: joining two rows removes a
    /// node, but the text it held survives in the row above, so nothing left
    /// the task.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    fn delete_rows(&mut self, ws: &mut WorkspaceDoc, promote: bool) -> bool {
        // Its own undo step: closing the group first stops it folding into
        // whatever was typed a moment earlier.
        ws.checkpoint();
        let Some(row) = self.body().focused() else {
            return false;
        };
        let (kind, id, caret) = (row.kind, row.id, row.caret());
        // First selected row — the start of the range — not whichever end holds
        // focus. A drag or Shift+Down leaves focus at the bottom; landing there
        // after the hole closes jumps past the first survivor (paste already
        // uses the first selected row for the same reason).
        let land = self
            .body()
            .rows
            .iter()
            .position(|r| r.selected)
            .unwrap_or(self.body().focus);
        self.view_mut().deleted_caret = Some((id, caret));
        // Only the tops of the selection: a child travels with its parent, so
        // deleting a child after its parent has gone would hit a node that is
        // already gone.
        let doomed = self.body_mut().selection_roots(ws);
        if doomed.is_empty() {
            return false;
        }
        // **What each doomed row takes out of a task, asked before it goes**
        // (T-0c). The tops only, which is the same granularity the arrival side
        // keeps: a node inside a deleted subtree loses a task ancestor that is
        // going with it, and an event filed under a task that no longer exists
        // is noise rather than history. A promoted child is not a departure
        // either — it lands in its parent's place, under the same task.
        let departing = koru_core::tasks::departing_all(ws, &doomed);

        ws.begin_group();
        let mut removed = false;
        // A child the user selected alongside its parent is deleted outright; a
        // child that only came along because its parent was selected is
        // promoted. Distinguishing them matters: without it, selecting a parent
        // *and* one of its children then deleting would outdent the child the
        // user meant to delete.
        let explicit: std::collections::HashSet<NodeId> =
            self.body().selected_ids().into_iter().collect();
        let mut gone: Vec<NodeId> = Vec::new();
        for id in doomed {
            let one = if promote {
                if let Ok(children) = ws.children(Parent::Node(id)) {
                    for child in children {
                        if explicit.contains(&child) {
                            let _ = ws.delete(child);
                        }
                    }
                }
                ws.delete_promoting_children(id).is_ok()
            } else {
                ws.delete(id).is_ok()
            };
            if one {
                gone.push(id);
            }
            removed |= one;
        }
        if !removed {
            ws.checkpoint();
            return false;
        }
        // Only the rows that actually went. A delete that refused is not a
        // departure, the same way a row that refused to move is not a move.
        let departed: Vec<_> = departing
            .into_iter()
            .filter(|d| gone.contains(&d.child()))
            .collect();
        self.body_mut().note_deletions(&departed);

        // A view always has a line to type on. Asked *after* the delete rather
        // than predicted before it: a doomed parent may hold children that are
        // promoted rather than removed, so counting the doomed rows against the
        // visible ones said "not all of them" when one selected parent had
        // emptied the whole page.
        self.body_mut().rebuild(ws);
        if self.body().rows.len() == self.body().first_body_row() {
            // The body's own root, not the deleted row's parent — with nothing
            // left, the only place a first line can go is the top.
            let under = match self.body().root {
                Some(root) => Parent::Node(root),
                None => Parent::Root,
            };
            // The fresh line has to be typeable text: a non-text block (a
            // divider, or a blob) would leave the page a block you cannot type
            // into, and cloning a divider here would make delete feel like it
            // did nothing.
            let line_kind = if kind.shape() == koru_core::node::Shape::Text
                && kind != koru_core::node::Kind::Divider
            {
                kind
            } else {
                koru_core::node::Kind::Paragraph
            };
            if let Ok(fresh) = Node::new(line_kind, Content::text("")) {
                let _ = ws.insert(under, fresh);
            }
        }
        ws.checkpoint();

        self.body_mut().clear_selection();
        self.body_mut().rebuild(ws);
        // Land at the start of the hole, or the last row when the selection ran
        // to the end of the page. A failed fresh-row insert leaves only a
        // document title; its useful fallback is the end, ready to continue
        // typing, rather than byte zero.
        self.body_mut().focus = land.min(self.body_mut().rows.len().saturating_sub(1));
        let caret = self
            .body()
            .focused()
            .filter(|_| self.body().focus_is_title())
            .map_or(0, |row| row.text.len());
        self.place_caret(caret);
        self.disarm();
        self.view_mut().overlay = Overlay::None;
        true
    }

    /// Arm a system-clipboard image copy for the focused image row. Returns
    /// whether one was armed. Hash-based so the shell does not need the node.
    fn arm_copy_focused_image(&mut self) -> bool {
        let Some(hash) = self
            .body()
            .focused()
            .and_then(|r| r.image.as_ref().map(|img| img.hash.0.clone()))
        else {
            return false;
        };
        crate::clipboard::clear();
        self.view_mut().wants_copy_image = Some(hash);
        true
    }

    /// Undo the latest image/rich paste batch via a compensating edit.
    ///
    /// Live Loro undo reports `moved=true` without removing nested paste nodes
    /// (Phase 0 logs). Removing the paste and restoring anything it displaced
    /// is one Loro step; redo reverses it with `ws.undo()`.
    fn undo_paste_batch(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(batch) = self.view_mut().last_image_pastes.pop() else {
            return false;
        };
        if batch.inserted.is_empty() {
            return false;
        }
        let still = batch
            .inserted
            .iter()
            .filter(|id| ws.read(**id).is_ok())
            .count();
        if still == 0 {
            crate::clipboard::undo_dbg("paste batch already gone; popped hint");
            return false;
        }

        let before = self.body_mut().ids();
        self.view_mut().paste_redo_exhausted = false;
        ws.checkpoint();
        ws.begin_group();
        for id in &batch.inserted {
            let _ = ws.delete(*id);
        }
        for replaced in &batch.replaced {
            let _ = ws.restore_cutting(&replaced.cutting, replaced.parent, replaced.index);
        }
        ws.checkpoint();
        self.view_mut().last_image_redos.push(batch);
        self.body_mut().rebuild(ws);
        self.disarm();
        self.follow_history(&before);
        self.view_mut().notice = None;
        crate::clipboard::undo_dbg(format!(
            "inverse deleted paste batch; paste_redos={} can_undo={} can_redo={}",
            self.view().last_image_redos.len(),
            ws.can_undo(),
            ws.can_redo()
        ));
        true
    }

    /// Redo a paste batch: reverse the inverse delete via `ws.undo()`.
    fn redo_paste_batch(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(batch) = self.view_mut().last_image_redos.pop() else {
            // Full paste redo cycle left inverse deletes on Loro's redo stack;
            // do not replay them.
            if self.view().paste_redo_exhausted {
                crate::clipboard::undo_dbg("paste redo exhausted; ignoring Loro redo");
                return true; // handled as no-op
            }
            return false;
        };
        let before = self.body_mut().ids();
        if !ws.undo().unwrap_or(false) {
            self.view_mut().last_image_redos.push(batch);
            crate::clipboard::undo_dbg("redo paste: ws.undo of delete failed");
            return false;
        }
        self.view_mut().last_image_pastes.push(batch);
        if self.view().last_image_redos.is_empty() {
            self.view_mut().paste_redo_exhausted = true;
        }
        self.body_mut().rebuild(ws);
        self.disarm();
        self.follow_history(&before);
        self.view_mut().notice = None;
        crate::clipboard::undo_dbg(format!(
            "redo paste via undo-of-delete; paste_stack={} redos={}",
            self.view().last_image_pastes.len(),
            self.view().last_image_redos.len()
        ));
        true
    }

    /// Delete the focused row's node and land on the line above (caret at its
    /// end), or on the next line, or leave a fresh empty paragraph if it was
    /// the only row. Used by Backspace on an empty code block — the block has
    /// nothing to merge, so it goes away rather than emptying into a neighbour.
    fn delete_focused_block(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.title_focused() {
            return false;
        }
        let Some(id) = self.body().focused().map(|r| r.id) else {
            return false;
        };
        let index = self.body().focus;
        let prev = index.checked_sub(1);
        self.view_mut().deleted_caret = Some((id, 0));
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.delete(id);
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        if self.body().rows.is_empty() {
            // Nothing left — leave a fresh empty paragraph to type in, not a
            // code block (the user is leaving the block).
            let under = match self.body().root {
                Some(r) => Parent::Node(r),
                None => Parent::Root,
            };
            let _ = ws.insert(under, Node::paragraph("").unwrap());
            self.body_mut().rebuild(ws);
        }
        self.body_mut().clear_selection();
        // Land on the previous row's end (merge-like) when there is one;
        // otherwise on whatever took this row's place.
        let land = prev
            .unwrap_or(index)
            .min(self.body().rows.len().saturating_sub(1));
        self.body_mut().focus = land;
        if let Some(row) = self.body_mut().focused_mut() {
            row.place_caret(row.text.len());
        }
        self.disarm();
        self.view_mut().overlay = Overlay::None;
        self.reflow();
        true
    }

    /// Remove the focused empty block, leaving the row below untouched.
    ///
    /// Only called from `delete_selection_or`'s guard, which has already
    /// established the row is empty, childless and not the title. The row
    /// below slides up into this one's place, and the caret goes to its
    /// start — Delete was aimed below, so that is where the user is looking.
    fn delete_empty_block(&mut self, ws: &mut WorkspaceDoc) {
        let Some(id) = self.body().focused().map(|r| r.id) else {
            return;
        };
        let index = self.body().focus;
        self.view_mut().deleted_caret = Some((id, 0));
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.delete(id);
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().clear_selection();
        let land = index.min(self.body().rows.len().saturating_sub(1));
        self.body_mut().focus = land;
        if let Some(row) = self.body_mut().focused_mut() {
            row.place_caret(0);
        }
        self.disarm();
        self.view_mut().overlay = Overlay::None;
        self.reflow();
    }

    /// The focused line, or nothing when the caret has been dropped.
    fn line(&mut self) -> Option<&mut Paragraph> {
        self.body_mut().focused_mut()
    }

    /// Delete, joining with a neighbour when the caret is already at an edge.
    ///
    /// Three layers in four lines: the line deletes within itself, the list
    /// merges two nodes, and the editor only decides which of them to ask.
    fn delete_selection_or(&mut self, ws: &mut WorkspaceDoc, backwards: bool, by_word: bool) {
        self.view_mut().desired_x = None;
        // An image row has no text to delete within and no neighbour to merge
        // with, so Backspace/Delete remove the block itself — delegating to the
        // whole-row path, which already handles a focused image and repositions
        // the caret. A divider is the same: no text, no merge. A diagram
        // showing its picture is selected like an image and goes like one —
        // the whole block, in one undoable step — rather than losing a
        // character of text nobody can see.
        if self.body().focused().is_some_and(|r| {
            r.kind == koru_core::node::Kind::Image
                || r.kind == koru_core::node::Kind::Divider
                || r.diagram_shows_picture()
        }) {
            self.delete_rows(ws, true);
            return;
        }
        // Delete in an *empty* block removes the block and leaves the row
        // below untouched, whatever either one's kind: an empty block has no
        // content to defend, so its kind is what is lost and the row below is
        // inherited as it is. A merge would do the opposite — pull the row
        // below's text up and kill *its* identity: a heading becomes body
        // text, a task loses its checkbox, an image is deleted outright. The
        // title is exempt — it is the document, and row 0 can never go — and
        // a block with children is not disposable either, because deleting it
        // would take the subtree with it.
        if !backwards {
            let empty = self
                .body()
                .focused()
                .is_some_and(|row| !row.is_title && row.text.is_empty());
            let below_exists = self.body().rows.get(self.body().focus + 1).is_some();
            let childless = self.body().focused().is_some_and(|row| {
                ws.children(Parent::Node(row.id))
                    .is_ok_and(|c| c.is_empty())
            });
            if empty && below_exists && childless {
                self.delete_empty_block(ws);
                return;
            }
        }
        // An empty code block is deleted by Backspace: there is no text to take
        // a character from and no newline to remove, so Backspace removes the
        // block itself and lands on the line above (the way merging an empty
        // line would, without leaving an empty code block behind).
        if backwards
            && self
                .body()
                .focused()
                .is_some_and(|r| r.kind == Kind::Code && r.text.is_empty() && r.range().is_empty())
        {
            self.delete_focused_block(ws);
            return;
        }
        // Empty first body row under the title: collapse into the title rather
        // than no-op at the title seam (must not merge non-empty text into the
        // document name).
        if backwards && self.body().empty_first_body_at_start() {
            self.delete_focused_block(ws);
            return;
        }
        // A multi-line code block cannot merge its text backwards into a
        // one-line node: the structural merge would have to move a `\n` into a
        // kind that forbids one, and refusing it (even atomically) is a
        // half-applied join in disguise. Backspace at the very start of such a
        // block is a no-op instead. Joining two code blocks is left alone.
        if backwards {
            if let Some(row) = self.body().focused() {
                if row.kind.allows_newlines()
                    && row.text.contains('\n')
                    && row.range().is_empty()
                    && row.at_start()
                {
                    let neighbour_allows = self
                        .body()
                        .focus
                        .checked_sub(1)
                        .and_then(|i| self.body().rows.get(i))
                        .is_some_and(|n| n.kind.allows_newlines());
                    if !neighbour_allows {
                        return;
                    }
                }
            }
        }
        // Where the caret was before the text went, so undo can put it back
        // there rather than leaving it where the shortened line ended. After
        // Ctrl+Backspace and Ctrl+Z the word is restored *and* the caret is
        // behind it again, which is what makes the pair reversible.
        //
        // **Only when a step is starting.** A run of Ctrl+Backspace presses
        // joins one undo group and comes back in one Ctrl+Z, so the position to
        // return to is the one from before the *first* press. Recording on
        // every press let the last one win, and undoing three deletes put the
        // caret two words short of where it started.
        if !ws.in_group() {
            if let Some(row) = self.body().focused() {
                // The *start* of what is about to go, not the caret. With a
                // selection the caret sits at whichever end the drag stopped
                // at, so recording it put the caret after text that had just
                // come back — and made dragging the same words right-to-left
                // undo differently from left-to-right. Collapsed, the two are
                // the same offset anyway.
                let at = row.range().start;
                self.view_mut().deleted_caret = Some((row.id, at));
            }
        }
        let Some(row) = self.body_mut().focused_mut() else {
            return;
        };
        // The row's id before the delete, because a merge below moves the
        // caret and this row may be the one that goes.
        let id = row.id;
        if row.delete_within(ws, backwards, by_word) {
            // Text left this row and the row stayed: the burst is on it.
            // `merge` notes the surviving row itself, from the one place that
            // knows which of the two that is.
            self.body_mut().note_text_edit(ws, id);
            return;
        }
        if self.body_mut().merge(ws, backwards).is_some() && backwards {
            self.view_mut().overlay = Overlay::None;
        }
    }

    /// Move the caret, stepping into the neighbouring row at a line's edge.
    fn move_horizontal(&mut self, ws: &mut WorkspaceDoc, dir: i32, by_word: bool, extend: bool) {
        ws.checkpoint();
        self.view_mut().desired_x = None;
        let last = self.body_mut().rows.len().saturating_sub(1);
        let index = self.body_mut().focus;
        let Some(row) = self.line() else {
            return;
        };
        if row.move_within(dir, by_word, extend) {
            return;
        }
        // At an edge: the neighbour, landing at the end it was entered from.
        let target = if dir < 0 {
            index.checked_sub(1)
        } else {
            (index < last).then(|| index + 1)
        };
        let Some(target) = target else {
            return;
        };
        self.body_mut().focus = target;
        if let Some(row) = self.line() {
            row.caret_to_edge(dir > 0);
        }
    }

    /// Stop *on* a pill the caret is about to cross, rather than stepping over
    /// it. `true` when the motion was spent doing that.
    ///
    /// A pill is a single anchor character, so one Left or Right always crossed
    /// the whole thing and there was no way to reach one without the mouse. Now
    /// the first press selects it and the second steps past — the two-press
    /// pattern a checkbox or a link gets in any list, and the reason the state
    /// is a *stop* rather than a mode: nothing has to be pressed to leave it.
    ///
    /// Word motion and Shift+Arrow are excluded deliberately. `Ctrl+Arrow` means
    /// "skip a word" and a pill already *is* one word (see
    /// `Paragraph::before`/`after`); `Shift+Arrow` is building a text selection,
    /// and a selection that silently became an object selection would delete
    /// something other than what is highlighted.
    fn step_onto_pill(&mut self, dir: i32, by_word: bool, extend: bool) -> bool {
        if by_word || extend {
            self.view_mut().selected_pill = None;
            return false;
        }
        // Standing on one already: this press is the one that steps past, so let
        // go and let the ordinary motion run.
        if self.view().selected_pill.is_some() {
            self.view_mut().selected_pill = None;
            return false;
        }
        let index = self.body().focus;
        let Some(row) = self.body().rows.get(index) else {
            return false;
        };
        // A live selection is being collapsed by this press, not walked out of.
        if !row.range().is_empty() {
            return false;
        }
        let at = row.caret();
        let anchor = if dir > 0 {
            row.text[at..].starts_with(ANCHOR).then_some(at)
        } else {
            row.text[..at]
                .ends_with(ANCHOR)
                .then(|| at.saturating_sub(ANCHOR_LEN))
        };
        let Some(anchor) = anchor else {
            return false;
        };
        let Some(attr) = row
            .runs
            .iter()
            .find(|r| r.start.get() == anchor)
            .and_then(|r| r.pill)
        else {
            return false;
        };
        self.view_mut().selected_pill = Some((index, attr));
        true
    }

    /// **Every local text insertion goes through here** — a keypress, an
    /// Enter inside a code block, a plain-text paste, a completion's
    /// replacement — which is what makes it the one place a burst note belongs
    /// rather than twenty ([ADR-286]).
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    fn insert_text(&mut self, ws: &mut WorkspaceDoc, s: &str) {
        self.view_mut().desired_x = None;
        let Some(row) = self.line() else {
            return;
        };
        let id = row.id;
        row.insert_text(ws, s);
        self.body_mut().note_text_edit(ws, id);
    }

    fn toggle_mark(&mut self, ws: &mut WorkspaceDoc, mark: Mark) {
        self.view_mut().desired_x = None;
        if self.body().has_row_selection() {
            // Intersection across the selection: partly-bold rows become all bold.
            let on = !self
                .body()
                .rows
                .iter()
                .filter(|r| r.selected && row_takes_marks(r) && !r.text.is_empty())
                .all(|r| {
                    ws.marks_in(r.id, 0..r.text.len())
                        .unwrap_or_default()
                        .has(mark)
                });
            format_selected_rows(self, ws, |row, ws| {
                let range = 0..row.text.len();
                if range.is_empty() {
                    return;
                }
                let _ = ws.set_mark(row.id, range, mark, on);
                let _ = row.refresh(ws);
            });
            return;
        }
        if self.marks_refused() {
            return;
        }
        if let Some(row) = self.line() {
            row.toggle_mark(ws, mark);
        }
    }

    fn toggle_script(&mut self, ws: &mut WorkspaceDoc, script: Script) {
        self.view_mut().desired_x = None;
        if self.body().has_row_selection() {
            let all_on = self
                .body()
                .rows
                .iter()
                .filter(|r| r.selected && row_takes_marks(r) && !r.text.is_empty())
                .all(|r| {
                    ws.style_in(r.id, 0..r.text.len())
                        .unwrap_or_default()
                        .script
                        == Some(script)
                });
            let next = (!all_on).then_some(script);
            format_selected_rows(self, ws, |row, ws| {
                let range = 0..row.text.len();
                if range.is_empty() {
                    return;
                }
                let _ = ws.set_script(row.id, range, next);
                let _ = row.refresh(ws);
            });
            return;
        }
        if self.marks_refused() {
            return;
        }
        if let Some(row) = self.line() {
            row.toggle_script(ws, script);
        }
    }

    fn cycle_highlight(&mut self, ws: &mut WorkspaceDoc) {
        self.view_mut().desired_x = None;
        if self.body().has_row_selection() {
            let names = theme::highlight_names();
            // One swatch for the whole selection: cycle from the first eligible
            // row so mixed highlights converge rather than each stepping alone.
            let current = self
                .body()
                .rows
                .iter()
                .filter(|r| r.selected && row_takes_marks(r))
                .find(|r| !r.text.is_empty())
                .and_then(|r| {
                    ws.style_in(r.id, 0..r.text.len())
                        .ok()
                        .and_then(|s| s.highlight)
                });
            let next = crate::paragraph::next_swatch(current.as_deref(), &names);
            format_selected_rows(self, ws, |row, ws| {
                let range = 0..row.text.len();
                if range.is_empty() {
                    return;
                }
                let _ = ws.set_highlight(row.id, range, next);
                let _ = row.refresh(ws);
            });
            return;
        }
        if self.marks_refused() {
            return;
        }
        if let Some(row) = self.line() {
            row.cycle_highlight(ws);
        }
    }

    fn clear_formatting(&mut self, ws: &mut WorkspaceDoc) {
        self.view_mut().desired_x = None;
        if self.body().has_row_selection() {
            format_selected_rows(self, ws, |row, ws| {
                let range = 0..row.text.len();
                if range.is_empty() {
                    return;
                }
                for mark in Mark::ALL {
                    let _ = ws.set_mark(row.id, range.clone(), mark, false);
                }
                let _ = ws.set_highlight(row.id, range.clone(), None);
                let _ = ws.set_size(row.id, range.clone(), None);
                let _ = ws.set_script(row.id, range, None);
                let _ = row.refresh(ws);
            });
            return;
        }
        if self.marks_refused() {
            return;
        }
        if let Some(row) = self.line() {
            row.clear_formatting(ws);
        }
    }

    fn set_align(&mut self, ws: &mut WorkspaceDoc, align: Align) {
        if self.body().has_row_selection() {
            let ids: Vec<_> = self
                .body()
                .rows
                .iter()
                .filter(|r| r.selected && row_takes_block_format(r))
                .map(|r| (r.id, r.align))
                .collect();
            if ids.is_empty() {
                return;
            }
            // Toggle off only when every eligible row is already this align.
            let next = (!ids.iter().all(|(_, a)| *a == align)).then_some(align);
            ws.checkpoint();
            ws.begin_group();
            for (id, _) in ids {
                let _ = ws.set_align(id, next);
                if let Some(row) = self.body_mut().row_mut(id) {
                    let _ = row.refresh(ws);
                    row.dirty = true;
                }
            }
            ws.checkpoint();
            return;
        }
        if self.title_focused()
            || self
                .body()
                .focused()
                .is_some_and(|row| row.kind == Kind::Table)
        {
            return;
        }
        if let Some(row) = self.line() {
            row.set_align(ws, align);
        }
    }

    /// Whether the caret is in the document's title row.
    ///
    /// **The gate every block-shaped operation asks**, beside the `*_enabled`
    /// view gates. A title has one step on the type scale and one kind, and
    /// both are its position rather than its metadata — so `Ctrl+Alt+1`, a
    /// `# ` shorthand, a fence and the block dropdown all have to stop here
    /// rather than writing `meta.heading` onto the document node and syncing a
    /// presentation choice to every peer.
    fn title_focused(&self) -> bool {
        self.body().focus_is_title()
    }

    /// Whether the caret is somewhere no inline mark may go.
    ///
    /// Two places, for two different reasons. A **title** is a name: bolding
    /// half of it is formatting a tab label and a tree row would draw as plain
    /// text anyway. A **code block** is coloured by the syntax engine from its
    /// text and its language, and that colour is never stored (ADR-44-code) —
    /// so a bold run inside one is a mark the highlighter paints straight over,
    /// which survives a round-trip and shows up nowhere.
    fn marks_refused(&self) -> bool {
        self.title_focused()
            || self
                .body()
                .focused()
                .is_some_and(|r| matches!(r.kind, Kind::Code | Kind::Table))
    }

    /// The focused block's heading level, when this view offers headings.
    ///
    /// The gate is here rather than on the action's [`Scope`], because it also
    /// has to cover the toolbar button, the `/` menu and the `#` shorthand —
    /// see `ViewState::headings_enabled`.
    ///
    /// [`Scope`]: crate::input::action::Scope
    fn set_heading(&mut self, ws: &mut WorkspaceDoc, heading: Option<Heading>) {
        if !self.view().headings_enabled {
            return;
        }
        if self.body().has_row_selection() {
            let ids: Vec<_> = self
                .body()
                .rows
                .iter()
                .filter(|r| r.selected && row_takes_block_format(r))
                .map(|r| (r.id, r.heading))
                .collect();
            if ids.is_empty() {
                return;
            }
            // Some(h) on rows already all at h clears them; None always clears.
            let next = match heading {
                None => None,
                Some(h) if ids.iter().all(|(_, cur)| *cur == Some(h)) => None,
                Some(h) => Some(h),
            };
            ws.checkpoint();
            ws.begin_group();
            for (id, _) in ids {
                let _ = ws.set_heading(id, next);
                if let Some(row) = self.body_mut().row_mut(id) {
                    let _ = row.refresh(ws);
                    row.dirty = true;
                }
            }
            ws.checkpoint();
            return;
        }
        if self.title_focused()
            || self
                .body()
                .focused()
                .is_some_and(|row| row.kind == Kind::Table)
        {
            return;
        }
        if let Some(row) = self.line() {
            row.set_heading(ws, heading);
        }
    }

    /// Turn the selected prose rows into one or more lists, preserving each
    /// contiguous sibling run as one ListBlock. Existing list items are first
    /// extracted from their containers, so formatting one row never changes
    /// unselected siblings merely because they share a ListBlock.
    ///
    /// Selection roots ensure selecting a parent and its children never moves a
    /// descendant twice. Non-contiguous Ctrl selections remain separate lists;
    /// joining them would reorder the unselected content between them.
    fn set_list_style(&mut self, ws: &mut WorkspaceDoc, style: ListStyle) {
        if !self.view().task_lists_enabled || self.title_focused() {
            return;
        }
        let focus = self.body().focused_id();
        let ids = self.body().selection_roots(ws);
        let mut selected = Vec::new();

        for id in ids {
            if !ws.read(id).is_ok_and(|node| node.kind == Kind::Paragraph) {
                continue;
            }
            let current = ws.parent(id).ok().and_then(|parent| match parent {
                Parent::Node(block) => ws.read(block).ok().and_then(|node| {
                    (node.kind == Kind::ListBlock)
                        .then_some((block, node.meta.list_style.unwrap_or(ListStyle::Bullet)))
                }),
                _ => None,
            });
            selected.push((id, current));
        }
        if selected.is_empty() {
            return;
        }
        // A list button is a toggle only when every selected row already has
        // that style. With a mixed selection it applies the requested style to
        // all selected rows, matching inline-formatting controls.
        let remove = selected
            .iter()
            .all(|(_, current)| current.is_some_and(|(_, current)| current == style));

        ws.checkpoint();
        ws.begin_group();
        // A fully selected list can be changed in place (or unwrapped once).
        // Besides preserving its invisible container identity, this avoids an
        // extract-per-row path for a large selection. Partial lists must split,
        // because their unselected siblings are outside the command's scope.
        let mut whole_blocks: Vec<(NodeId, ListStyle)> = Vec::new();
        for (_, current) in &selected {
            let Some((block, current_style)) = current else {
                continue;
            };
            if whole_blocks
                .iter()
                .any(|(whole_block, _)| whole_block == block)
            {
                continue;
            }
            let all_selected = ws.children(Parent::Node(*block)).is_ok_and(|children| {
                children
                    .iter()
                    .all(|child| selected.iter().any(|(id, _)| id == child))
            });
            if all_selected {
                whole_blocks.push((*block, *current_style));
            }
        }
        // Decide the complete set before mutating any block. Coalescing one
        // block can delete an adjacent selected container, so discovering and
        // changing in the same loop would make later selected items look like
        // partial-list items and extract them again.
        for (block, current_style) in &whole_blocks {
            if remove {
                let _ = ws.unwrap_list_block(*block);
            } else if *current_style != style {
                let _ = ws.set_list_style(*block, Some(style));
            }
        }
        if !remove {
            for (block, _) in &whole_blocks {
                if ws.read(*block).is_ok() {
                    let _ = ws.coalesce_list_block(*block);
                }
            }
        }
        // Extract only selected items. `extract_list_item` splits an outer list
        // around an interior item, preserving the visual order and restarting
        // a numbered suffix at one.
        for (id, current) in &selected {
            if current.is_some_and(|(block, _)| {
                !whole_blocks
                    .iter()
                    .any(|(whole_block, _)| *whole_block == block)
            }) {
                let _ = ws.extract_list_item(*id);
            }
        }

        if !remove {
            let mut runs: Vec<Vec<NodeId>> = Vec::new();
            let mut previous: Option<(Parent, usize)> = None;
            for (id, current) in &selected {
                if current.is_some_and(|(block, _)| {
                    whole_blocks
                        .iter()
                        .any(|(whole_block, _)| *whole_block == block)
                }) {
                    previous = None;
                    continue;
                }
                let (Ok(parent), Ok(index)) = (ws.parent(*id), ws.sibling_index(*id)) else {
                    continue;
                };
                if previous.is_some_and(|(p, at)| p == parent && at + 1 == index) {
                    runs.last_mut().expect("a previous list run").push(*id);
                } else {
                    runs.push(vec![*id]);
                }
                previous = Some((parent, index));
            }
            // Work bottom-up so one new container cannot shift the sibling
            // indices used to validate a later disjoint run.
            for run in runs.iter().rev() {
                let _ = ws.wrap_in_list_block_many(run, style);
            }
        }
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        if let Some(id) = focus {
            self.body_mut().focus_on(id);
        }
        self.view_mut().desired_x = None;
    }

    /// Toolbar path for a divider: drop a rule at the caret. An empty line
    /// becomes the divider; a line with text keeps that text and gets the
    /// divider after it, with a fresh empty paragraph beneath for the caret.
    /// The same thing the `/divider` slash command and the `---` shorthand do,
    /// reached from a button.
    fn insert_divider(&mut self, ws: &mut WorkspaceDoc) {
        if !self.view().dividers_enabled || self.title_focused() {
            return;
        }
        let (id, has_text) = match self.body().focused() {
            Some(row) => (row.id, !row.text.is_empty()),
            None => return,
        };
        ws.checkpoint();
        ws.begin_group();
        if has_text {
            let Some(divider) = Node::divider().ok() else {
                ws.checkpoint();
                return;
            };
            let Some(para) = Node::paragraph("").ok() else {
                ws.checkpoint();
                return;
            };
            let div = ws.insert_after(id, divider).unwrap_or(id);
            let next = ws.insert_after(div, para).unwrap_or(div);
            self.body_mut().rebuild(ws);
            self.body_mut().focus_on(next);
            self.place_caret(0);
        } else {
            let _ = self.replace_with_divider(ws, id);
        }
        ws.checkpoint();
    }

    /// Turn a picker's result into the attribute a pill draws.
    ///
    /// A date is a value and lives on the task. A contact is a *node* in the
    /// library, so the attribute is a reference to it — which is what lets a
    /// kanban group by assignee without parsing any text.
    fn attr_for(&mut self, ws: &mut WorkspaceDoc, spec: &PillSpec) -> Attr {
        match spec.kind {
            PillKind::Date => Attr::new(
                "due",
                AttrValue::Date(koru_core::time::DateOnly::new(
                    spec.date
                        .unwrap_or_else(|| chrono::Local::now().date_naive()),
                )),
            ),
            PillKind::Contact => match koru_workspace::contact(ws, &spec.label) {
                Some(node) => Attr::new("assignee", AttrValue::Ref(Ref::new(node))),
                // The library write failed; keep the name rather than the pill.
                None => Attr::new("assignee", AttrValue::Text(spec.label.clone())),
            },
            PillKind::Link => Attr::labelled(
                "link",
                AttrValue::Url(spec.url.clone().unwrap_or_default()),
                spec.label.clone(),
            ),
            // A document pill points at a real node, so the pill's caption is
            // the document's own title and follows it when it changes.
            PillKind::Document => match spec.document {
                Some(target) => Attr::labelled(
                    "document",
                    AttrValue::Document(koru_core::DocumentReference {
                        document: target.document,
                        node: target.node,
                    }),
                    spec.label.clone(),
                ),
                None => match koru_workspace::document(ws, &spec.label) {
                    Some(node) => Attr::new("document", AttrValue::Ref(Ref::new(node))),
                    // The library write failed; keep the name rather than the link.
                    None => Attr::new("document", AttrValue::Text(spec.label.clone())),
                },
            },
            // An attachment pill points at a workspace file document.
            PillKind::File => match spec.document {
                Some(target) => Attr::labelled(
                    "attachment",
                    AttrValue::Ref(Ref::in_doc(
                        koru_core::NodeId::from_uuid(target.document.as_uuid()),
                        DocRef::Document(target.document),
                    )),
                    spec.label.clone(),
                ),
                None => Attr::new("attachment", AttrValue::Text(spec.label.clone())),
            },
            // **No picker mints a run pill.** A run pill is written by the
            // container when a delegation is accepted and the run id comes
            // back — never by a `/` or `@` trigger, because there is nothing
            // for a person to pick: the run does not exist until it is
            // created. A `PillSpec` that claimed otherwise would put a
            // reference to nothing in somebody's document, so it becomes the
            // label as text, which is what every other unresolvable spec does.
            PillKind::Run => Attr::new(
                koru_core::attributes::ROLE_RUN,
                AttrValue::Text(spec.label.clone()),
            ),
            PillKind::Schedule | PillKind::SchedulePaused => Attr::new(
                koru_core::attributes::ROLE_SCHEDULE,
                AttrValue::Text(spec.label.clone()),
            ),
        }
    }

    /// Replace whatever the picker was standing in for with a pill: either the
    /// typed trigger text, or — when the picker was opened by clicking a pill —
    /// that pill's anchor.
    fn commit_pill(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger, spec: PillSpec) {
        self.view_mut().overlay = Overlay::None;

        // A rename changes the node's own display value rather than inserting
        // anything. Text-backed documents and blob-backed images store that
        // value in different places, but share the prompt and commit path.
        match trigger.anchor {
            Anchor::Rename(node) => {
                if !spec.label.is_empty() {
                    let _ = ws.set_text(node, &spec.label);
                    self.body_mut().rebuild(ws);
                }
                return;
            }
            Anchor::RenameImage(node) => {
                if !spec.label.is_empty() {
                    let _ = ws.set_name(node, Some(&spec.label));
                    self.body_mut().rebuild(ws);
                }
                return;
            }
            _ => {}
        }

        let resolved = match trigger.stable.as_ref() {
            Some(range) => match ws.resolve_text_range(range) {
                Ok(pair) => Some(pair),
                Err(_) => {
                    self.view_mut().notice = Some("that insertion point changed elsewhere".into());
                    return;
                }
            },
            None => None,
        };

        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let start = resolved
            .map(|(anchor, focus)| anchor.min(focus))
            .unwrap_or(trigger.start)
            .min(len);
        let end = match trigger.anchor {
            // Exactly the one anchor character; deleting it drops its attribute.
            Anchor::Pill => resolved
                .map(|(anchor, focus)| anchor.max(focus))
                .unwrap_or(start + ANCHOR_LEN)
                .min(len),
            // Everything typed since the trigger character.
            Anchor::TriggerText => resolved
                .map(|(anchor, focus)| anchor.max(focus))
                .unwrap_or(caret)
                .clamp(start, len),
            // A free-standing prompt replaces nothing, and neither does a
            // picker opened from a chip.
            Anchor::Insert | Anchor::Chip | Anchor::NewBlock { .. } => start,
            // Text that was selected when the prompt opened: the pill takes
            // its place.
            Anchor::Selection(fallback_end) => resolved
                .map(|(anchor, focus)| anchor.max(focus))
                .unwrap_or(fallback_end)
                .min(len),
            // Handled above; it never reaches the text.
            Anchor::Rename(_) | Anchor::RenameImage(_) | Anchor::PageIcon(_) => return,
        };
        if !valid_overlay_span(ws, &trigger, start, end) {
            self.view_mut().notice = Some("that insertion point changed elsewhere".into());
            return;
        }

        self.body_mut().focus_on(trigger.node);
        let attr = self.attr_for(ws, &spec);
        let id = trigger.node;
        ws.checkpoint();
        if end > start {
            let _ = ws.delete_text(id, start..end);
        }
        let _ = ws.insert_pill(id, start, &attr);

        // A pill is a word of its own, so leave a gap after it. Without one the
        // next thing typed runs straight into the pill, and a `/` there is not
        // at a word boundary — so the command menu would refuse to open in the
        // one place you have just proved you wanted it.
        //
        // Safe to sit next to: `PILL_KEY` is configured `ExpandType::None`, so
        // the anchor cannot absorb the space (see `configure_styles`).
        let after = start + ANCHOR_LEN;
        let gap = ws
            .read(id)
            .ok()
            .and_then(|node| {
                node.content
                    .as_text()
                    .map(|t| t[after.min(t.len())..].starts_with(' '))
            })
            .unwrap_or(false);
        if !gap {
            let _ = ws.insert_text(id, after, " ");
        }

        self.body_mut().refresh(ws, id);
        // Past the gap either way, so re-picking a pill that already had one
        // does not stack up spaces or strand the caret behind them.
        self.place_caret(after + 1);
        self.view_mut().desired_x = None;
    }

    /// Replace the picker's preserved range with a Unicode emoji sequence.
    /// The sequence stays ordinary text, and `replace_text` makes the delete +
    /// insert one Loro undo step even for a multi-codepoint ZWJ emoji.
    fn commit_unicode(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger, value: &str) {
        if emojis::get(value).is_none() {
            self.view_mut().notice = Some("unsupported emoji selection".into());
            return;
        }
        let resume_name = match &mut self.view_mut().overlay {
            Overlay::Emoji(picker) => picker.resume_name.take(),
            _ => None,
        };
        self.view_mut().overlay = Overlay::None;
        if let Anchor::PageIcon(node) = trigger.anchor {
            ws.checkpoint();
            let saved = ws.set_icon(node, Some(value)).is_ok();
            if !saved {
                self.view_mut().notice = Some("could not set this page icon".into());
            }
            self.body_mut().rebuild(ws);
            if let Some(mut prompt) = resume_name {
                if saved {
                    prompt.page_icon = Some(value.to_owned());
                }
                self.view_mut().overlay = Overlay::Name(prompt);
            }
            return;
        }
        let range = match trigger.stable.as_ref() {
            Some(stable) => match ws.resolve_text_range(stable) {
                Ok((anchor, focus)) => anchor.min(focus)..anchor.max(focus),
                Err(_) => {
                    self.view_mut().notice = Some("that insertion point changed elsewhere".into());
                    return;
                }
            },
            None => {
                let end = match trigger.anchor {
                    Anchor::Selection(end) => end,
                    Anchor::TriggerText => self
                        .body()
                        .row(trigger.node)
                        .map(Paragraph::caret)
                        .unwrap_or(trigger.start),
                    _ => trigger.start,
                };
                trigger.start.min(end)..trigger.start.max(end)
            }
        };
        if !valid_overlay_span(ws, &trigger, range.start, range.end) {
            self.view_mut().notice = Some("that insertion point changed elsewhere".into());
            return;
        }
        // The query was a typing group of its own. End it before the compound
        // replacement so one undo restores `:query` rather than erasing both.
        ws.checkpoint();
        let caret = match ws.replace_text(trigger.node, range, value) {
            Ok(caret) => caret,
            Err(_) => {
                self.view_mut().notice = Some("could not insert emoji here".into());
                return;
            }
        };
        self.body_mut().focus_on(trigger.node);
        self.body_mut().refresh(ws, trigger.node);
        self.place_caret(caret);
        self.view_mut().desired_x = None;
    }

    /// If the word ending at `caret` is a URL, turn it into a link pill in place.
    ///
    /// Called after a whitespace terminator is typed and before a split (Enter).
    /// Both the pill's caption and its target are the URL itself. The caret is
    /// taken explicitly because the row's parley selection is only brought up to
    /// date at reflow, and this runs before then. Returns whether a link was made.
    fn autolink_at(&mut self, ws: &mut WorkspaceDoc, caret: usize) -> bool {
        // A chat message keeps a URL as the text that was typed.
        if !self.view().policy.rewrites_typing() {
            return false;
        }
        let Some((id, text)) = self.body().focused().map(|row| (row.id, row.text.clone())) else {
            return false;
        };
        let Some((range, url)) = trigger::url_ending_at(&text, caret) else {
            return false;
        };
        let (start, end) = (range.start, range.end);
        let url_len = end - start;

        // Linkification is its own undo step: one Ctrl+Z brings the raw URL
        // back. The leading checkpoint closes the typing run; `begin_group` +
        // `checkpoint` bracket the delete + insert as one further step.
        ws.checkpoint();
        let attr = self.attr_for(
            ws,
            &PillSpec::link(url.clone(), link_prompt::normalize_url(&url)),
        );
        ws.begin_group();
        let ok = ws.delete_text(id, start..end).is_ok() && ws.insert_pill(id, start, &attr).is_ok();
        ws.checkpoint();
        if !ok {
            return false;
        }
        self.body_mut().refresh(ws, id);
        // The URL bytes were replaced by the anchor; the caret — which sat at or
        // after the terminator the user typed — shifts with everything past the
        // URL. The terminator itself stays as text after the pill.
        self.place_caret(caret - url_len + ANCHOR_LEN);
        self.view_mut().desired_x = None;
        true
    }

    // ------------------------------------------------------------ keyboard

    /// Returns true if a redraw is needed.
    ///
    /// Takes the decomposed key rather than a `winit::KeyEvent`: that type has a
    /// `platform_specific` field which cannot be constructed outside winit, so
    /// accepting it made the whole key map impossible to unit-test.
    fn focus_table_cell(&mut self, id: NodeId, table_row: usize, column: usize) -> bool {
        let Some(index) = self.body().rows.iter().position(|row| row.id == id) else {
            return false;
        };
        let Some(range) = self.body().rows[index]
            .table
            .as_ref()
            .and_then(|visual| crate::table::source_range(visual, table_row, column))
        else {
            return false;
        };
        self.body_mut().focus = index;
        let row = &mut self.body_mut().rows[index];
        row.selection =
            Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
            );
        self.view_mut().active = true;
        self.view_mut().table_cell = Some((id, table_row, column));
        self.view_mut().notice = Some(format!(
            "Editing table cell {}:{}; type to replace, Tab moves, Ctrl+Enter adds a paragraph below",
            table_row + 1,
            column + 1
        ));
        true
    }

    /// Switch the focused table between its canonical source and derived grid.
    /// Returning to the grid validates first, so malformed source remains
    /// visible with the same line-specific error as Escape.
    fn toggle_table_view(&mut self) -> bool {
        let Some(row) = self.body().focused().filter(|row| row.kind == Kind::Table) else {
            return false;
        };
        let id = row.id;
        let source = row.text.clone();
        let source_visible = self.view().table_source == Some(id)
            || row.table.as_ref().is_some_and(|table| table.grid.is_none());
        self.view_mut().table_cell = None;
        if source_visible {
            match crate::table::parse_source(&source) {
                Ok(_) => {
                    self.view_mut().table_source = None;
                    self.view_mut().notice = None;
                }
                Err(errors) => {
                    let error = &errors[0];
                    self.view_mut().table_source = Some(id);
                    self.view_mut().notice =
                        Some(format!("Table line {}: {}", error.line, error.message));
                    self.body_mut().focus_on(id);
                }
            }
        } else {
            self.view_mut().table_source = Some(id);
            self.view_mut().notice = Some(
                "Editing Markdown source; use Show table or Escape to validate the grid".into(),
            );
        }
        self.reflow();
        true
    }

    /// Apply a table-menu command to the focused grid. The parsed structure is
    /// temporary: every mutation serializes immediately back to the node's one
    /// canonical Markdown string, then the ordinary refresh rebuilds the grid.
    fn edit_table(&mut self, ws: &mut WorkspaceDoc, edit: crate::table::Edit) -> bool {
        let Some(row) = self.body().focused().filter(|row| row.kind == Kind::Table) else {
            return false;
        };
        let id = row.id;
        if edit == crate::table::Edit::ToggleView {
            return self.toggle_table_view();
        }
        let Some(mut parsed) = row
            .table
            .as_ref()
            .and_then(|table| table.grid.as_ref())
            .map(|grid| grid.parsed.clone())
        else {
            return false;
        };
        let (_, selected_row, selected_column) = self
            .view()
            .table_cell
            .filter(|(table, _, _)| *table == id)
            .unwrap_or((id, 1.min(parsed.rows.len()), 0));
        let mut target_row = selected_row;
        let mut target_column = selected_column.min(parsed.columns().saturating_sub(1));
        let changed = match edit {
            crate::table::Edit::InsertRowAbove => {
                let at = selected_row.saturating_sub(1).min(parsed.rows.len());
                koru_blocks::table::insert_row(&mut parsed, at);
                target_row = at + 1;
                true
            }
            crate::table::Edit::InsertRowBelow => {
                let at = selected_row.min(parsed.rows.len());
                koru_blocks::table::insert_row(&mut parsed, at);
                target_row = at + 1;
                true
            }
            crate::table::Edit::DeleteRow if selected_row > 0 => {
                let at = selected_row - 1;
                let removed = koru_blocks::table::delete_row(&mut parsed, at);
                target_row = if parsed.rows.is_empty() {
                    0
                } else {
                    (at + 1).min(parsed.rows.len())
                };
                removed
            }
            crate::table::Edit::InsertColumnBefore => {
                let at = selected_column.min(parsed.columns());
                koru_blocks::table::insert_column(&mut parsed, at);
                target_column = at;
                true
            }
            crate::table::Edit::InsertColumnAfter => {
                let at = (selected_column + 1).min(parsed.columns());
                koru_blocks::table::insert_column(&mut parsed, at);
                target_column = at;
                true
            }
            crate::table::Edit::DeleteColumn => {
                let removed = koru_blocks::table::delete_column(&mut parsed, selected_column);
                target_column = selected_column.min(parsed.columns().saturating_sub(1));
                removed
            }
            crate::table::Edit::ToggleView | crate::table::Edit::DeleteRow => false,
        };
        if !changed {
            return false;
        }
        let source = koru_blocks::table::serialize(&parsed);
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.set_text(id, &source);
        ws.checkpoint();
        self.body_mut().refresh(ws, id);
        self.reflow();
        self.focus_table_cell(id, target_row, target_column)
    }

    /// Grid traversal is projection behavior over the source ranges. No cell
    /// contents are copied into editor state: adding a final row serializes the
    /// parsed Markdown once, and focus always resolves back to a source range.
    fn table_grid_key(&mut self, ws: &mut WorkspaceDoc, key: &Key, mods: ModifiersState) -> bool {
        // A row selection means the table *block* is selected, not a cell.
        // Let plain Enter reach the block path below instead of reviving a
        // stale cell target left from an earlier click.
        if self.body().has_row_selection() {
            return false;
        }
        if mods.control_key() || mods.super_key() || mods.alt_key() {
            return false;
        }
        let backwards = mods.shift_key();
        let is_tab = matches!(key, Key::Named(NamedKey::Tab));
        let is_enter = matches!(key, Key::Named(NamedKey::Enter));
        if !is_tab && !is_enter {
            return false;
        }
        let Some((id, current_row, current_column)) = self.view().table_cell else {
            return false;
        };
        let Some(parsed) = self
            .body()
            .focused()
            .filter(|row| row.id == id && row.kind == Kind::Table)
            .and_then(|row| row.table.as_ref())
            .and_then(|visual| visual.grid.as_ref())
            .map(|grid| grid.parsed.clone())
        else {
            return false;
        };
        let columns = parsed.columns();
        if columns == 0 {
            return false;
        }
        let row_count = parsed.rows.len() + 1;
        let (target_row, target_column, add_row) = if is_tab {
            let current = current_row * columns + current_column;
            if backwards {
                let target = current.saturating_sub(1);
                (target / columns, target % columns, false)
            } else if current + 1 >= row_count * columns {
                (row_count, 0, true)
            } else {
                let target = current + 1;
                (target / columns, target % columns, false)
            }
        } else if backwards {
            (current_row.saturating_sub(1), current_column, false)
        } else if current_row + 1 >= row_count {
            (row_count, current_column, true)
        } else {
            (current_row + 1, current_column, false)
        };

        if add_row {
            let mut changed = parsed;
            let at = changed.rows.len();
            koru_blocks::table::insert_row(&mut changed, at);
            let source = koru_blocks::table::serialize(&changed);
            ws.checkpoint();
            ws.begin_group();
            let _ = ws.set_text(id, &source);
            ws.checkpoint();
            self.body_mut().refresh(ws, id);
            self.reflow();
        }
        self.focus_table_cell(id, target_row, target_column)
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if self.view().body_tone_strip.is_some() {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.view_mut().body_tone_strip = None;
                return true;
            }
            // Other keys dismiss the strip and fall through.
            self.view_mut().body_tone_strip = None;
        }
        if self.view_mut().overlay.is_open() {
            let outcome = self.overlay_key(ws, key, text, mods);
            // A move-mode picker's Commit/NewPage/Close never reach the pill
            // machinery: they name a destination, and the source is edited
            // only when the shell reports the destination write landed.
            let outcome = match self.route_move_outcome(outcome) {
                Ok(()) => return true,
                Err(outcome) => outcome,
            };
            match outcome {
                // `overlay_key` resolves these into the next panel — or a
                // direct commit, for a typed argument — itself.
                Outcome::Become(_) | Outcome::CommandArg(..) => {
                    // A commit changes the body (a heading, a divider, or a
                    // pill's node); settle it before this frame is painted.
                    self.settle();
                    return true;
                }
                Outcome::BecomePageIcon(node) => {
                    let resume = self.take_name_prompt();
                    self.open_page_icon_picker(node, resume);
                    return true;
                }
                Outcome::Consumed => return true,
                Outcome::Close => {
                    if !self.resume_name_prompt() {
                        self.view_mut().overlay = Overlay::None;
                    }
                    return true;
                }
                Outcome::Commit(spec) => {
                    let trigger = self.view_mut().overlay.trigger();
                    if let Some(t) = trigger {
                        self.commit_pill(ws, t, spec);
                    }
                    self.reflow();
                    self.scroll_to_caret();
                    return true;
                }
                Outcome::NewPage(title) => {
                    if title.is_empty() {
                        if let Some(trigger) = self.view().overlay.trigger() {
                            self.commit_unicode(ws, trigger, "");
                        } else {
                            self.view_mut().overlay = Overlay::None;
                        }
                    } else {
                        let trigger = self.view().overlay.trigger();
                        self.view_mut().pending_new_page_trigger = trigger;
                        self.view_mut().overlay = Overlay::None;
                    }
                    self.view_mut().wants_new_page = Some(title);
                    return true;
                }
                Outcome::SetLanguage(node, lang) => {
                    self.view_mut().overlay = Overlay::None;
                    let _ = ws.set_language(node, lang.as_deref());
                    self.body_mut().refresh(ws, node);
                    self.reflow();
                    return true;
                }
                outcome @ (Outcome::SetAttr(..)
                | Outcome::ClearAttr(..)
                | Outcome::AddTag(..)
                | Outcome::RemoveTag(..)
                | Outcome::CreateTag(..)
                | Outcome::JoinList(..)
                | Outcome::CreateList(..)) => {
                    let trigger = self.view().overlay.trigger();
                    self.attribute_outcome(ws, trigger, &outcome);
                    return true;
                }
                Outcome::CommitText(text) => {
                    let trigger = self.view_mut().overlay.trigger();
                    if let Some(trigger) = trigger {
                        self.commit_unicode(ws, trigger, &text);
                    }
                    self.reflow();
                    self.scroll_to_caret();
                    return true;
                }
                Outcome::Ignored => {}
            }
        }

        // Escape is the one-way exit from a table's Markdown projection. The
        // shared toggle still validates, so malformed source remains visible
        // with its line-specific error instead of disappearing behind a grid
        // that cannot represent it.
        let bare_escape = matches!(key, Key::Named(NamedKey::Escape))
            && !mods.control_key()
            && !mods.super_key()
            && !mods.alt_key()
            && !mods.shift_key();
        if self.view().active
            && bare_escape
            && self
                .body()
                .focused()
                .is_some_and(|row| row.kind == Kind::Table && row.table_source_visible)
        {
            return self.toggle_table_view();
        }

        if self.view().active && self.table_grid_key(ws, key, mods) {
            return true;
        }

        // **A selected pill outranks the key map**, and has to: Ctrl+Enter is
        // already Toggle done and Shift+Enter already opens a paragraph above,
        // so the three dispositions cannot be expressed as chords. It is asked
        // here, before `resolve`, rather than as an `Action` because the answer
        // depends on which *kind* of pill is selected, which a chord cannot say.
        if self.view().selected_pill.is_some()
            && let Some(handled) = self.pill_key(ws, key, text, mods)
        {
            return handled;
        }

        let action = keymap::resolve(key, text, mods);

        // **The composer policy is asked before anything acts.** A refused
        // chord is *unhandled* rather than swallowed, so it reaches whatever a
        // host binds it to — the same nothing a view that lacks the action
        // produces (`crate::policy`).
        if let Some(a) = action
            && !self.view().policy.allows(a)
        {
            return false;
        }

        // With no active line there is nothing to edit, so only the toggles that
        // belong to the window still apply.
        if !self.view_mut().active {
            return match action {
                Some(a) if a.works_without_a_caret() => self.act(ws, a),
                Some(a) => {
                    crate::clipboard::undo_dbg(format!(
                        "key dropped !active action={a:?} (Undo/Redo need works_without_a_caret or active)"
                    ));
                    koru_ui::keys::debug::write(format_args!(
                        "[Koru][input] editor key dropped=!active action={a:?}"
                    ));
                    false
                }
                None => {
                    koru_ui::keys::debug::write(format_args!(
                        "[Koru][input] editor key dropped=!active printable"
                    ));
                    false
                }
            };
        }

        let handled = match action {
            Some(a) => self.act(ws, a),
            // Anything that produced text and is not a control chord.
            None => match (text, koru_keys::is_chord(mods)) {
                (Some(t), false) if !t.chars().any(|c| c.is_control()) => {
                    // `@` over a row or text selection opens the move picker instead of
                    // typing — asked *before* the clear below, which is what
                    // makes every other keystroke land in the focused row
                    // with the marks gone.
                    if t == "@"
                        && self.view().policy.rewrites_typing()
                        && !self.view().overlay.is_open()
                        && self.has_move_selection()
                        && self.open_move_selection_picker(ws)
                    {
                        return true;
                    }
                    self.body_mut().clear_selection();
                    self.type_text(ws, t);
                    true
                }
                _ => false,
            },
        };

        if handled {
            self.settle();
            self.sync_trigger_range(ws);
        }
        handled
    }

    /// Do one thing, whatever asked for it — a chord, a toolbar button, or a
    /// view that resolved the chord itself.
    ///
    /// Returns whether it was handled, so a caller can fall through. Everything
    /// here is shared between the two views; the structural arms are the
    /// outline's and stay only because its methods are still `impl Editor`.
    fn act(&mut self, ws: &mut WorkspaceDoc, action: Action) -> bool {
        // Plain Enter on a selected rendered table is a block action. Preserve
        // that one selection until `enter_on_block` identifies the target; the
        // helper clears it after inserting the paragraph.
        let enter_on_selected_table = action == Action::SplitLine
            && self.body().focused().is_some_and(|row| {
                row.kind == Kind::Table && row.selected && !row.table_source_visible
            });
        // A row selection belongs to the caret that made it: anything neither
        // extending it nor acting on the whole of it ends it.
        if !action.keeps_row_selection() && !enter_on_selected_table {
            self.body_mut().clear_selection();
        }
        // A notice answers the thing that just happened, so the next thing the
        // user does replaces it. Undo and redo are excepted because they are
        // what *set* it — pressing Ctrl+Z twice should not make the reason
        // disappear on the second press.
        if !matches!(action, Action::Undo | Action::Redo) {
            self.view_mut().notice = None;
        }
        // Paste undo/redo hints: keep pastes across either paste action (the
        // shell pushes after rich/image paste); clear both stacks on other
        // edits. A new paste clears redos only.
        if !matches!(
            action,
            Action::Undo | Action::Redo | Action::Paste | Action::PastePlain
        ) {
            self.view_mut().last_image_pastes.clear();
            self.view_mut().last_image_redos.clear();
            self.view_mut().paste_redo_exhausted = false;
        } else if matches!(action, Action::Paste | Action::PastePlain) {
            self.view_mut().last_image_redos.clear();
            self.view_mut().paste_redo_exhausted = false;
        }
        match action {
            // -------------------------------------------------------- caret
            // The pill stop is asked **before** `disarm`, which is what lets go
            // of it: asking after would clear the selection this press was
            // supposed to step out of, and every press would re-select the same
            // pill instead of walking past it.
            Action::MoveLeft { by_word, extend } => {
                if !self.step_onto_pill(-1, by_word, extend) {
                    self.disarm();
                    self.move_horizontal(ws, -1, by_word, extend);
                }
            }
            Action::MoveRight { by_word, extend } => {
                if !self.step_onto_pill(1, by_word, extend) {
                    self.disarm();
                    self.move_horizontal(ws, 1, by_word, extend);
                }
            }
            Action::MoveUp { extend } => {
                self.disarm();
                self.move_vertical(ws, -1, extend);
            }
            Action::MoveDown { extend } => {
                self.disarm();
                self.move_vertical(ws, 1, extend);
            }
            Action::MoveParagraph(dir) => {
                self.disarm();
                self.move_paragraph(ws, dir);
            }
            Action::TogglePageTitle => {
                self.disarm();
                self.toggle_page_title(ws);
            }
            Action::PageUp { extend } => {
                self.disarm();
                self.move_page(ws, -1, extend);
            }
            Action::PageDown { extend } => {
                self.disarm();
                self.move_page(ws, 1, extend);
            }
            Action::LineStart { extend } => {
                self.disarm();
                if let Some(row) = self.body_mut().focused_mut() {
                    let sel = row.selection;
                    let next = sel.line_start(&row.layout, extend);
                    row.set_selection_from_layout(next);
                }
                self.view_mut().desired_x = None;
            }
            Action::LineEnd { extend } => {
                self.disarm();
                if let Some(row) = self.body_mut().focused_mut() {
                    let sel = row.selection;
                    let next = sel.line_end(&row.layout, extend);
                    row.set_selection_from_layout(next);
                }
                self.view_mut().desired_x = None;
            }
            Action::SelectAll => {
                let whole_block_is_selected = self.body().focused().is_some_and(|row| {
                    let range = row.range();
                    range.start == 0 && range.end == row.text.len()
                });
                if self.body().has_row_selection() || whole_block_is_selected {
                    self.body_mut().select_all_rows();
                } else if let Some(row) = self.body_mut().focused_mut() {
                    row.select_all();
                }
            }
            Action::CollapseSelection => {
                // Escape in an open diagram text closes it first: the block
                // is a picture again (or its text and reason), still focused.
                if self.view().diagram_text_open.is_some()
                    && self.view().diagram_text_open == self.body().focused_id()
                {
                    self.close_diagram_text();
                }
                if let Some(row) = self.body_mut().focused_mut() {
                    row.selection = row.selection.collapse();
                }
            }

            // --------------------------------------------------------- text
            Action::SplitLine => {
                if !self.enter_on_block(ws) {
                    self.split_line(ws);
                }
            }
            Action::SplitLineAbove => {
                if !self.enter_block_above(ws) {
                    self.split_line(ws);
                }
            }
            Action::InsertParagraphAbove => {
                return self.insert_paragraph_before_focused(ws);
            }
            // A multi-row selection is deleted whole, the same as Ctrl+D,
            // rather than clearing the marks and deleting a character in the
            // focused row.
            Action::DeleteBack { .. } | Action::DeleteForward { .. }
                if self.body().has_row_selection() =>
            {
                return self.delete_rows(ws, true);
            }
            Action::DeleteBack { by_word } => self.delete_selection_or(ws, true, by_word),
            Action::DeleteForward { by_word } => self.delete_selection_or(ws, false, by_word),
            Action::InsertTab => self.type_text(ws, "\t"),
            Action::DeleteRows => return self.delete_rows(ws, true),

            // --------------------------------------------------- clipboard
            Action::Copy => {
                // Single focused image only — a multi-row selection that happens
                // to end on a picture must go through the row path so text and
                // every image leave together.
                if !self.body().has_row_selection() && self.arm_copy_focused_image() {
                    return true;
                }
                return crate::clipboard::copy(self, ws);
            }
            Action::Cut => {
                // Arm the bitmap first; delete only after the shell writes it
                // (`pending_cut_image`), or external apps see an empty clipboard.
                if !self.body().has_row_selection() && self.arm_copy_focused_image() {
                    if let Some(id) = self.body().focused_id() {
                        self.view_mut().pending_cut_image = Some(id);
                    }
                    return true;
                }
                return crate::clipboard::cut(self, ws);
            }
            // A composer takes both pastes as its own text: lines at the caret,
            // never rows, marks or links (`crate::policy`).
            Action::Paste | Action::PastePlain
                if self.view().policy == crate::policy::Policy::Composer =>
            {
                return crate::clipboard::paste_as_lines(self, ws);
            }
            Action::Paste => return crate::clipboard::paste(self, ws),
            Action::PastePlain => return crate::clipboard::paste_unformatted(self, ws),

            // --------------------------------------------------- formatting
            Action::ToggleMark(mark) => self.toggle_mark(ws, mark),
            Action::SetScript(script) => self.toggle_script(ws, script),
            Action::CycleHighlight => self.cycle_highlight(ws),
            Action::SetAlign(align) => self.set_align(ws, align),
            Action::SetHeading(heading) => self.set_heading(ws, heading),
            Action::SetListStyle(style) => self.set_list_style(ws, style),
            Action::CodeBlock => self.toggle_code_block(ws),
            Action::InsertDivider => self.insert_divider(ws),
            Action::ClearFormatting => self.clear_formatting(ws),

            // ----------------------------------------------------- document
            Action::Undo | Action::Redo => {
                // Prefer Loro. Live image paste can report moved=true while
                // leaving nodes in place — then inverse-delete the paste batch
                // (one Loro step redo can reverse). No parallel redo stack.
                let want_redo = action == Action::Redo;
                if want_redo {
                    if self.redo_paste_batch(ws) {
                        return true;
                    }
                } else if self.undo_paste_batch(ws) {
                    return true;
                }
                let before = self.body_mut().ids();
                let before_u = ws.can_undo();
                let before_r = ws.can_redo();
                let had = if want_redo { before_r } else { before_u };
                let img_before = self
                    .body()
                    .rows
                    .iter()
                    .filter(|r| r.kind == koru_core::node::Kind::Image)
                    .count();
                crate::clipboard::undo_dbg(format!(
                    "action={action:?} active={} before can_undo={before_u} can_redo={before_r} had={had} image_rows={img_before} paste_stack={}",
                    self.view().active,
                    self.view().last_image_pastes.len()
                ));
                let result = if want_redo { ws.redo() } else { ws.undo() };
                match result {
                    Ok(true) => {
                        self.body_mut().rebuild(ws);
                        self.disarm();
                        self.follow_history(&before);
                        if let Some((node, at)) =
                            (!want_redo).then(|| ws.take_popped_undo_caret()).flatten()
                        {
                            if !self.select_text_range(node, at..at, None)
                                && self.body().root == Some(node)
                                && !self.body().title_row
                            {
                                self.start_rename_selection(ws, at..at);
                            }
                        }
                        self.view_mut().notice = None;
                        let img_after = self
                            .body()
                            .rows
                            .iter()
                            .filter(|r| r.kind == koru_core::node::Kind::Image)
                            .count();
                        crate::clipboard::undo_dbg(format!(
                            "moved=true after can_undo={} can_redo={} image_rows={img_after}",
                            ws.can_undo(),
                            ws.can_redo()
                        ));
                    }
                    Ok(false) => {
                        let after_u = ws.can_undo();
                        let after_r = ws.can_redo();
                        crate::clipboard::undo_dbg(format!(
                            "moved=false after can_undo={after_u} can_redo={after_r} had={had}"
                        ));
                        if had {
                            self.view_mut().notice = Some(if want_redo {
                                "can't redo this step — it conflicts with a change from another window"
                                    .into()
                            } else {
                                "can't undo this step — it conflicts with a change from another window"
                                    .into()
                            });
                        }
                    }
                    Err(e) => {
                        crate::clipboard::undo_dbg(format!("err {e:?}"));
                        self.view_mut().notice = Some(format!("undo failed ({e})"));
                    }
                }
            }
            Action::OpenLinkPrompt => {
                let Some((node, range)) = self.target() else {
                    return false;
                };
                // What was selected seeds the prompt, and the pill takes its
                // place on commit. With nothing selected there is nothing to
                // replace, and the pill is simply inserted at the caret.
                let selected = self
                    .body()
                    .focused()
                    .map(|row| row.text[range.clone()].to_owned())
                    .unwrap_or_default();
                let trigger = Trigger {
                    kind: TriggerKind::Slash,
                    start: range.start,
                    node,
                    anchor: match range.is_empty() {
                        true => Anchor::Insert,
                        false => Anchor::Selection(range.end),
                    },
                    stable: ws.capture_text_range(node, range.start, range.end).ok(),
                };
                let (engine, overlay) = {
                    let view = self.view_mut();
                    (&mut view.engine, &mut view.overlay)
                };
                *overlay = trigger::open_link_prompt_for(engine, trigger, &selected);
            }
            Action::OpenEmojiPicker => {
                let Some((node, range)) = self.target() else {
                    return false;
                };
                let trigger = Trigger {
                    kind: TriggerKind::Colon,
                    start: range.start,
                    node,
                    anchor: if range.is_empty() {
                        Anchor::Insert
                    } else {
                        Anchor::Selection(range.end)
                    },
                    stable: ws.capture_text_range(node, range.start, range.end).ok(),
                };
                let view = self.view_mut();
                view.overlay = Overlay::Emoji(EmojiPicker::standalone(&mut view.engine, trigger));
            }
            // ---------------------------------------------------- structure
            //
            // Not this layer's. Indenting and reordering are the outline's,
            // and `TaskOutline::act` claims them before a key
            // reaches here. Returning `false` is what makes a document ignore
            // them rather than perform them on prose.
            //
            // Named rather than caught by a guard, because a guard would make
            // the match non-exhaustive and cost the compiler check that every
            // *shared* action is handled — which is the one worth keeping.
            Action::ToggleDone => {
                // The outline claims ToggleDone for ticking a task before a key
                // reaches here. In a document it is Ctrl+Enter: a code block
                // moves to a following paragraph (reusing one when present), a
                // table and prose create a fresh paragraph immediately below.
                if self.body().focused().is_some_and(|r| r.kind == Kind::Code) {
                    self.exit_code_block(ws);
                    return true;
                }
                if let Some(table) = self
                    .body()
                    .focused()
                    .filter(|row| row.kind == Kind::Table)
                    .map(|row| row.id)
                {
                    return self.insert_paragraph_after_block(ws, table);
                }
                if let Some(paragraph) = self
                    .body()
                    .focused()
                    .filter(|row| row.kind == Kind::Paragraph)
                    .map(|row| row.id)
                {
                    return self.insert_paragraph_after_block(ws, paragraph);
                }
                // `Scope::Structural` is this tree's name for what the branch
                // this arrived on called `Outline` — the realm is gone (ADR-90)
                // and the scope was renamed with it.
                debug_assert_eq!(action.scope(), Scope::Structural);
                return false;
            }
            // **The box a row draws, from the keyboard** — the one call the
            // press on that box makes, reached the one way it can be reached
            // without a pointer or a screen reader (Q-T2).
            //
            // It asks [`Paragraph::is_task`] and nothing else, which is the
            // same predicate `checkbox_rect` asks before `mouse_down` reaches
            // `set_done`: a row that draws a box can be ticked and a row that
            // draws none cannot, whatever kind it is. **What ticking means is
            // decided in the domain** — `set_done` runs the lifecycle when
            // `tasks::checkbox_completes` says the box *is* completion and
            // writes the checkbox otherwise — so this arm does not get to have
            // an opinion about tasks.
            //
            // **A row with no box is refused rather than surprised.** `false`
            // leaves the key unhandled, which reaches nothing else (no layer
            // above the editor binds Enter) and so is the same nothing a press
            // on empty space produces. Answering `true` would claim a key that
            // did nothing; doing something else on prose would make one chord
            // mean two things.
            Action::TickCheckbox => {
                let Some((id, done)) = self
                    .body()
                    .focused()
                    .filter(|row| row.is_task())
                    .map(|row| (row.id, row.done))
                else {
                    return false;
                };
                self.body_mut().set_done(ws, id, !done);
            }
            // **Only a drawn diagram has a picture to save**, and only while
            // it is showing it: a paragraph, a block whose text is open, a
            // placeholder or a block that does not draw all answer `false`,
            // the `TickCheckbox` rule, so the chord is left unhandled rather
            // than claimed for nothing. The same test the menu applies before
            // it offers the row; the shell performs the save.
            Action::SaveDiagramAs => {
                let Some(node) = self
                    .body()
                    .focused()
                    .filter(|row| crate::diagram::is_diagram_row(row))
                    .map(|row| row.id)
                else {
                    return false;
                };
                let view = self.view();
                let drawn = view.diagram_text_open != Some(node)
                    && matches!(
                        view.diagrams.get(&node).map(|entry| &entry.state),
                        Some(crate::diagram::DiagramState::Drawn(_))
                    );
                if !drawn {
                    return false;
                }
                self.view_mut().wants_save_diagram_as = Some(SaveDiagramAs { node });
            }
            Action::ToggleHideCompleted | Action::Reorder(_) | Action::Indent(_) => {
                debug_assert_eq!(action.scope(), Scope::Structural);
                return false;
            }

            // --------------------------------------------------------- view
            // Sharing belongs to the document, not to a view of it, so the shell
            // performs it against the `Workspace` after this returns.
            Action::ToggleSharing => self.view_mut().wants_sharing_toggle = true,
        }
        true
    }

    /// What every handled key does afterwards: rebuild the layouts, re-filter
    /// any open picker, and bring the caret into view.
    ///
    /// Public because a view that handles a key itself has to run it too — a
    /// caret is only placed after reflow, never during an edit.
    fn settle(&mut self) {
        self.reflow();
        let (body, view) = self.parts();
        view.overlay.refresh(body);
        if self.body().focused().is_some_and(|row| {
            row.image.is_some()
                || row.kind == koru_core::node::Kind::Divider
                || row.diagram_shows_picture()
        }) {
            self.scroll_row_into_view(self.body().focus);
        } else {
            self.scroll_to_caret();
        }
        self.sync_diagram_controls_focus(Instant::now());
    }

    /// Refresh a live typed trigger's CRDT endpoints after a local edit. The
    /// overlay keeps byte offsets for layout, but only this range survives a
    /// subsequent peer merge safely.
    fn sync_trigger_range(&mut self, ws: &WorkspaceDoc) {
        let Some(trigger) = self.view().overlay.trigger() else {
            return;
        };
        if trigger.anchor != Anchor::TriggerText {
            return;
        }
        let Some(row) = self.body().row(trigger.node) else {
            return;
        };
        let start = trigger
            .stable
            .as_ref()
            .and_then(|range| ws.resolve_text_position(&range.anchor).ok())
            .unwrap_or(trigger.start);
        let focus = row.caret();
        let Some(stable) = ws.capture_text_range(trigger.node, start, focus).ok() else {
            return;
        };
        if let Some(trigger) = self.view_mut().overlay.trigger_mut() {
            trigger.start = start;
            trigger.stable = Some(stable);
        }
    }

    /// `#` at the start of a line, then a space, becomes a heading.
    ///
    /// The hashes are **removed**, not kept: there are no markdown delimiters in
    /// the text here — a heading is `NodeMeta.heading`, the same way emphasis is
    /// a Loro mark, and anything that greps for `#` is looking at a model this
    /// codebase does not have. This is a typing shorthand for the toolbar
    /// button, and the only place `#` is read at all.
    ///
    /// Tested *before* the space is inserted, so it is one delete rather than an
    /// insert and a delete. `false` means it was an ordinary space and the
    /// caller should go on and insert it.
    fn heading_shorthand(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if !self.view().headings_enabled {
            return false;
        }
        let Some(row) = self.body().focused() else {
            return false;
        };
        let range = row.range();
        let n = range.start;
        if !range.is_empty()
            || row.heading.is_some()
            || !(1..=6).contains(&n)
            || n > row.text.len()
            || !row.text[..n].bytes().all(|b| b == b'#')
        {
            return false;
        }

        let (id, level) = (row.id, Heading::from_level(n as i64));
        // One undo step, so a single Ctrl+Z puts the hashes back *and* takes the
        // heading off — which is what every editor with this shorthand does.
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.delete_text(id, 0..n);
        let _ = ws.set_heading(id, level);
        ws.checkpoint();
        self.body_mut().refresh(ws, id);
        self.place_caret(0);
        true
    }

    /// `> ` at the start of an ordinary paragraph becomes quoted prose. The
    /// marker is a typing gesture, not stored content: `Kind::Quote` is the
    /// semantic value used by rendering and clipboard export.
    fn quote_shorthand(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if !self.view().quotes_enabled {
            return false;
        }
        let Some(row) = self.body().focused() else {
            return false;
        };
        if row.kind != Kind::Paragraph
            || row.heading.is_some()
            || row.text != ">"
            || !row.selection.is_collapsed()
            || row.caret() != 1
        {
            return false;
        }
        let id = row.id;
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.delete_text(id, 0..1);
        let _ = ws.set_kind(id, Kind::Quote);
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(0);
        true
    }

    /// `[] ` at the start of a line wraps the current row into a
    /// `Kind::ListBlock` with `Checkbox` style. The `[]` is removed — the
    /// list block is the container, and the row becomes its first item.
    ///
    /// Like the heading shorthand, tested *before* the space is inserted. Text
    /// after the caret remains the item's content, so prefixing existing prose
    /// works too. `false` means it was an ordinary space.
    fn task_list_shorthand(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if !self.view().task_lists_enabled {
            return false;
        }
        let Some(row) = self.body().focused() else {
            return false;
        };
        let range = row.range();
        if row.kind != Kind::Paragraph
            || row.heading.is_some()
            || !range.is_empty()
            || range.start > row.text.len()
        {
            return false;
        }
        // `[]` (two bytes) is the marker; the space the caller is about to type
        // seals it. `[ ]` (with a space) is also accepted, the common variant.
        let marker = &row.text[..range.start];
        let is_marker = marker == "[]" || marker == "[ ]";
        if !is_marker {
            return false;
        }
        let (id, marker_len, current) = (row.id, marker.len(), row.list_style);
        self.apply_typed_list_marker(ws, id, marker_len, current, koru_core::ListStyle::Checkbox);
        true
    }

    /// `* `/`- `/`+ ` → Bullet, `1.`/`12.` → Numbered, `a.` → Letter.
    /// Each is a marker typed at the start of a line, sealed by the space the
    /// caller is about to insert. Wraps the current row into a ListBlock.
    fn list_marker_shorthand(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if !self.view().task_lists_enabled {
            return false;
        }
        let Some(row) = self.body().focused() else {
            return false;
        };
        let range = row.range();
        if row.kind != Kind::Paragraph
            || row.heading.is_some()
            || !range.is_empty()
            || range.start > row.text.len()
        {
            return false;
        }
        // Only the prefix before the caret is syntax. Existing prose after the
        // caret survives: typing `- ` at the start of "two" briefly produces
        // `-two`, then removes the marker and lists the unchanged word.
        let marker = &row.text[..range.start];
        let style = match marker.as_bytes() {
            // Bullet: `*`, `-`, `+`
            [b'*' | b'-' | b'+'] => Some(koru_core::ListStyle::Bullet),
            // Numbered: one or more digits then `.`
            rest if rest.len() >= 2 && *rest.last().unwrap() == b'.' => {
                let digits = &rest[..rest.len() - 1];
                if !digits.is_empty() && digits.iter().all(|b| b.is_ascii_digit()) {
                    Some(koru_core::ListStyle::Numbered)
                } else {
                    None
                }
            }
            _ => None,
        };
        // Also check `a.` for Letter style — but only single-char letters to
        // avoid matching regular words. This is the most ambiguous shorthand,
        // kept because the user asked for it.
        let style = style.or_else(|| {
            let lower = marker.to_ascii_lowercase();
            if lower.len() == 2 && lower.ends_with('.') && lower.as_bytes()[0].is_ascii_lowercase()
            {
                Some(koru_core::ListStyle::Letter)
            } else {
                None
            }
        });
        let Some(style) = style else {
            return false;
        };
        let (id, marker_len, current) = (row.id, marker.len(), row.list_style);
        self.apply_typed_list_marker(ws, id, marker_len, current, style);
        true
    }

    /// Commit a typed list marker without ever nesting an item merely because
    /// it was already in another list. A different marker style extracts just
    /// this row and re-wraps it beside the surrounding list fragments; typing
    /// the marker it already has simply removes the literal marker.
    fn apply_typed_list_marker(
        &mut self,
        ws: &mut WorkspaceDoc,
        id: NodeId,
        marker_len: usize,
        current: Option<ListStyle>,
        style: ListStyle,
    ) {
        ws.checkpoint();
        ws.begin_group();
        let _ = ws.delete_text(id, 0..marker_len);
        if current != Some(style) {
            if current.is_some() {
                let _ = ws.extract_list_item(id);
            }
            let _ = ws.wrap_in_list_block(id, style);
        }
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(0);
    }

    /// `---` on an otherwise-empty line becomes a divider.
    ///
    /// The sibling of [`heading_shorthand`](Self::heading_shorthand): the two
    /// dashes already typed are the trigger, the third is the one this intercepts
    /// (before it reaches the document), so the conversion is one delete rather
    /// than an insert and a delete. The line must be a paragraph with no heading
    /// and nothing but the two dashes on it — `--` mid-sentence is an em-dash
    /// waiting for its pair, not a rule. `false` means it was an ordinary dash.
    fn divider_shorthand(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if !self.view().dividers_enabled {
            return false;
        }
        let Some(row) = self.body().focused() else {
            return false;
        };
        if row.kind != koru_core::node::Kind::Paragraph
            || row.heading.is_some()
            || row.text != "--"
            || !row.selection.is_collapsed()
            || row.caret() != row.text.len()
        {
            return false;
        }
        let id = row.id;
        // One undo step, so a single Ctrl+Z removes the divider and puts the two
        // dashes back where they were.
        ws.checkpoint();
        ws.begin_group();
        let _ = self.replace_with_divider(ws, id);
        ws.checkpoint();
        true
    }

    /// Insert typed text, then arm an overlay if it was a trigger character.
    fn type_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        // A document name never takes a literal tab (Tab is swallowed in the
        // document keymap; this covers InsertTab and any other path).
        if text == "\t" && self.title_focused() {
            return;
        }
        // Selecting text and pressing `` ` `` wraps it in inline code — the
        // praised gesture from GitHub/Obsidian/Slack — rather than replacing the
        // selection with a backtick. Toggling: press it again on an all-code
        // selection to clear it.
        if text == "`" && self.view().policy.rewrites_typing() {
            if let Some((node, range)) = self.target() {
                if !range.is_empty() {
                    let all_code = ws.marks_in(node, range.clone()).unwrap_or_default().code;
                    ws.checkpoint();
                    let _ = ws.set_mark(node, range.clone(), Mark::Code, !all_code);
                    self.body_mut().refresh(ws, node);
                    self.place_caret(range.start);
                    self.reflow();
                    return;
                }
            }
        }
        // A divider carries no text: keystrokes that reach it do nothing rather
        // than silently logging a `NonEmptyText` rejection from the model. A
        // diagram showing its picture has text, but none a person can see:
        // typing into it blind would change a drawing's source without
        // showing a letter, so it does nothing too — Enter opens the text.
        if self
            .body()
            .focused()
            .is_some_and(|r| r.kind == koru_core::node::Kind::Divider || r.diagram_shows_picture())
        {
            return;
        }
        // The markdown fence waits for its language. A third backtick stays
        // text, so ```` ```mermaid ```` can be typed whole; a space at the end
        // of a line that is exactly a fence opens the code block, the same
        // moment `## ` makes a heading. Enter does the same through the split
        // path. With a selection the space replaces it instead, as anywhere.
        if text == " "
            && self.view().code_enabled
            && !self.title_focused()
            && self.body().focused().is_some_and(|r| r.range().is_empty())
            && self.try_fence(ws)
        {
            return;
        }
        // Before the space reaches the document: `## ` is a heading, `[] ` is a
        // task list, `* ` is a bullet list.
        if text == " "
            && !self.title_focused()
            && (self.heading_shorthand(ws)
                || self.quote_shorthand(ws)
                || self.task_list_shorthand(ws)
                || self.list_marker_shorthand(ws))
        {
            return;
        }
        // A third dash on an otherwise empty line is a divider, not the last
        // character of an em-dash pair.
        if text == "-" && !self.title_focused() && self.divider_shorthand(ws) {
            return;
        }
        let Some((node, range)) = self.target() else {
            return;
        };
        let at = range.start;
        let trigger_kind = match text {
            "/" => Some(TriggerKind::Slash),
            "@" => Some(TriggerKind::At),
            _ => None,
        };
        let armable = trigger_kind
            .filter(|k| self.view().policy.arms(*k))
            .filter(|_| !self.view_mut().overlay.is_open())
            // A title is a name, not a place blocks or reference pills are
            // made. `/` still opens there for its inline character commands;
            // `@` stays refused because a title is also drawn as plain text in
            // the tab strip and explore tree.
            .filter(|k| !self.title_focused() || *k == TriggerKind::Slash)
            .filter(|_| {
                self.body_mut()
                    .focused()
                    .is_some_and(|r| trigger::is_trigger_position(r, at))
            });

        self.insert_text(ws, text);
        if text.chars().any(char::is_whitespace) {
            ws.checkpoint();
            // The same boundary tells spell check a word just finished, so the
            // recheck skips its quiet period — an underline inside 100 ms
            // instead of after the debounce. A flag on the view rather than a
            // call, because the controller lives above this crate.
            if let Some(id) = self.body_mut().focused().map(|row| row.id) {
                self.view_mut().word_completed = Some(id);
            }
            // A whitespace terminator may have just finished a URL — link it in
            // place. (Punctuation is deliberately *not* a trigger: a URL is full
            // of dots and colons, so linking on `.` would fire mid-URL. Trailing
            // punctuation is stripped only once a space has ended the word.) The
            // caret is passed explicitly: the row's parley selection is not
            // refreshed until reflow, and this runs first.
            self.autolink_at(ws, at + text.len());
        }

        if let Some(kind) = armable {
            // Block commands are hidden when the caret is inside a ListBlock
            // paragraph — only inline commands and the attribute commands make
            // sense there — and on a title, where nothing structural belongs at
            // all.
            let title = self.title_focused();
            let allow_blocks = !title
                && !self
                    .body()
                    .focused()
                    .is_some_and(|r| r.list_style.is_some());
            // A name takes no references either, so the menu on a title shows
            // the emoji and the five attribute commands and nothing else.
            let allows = command_menu::Allows {
                image: self.view().images_enabled,
                image_offline: self.view().images_offline,
                divider: self.view().dividers_enabled,
                quote: self.view().quotes_enabled,
                headings: self.view().headings_enabled,
                code: self.view().code_enabled,
                table: self.view().tables_enabled,
                tasks: self.view().task_lists_enabled,
                blocks: allow_blocks,
                pills: !title,
                // **A title takes attributes.** It refuses pills because a
                // pill is content *inside* the name; an attribute is metadata
                // on the node, and the node under a title row is the document
                // root the explore sidebar's Attributes submenu already writes
                // to. Refusing here made one attribute settable from the
                // sidebar and refused two inches away in the document
                // ([ADR-187]).
                //
                // [ADR-187]: ../../../docs/decisions/ADR-201-225.md#adr-210-titleattrs--a-title-row-takes-attributes-but-not-pills-and-date-is-gone
                attrs: true,
                // **The row's own kind**, asked of the one predicate ADR-272
                // makes the authority. A title row answers `Kind::Document`,
                // which is capable — promoting a page *is* marking its title
                // row, because `Body::title_row` makes the document's root node
                // row 0 of its own body (ADR-210). A quote or a code block
                // answers `false` and the menu dims `/task` with the reason.
                task_capable: self
                    .body()
                    .focused()
                    .is_some_and(|r| koru_core::tasks::task_capable(r.kind)),
                // Read from the document, not the row: the marker is node
                // state, and the row's checkbox is only one way to have it.
                task: self
                    .body()
                    .focused()
                    .is_some_and(|r| koru_core::tasks::is_task_node(ws, r.id).unwrap_or(false)),
                // **One predicate**, read where the menu opens rather than per
                // frame -- `koru_voice::available`'s own note asks callers to
                // latch it, and a menu opening is exactly that moment.
                voice: koru_voice::available(),
            };
            let overlay =
                trigger::open_for_trigger(ws, &mut self.view_mut().engine, kind, node, at, allows);
            self.view_mut().overlay = overlay;
        } else if !self.view().overlay.is_open()
            && (self.view().policy.arms(TriggerKind::Colon)
                || self.view().policy.arms(TriggerKind::Hash))
            && text.chars().count() == 1
            && text
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            // `:` alone is quiet. The first valid query character opens the
            // compact completion panel, provided the colon begins a token.
            let caret = at + text.len();
            let content = ws
                .read(node)
                .ok()
                .and_then(|node| node.content.as_text().map(str::to_owned));
            if let Some(content) = content {
                let colon = at
                    .checked_sub(1)
                    .filter(|&i| content.as_bytes().get(i) == Some(&b':'));
                let boundary = colon.is_some_and(|start| {
                    content[..start]
                        .chars()
                        .next_back()
                        .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
                });
                if boundary && self.view().policy.arms(TriggerKind::Colon) {
                    let start = colon.unwrap();
                    let trigger = Trigger {
                        kind: TriggerKind::Colon,
                        start,
                        node,
                        anchor: Anchor::TriggerText,
                        stable: ws.capture_text_range(node, start, caret).ok(),
                    };
                    if let Some(picker) = EmojiPicker::shortcode(trigger, &content[at..caret]) {
                        self.view_mut().overlay = Overlay::Emoji(picker);
                    }
                }
                // `#tag` — the same late-arm, on the hash. The word just
                // typed into looks back for a `#` that begins it; the guards:
                // never at line start (that is the heading shorthand), only
                // on a word boundary (whitespace before the `#`), and never
                // while any overlay is open — which is what keeps a `#`
                // inside an open `@` query quiet. The chooser itself enforces
                // the shortcode contract: no table match and fewer than two
                // query characters never opens (`TagPicker::for_hash`). Armed
                // on the first *and* second character so a brand-new tag can
                // still be born from prose; a longer word never re-arms, so
                // Escape stays escaped.
                //
                // **A title is not excluded.** It was, on the argument that
                // every word in a name is part of it — but `/tag` already
                // tags a document from its own title row (ADR-187), so
                // refusing the shorthand there left the two doors to one
                // attribute disagreeing, which is the inconsistency ADR-187
                // exists to remove. The chooser's own contract is what keeps
                // a stray `#` in a name quiet: it never opens without a
                // matching tag or two typed characters.
                if !self.view_mut().overlay.is_open() && self.view().policy.arms(TriggerKind::Hash)
                {
                    let mut start = at;
                    while start > 0 {
                        let Some(ch) = content[..start].chars().next_back() else {
                            break;
                        };
                        if ch.is_alphanumeric() || matches!(ch, '_' | '-') {
                            start -= ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    let hash = start
                        .checked_sub(1)
                        .filter(|&i| content.as_bytes().get(i) == Some(&b'#'))
                        .filter(|&i| {
                            i > 0
                                && content[..i]
                                    .chars()
                                    .next_back()
                                    .is_some_and(char::is_whitespace)
                        });
                    let q = &content[start..caret];
                    if let Some(hash) = hash
                        && (1..=2).contains(&q.chars().count())
                    {
                        let trigger = Trigger {
                            kind: TriggerKind::Hash,
                            start: hash,
                            node,
                            anchor: Anchor::TriggerText,
                            stable: ws.capture_text_range(node, hash, caret).ok(),
                        };
                        let table: Vec<tag_picker::TagChoice> = self
                            .view()
                            .tags
                            .sorted()
                            .into_iter()
                            .map(|(id, face)| tag_picker::TagChoice {
                                id,
                                name: face.name,
                                color: face.color,
                            })
                            .collect();
                        let applied = applied_tags(ws, Some(node));
                        if let Some(picker) =
                            tag_picker::TagPicker::for_hash(trigger, vec![node], table, applied, q)
                        {
                            self.view_mut().overlay = Overlay::Tag(picker);
                        }
                    }
                }
            }
        }
    }

    /// One key press offered to whatever panel is open.
    ///
    /// The panels implement themselves in `components`; this only has to know
    /// what to do with the answer. `Become` is the command menu choosing a
    /// picker, and it is built here because building one needs the text engine.
    fn overlay_key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let view = self.view_mut();
        let trigger = view.overlay.trigger();
        let engine = &mut view.engine;
        match view.overlay.key(engine, key, text, mods) {
            Outcome::Become(cmd) => {
                self.choose_command(ws, trigger, cmd);
                Outcome::Consumed
            }
            Outcome::CommandArg(cmd, arg) => {
                self.choose_command_arg(ws, trigger, cmd, &arg);
                Outcome::Consumed
            }
            other => other,
        }
    }

    fn take_name_prompt(&mut self) -> Option<crate::components::name_prompt::NamePrompt> {
        match std::mem::replace(&mut self.view_mut().overlay, Overlay::None) {
            Overlay::Name(prompt) => Some(prompt),
            other => {
                self.view_mut().overlay = other;
                None
            }
        }
    }

    /// The open tag chooser, taken out — but only the form with its **own
    /// search box**, which is the form that stays open across several choices.
    ///
    /// The `#tag` grammar is deliberately excluded and still commits and
    /// closes. Its query is the text behind the panel, and the commit is what
    /// takes that text back out; leaving the panel up would hang a picker over
    /// prose somebody is typing into, re-filtering against a `#home` that is no
    /// longer there. `#mv` then choosing is a single-tag gesture, and it stays
    /// one ([ADR-188], amended).
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    fn take_open_tag_picker(&mut self) -> Option<tag_picker::TagPicker> {
        if !matches!(&self.view().overlay, Overlay::Tag(t) if t.has_search()) {
            return None;
        }
        match std::mem::replace(&mut self.view_mut().overlay, Overlay::None) {
            Overlay::Tag(picker) => Some(picker),
            other => {
                self.view_mut().overlay = other;
                None
            }
        }
    }

    /// Put a tag chooser back up after a choice committed, with the row it
    /// named ticked (`now_applied`) or unticked.
    ///
    /// The trigger has to be retired first. The typed `/tag` has just been
    /// deleted out of the row, so a *second* choice through the same trigger
    /// would resolve a span that no longer starts with `/` and be refused as
    /// an insertion point that changed elsewhere. The panel is still where it
    /// was — `Anchor::Insert` anchors at the same byte index, which is where
    /// the caret now sits — so the trigger is retired to one that replaces
    /// nothing.
    fn reopen_tag_picker(
        &mut self,
        mut picker: tag_picker::TagPicker,
        tag: koru_core::ids::TagId,
        now_applied: bool,
    ) {
        if picker.trigger.anchor == Anchor::TriggerText {
            picker.trigger.anchor = Anchor::Insert;
            picker.trigger.stable = None;
        }
        let view = self.view_mut();
        if now_applied {
            picker.reopen_after_apply(&mut view.engine, tag);
        } else {
            picker.reopen_after_remove(&mut view.engine, tag);
        }
        view.overlay = Overlay::Tag(picker);
    }

    fn resume_name_prompt(&mut self) -> bool {
        let prompt = match &mut self.view_mut().overlay {
            Overlay::Emoji(picker) => picker.resume_name.take(),
            _ => None,
        };
        if let Some(prompt) = prompt {
            self.view_mut().overlay = Overlay::Name(prompt);
            true
        } else {
            false
        }
    }

    fn open_page_icon_picker(
        &mut self,
        node: NodeId,
        resume_name: Option<crate::components::name_prompt::NamePrompt>,
    ) {
        let trigger = Trigger {
            kind: TriggerKind::Colon,
            start: 0,
            node,
            anchor: Anchor::PageIcon(node),
            stable: None,
        };
        let view = self.view_mut();
        let mut picker = EmojiPicker::standalone(&mut view.engine, trigger);
        picker.resume_name = resume_name;
        view.overlay = Overlay::Emoji(picker);
    }

    /// A slash command was chosen. Most of them swap the overlay for the picker
    /// that finishes the job; two do not. `Image` has no panel, so it asks the
    /// shell (via `wants_image`) to open a file dialog — disk is not a view's to
    /// touch. A heading has nothing left to choose, so it is applied here.
    fn choose_command(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Option<Trigger>,
        cmd: command_menu::Command,
    ) {
        // A menu opened from a row's `+` has no block behind it yet. Make it
        // now — the instant a kind is chosen and not before — and re-point the
        // trigger, so every arm below is the same code the `/` menu runs.
        let trigger = match trigger {
            Some(t) => match t.anchor {
                Anchor::NewBlock { above } => self.materialise_new_block(ws, t, above),
                _ => Some(t),
            },
            None => None,
        };
        if cmd == command_menu::Command::Image {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.view_mut().wants_image = Some(t.node);
            }
            return;
        }
        if cmd == command_menu::Command::Dictate {
            self.view_mut().overlay = Overlay::None;
            // The typed `/dictate` goes first, so the session begins over an
            // empty caret rather than over the command that asked for it.
            if let Some(t) = trigger {
                self.erase_trigger(ws, t);
            }
            // A microphone is no more a view's to open than a file dialog is.
            // The shell drains this and asks the controller, which is what
            // makes the row and the chord one action.
            self.view_mut().wants_dictation = true;
            return;
        }
        if cmd == command_menu::Command::Delegate {
            self.view_mut().overlay = Overlay::None;
            // The typed `/delegate` goes first, so the modal opens over the
            // row rather than over the command that asked for it — and so
            // cancelling leaves the document as it was, with nothing to undo.
            let from_trigger = trigger.as_ref().map(|t| t.node);
            if let Some(t) = trigger {
                self.erase_trigger(ws, t);
            }
            // Handing work to an agent is the container's, not the view's:
            // the shell drains this and opens the one delegation modal, the
            // same one the `+` menu and the two right-click rows reach.
            let node = from_trigger.or_else(|| self.body().focused().map(|row| row.id));
            // **What is left on the row is what was asked for.** The command
            // itself is already gone — `erase_trigger` above ran first — so
            // this reads the words a person wrote *around* it, exactly as a
            // selection's *Delegate…* reads the line it was raised on. A row
            // holding nothing but `/delegate` leaves an empty objective, which
            // is the same empty box that gesture always opened.
            let objective = node
                .map(|node| crate::objective::row_words(ws, node))
                .unwrap_or_default();
            self.view_mut().wants_delegate = Some(crate::view_state::DelegateRequest {
                node,
                from_selection: false,
                objective,
            });
            return;
        }
        if cmd == command_menu::Command::Attachment {
            self.view_mut().overlay = Overlay::None;
            // Prefer the trigger's node (slash text or the + menu's new row);
            // fall back to the focused row so a missing trigger still opens
            // the file dialog rather than silently doing nothing.
            let from_trigger = trigger.as_ref().map(|t| t.node);
            let node = from_trigger.or_else(|| self.body().focused().map(|row| row.id));
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!(
                    "[koru:attach] editor slash Attachment trigger={from_trigger:?} node={node:?}"
                );
            }
            if let Some(node) = node {
                self.view_mut().wants_attachment = Some(node);
            }
            return;
        }
        if cmd == command_menu::Command::Remind {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.arm_reminder(ws, t, "");
            }
            return;
        }
        if let command_menu::Command::Heading(heading) = cmd {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_heading(ws, t, heading);
            }
            return;
        }
        if cmd == command_menu::Command::Code {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_code(ws, t);
            }
            return;
        }
        if cmd == command_menu::Command::Diagram {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_diagram(ws, t);
            }
            return;
        }
        if cmd == command_menu::Command::Table {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_table(ws, t);
            }
            return;
        }
        if cmd == command_menu::Command::Divider {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_divider(ws, t);
            }
            return;
        }
        if cmd == command_menu::Command::Quote {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_quote(ws, t);
            }
            return;
        }
        // **A heading, not a picker.** There is nothing left to choose once
        // `/task` is named — the reason it is applied here rather than becoming
        // an overlay.
        if cmd == command_menu::Command::Task {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_task(ws, t);
            }
            return;
        }
        if cmd == command_menu::Command::Complete {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_complete(ws, t);
            }
            return;
        }
        if let Some(style) = match cmd {
            command_menu::Command::Tasks => Some(koru_core::ListStyle::Checkbox),
            command_menu::Command::List(style) => Some(style),
            _ => None,
        } {
            self.view_mut().overlay = Overlay::None;
            if let Some(t) = trigger {
                self.commit_list(ws, t, style);
            }
            return;
        }
        // The attribute pickers open seeded with document state — the node's
        // current tags, color, due and priority — so they are built here,
        // where `ws` and the view's tag table are in reach, rather than in
        // `trigger::for_command`.
        if matches!(
            cmd,
            command_menu::Command::Attr
                | command_menu::Command::Tag
                | command_menu::Command::Color
                | command_menu::Command::Due
                | command_menu::Command::Priority
                | command_menu::Command::TaskList
        ) {
            match trigger {
                Some(t) => self.open_attribute_picker(ws, t, cmd),
                None => self.view_mut().overlay = Overlay::None,
            }
            return;
        }
        if let Some(trigger) = trigger {
            self.view_mut().overlay =
                trigger::for_command(&mut self.view_mut().engine, trigger, cmd);
        }
    }

    /// A command chosen **with a typed argument** — `due friday`,
    /// `priority high` (D7's typed-argument sugar; the menu's
    /// [`Outcome::CommandArg`]). This is the one place the argument is given
    /// meaning, so both commands share one behavior: parse here, where the
    /// clock and the document are in reach — commit directly when it parses,
    /// and open the command's ordinary picker when it does not, so Enter
    /// never silently does nothing.
    fn choose_command_arg(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Option<Trigger>,
        cmd: command_menu::Command,
        arg: &str,
    ) {
        // A menu opened from a row's `+` has no block behind it yet — the
        // same materialisation `choose_command` does.
        let trigger = match trigger {
            Some(t) => match t.anchor {
                Anchor::NewBlock { above } => self.materialise_new_block(ws, t, above),
                _ => Some(t),
            },
            None => None,
        };
        let Some(t) = trigger else {
            self.view_mut().overlay = Overlay::None;
            return;
        };
        match cmd {
            command_menu::Command::Remind => self.arm_reminder(ws, t, arg),
            command_menu::Command::Due => {
                // The clock is resolved here and injected — the parser never
                // reads system time (its whole test story).
                let now = chrono::Local::now().naive_local();
                // **The second reference point comes off whatever calendar is
                // on screen.** A signed duration counts from the highlighted
                // day (ADR-191 §3), and the due picker is still the open
                // overlay when its phrase box hands an argument up — so the
                // phrase is read here against the same selection it was read
                // against there, rather than against a second answer. A `/due`
                // typed in the command menu has no calendar beside it and
                // reads both points off the clock.
                let at = match &self.view().overlay {
                    Overlay::Date(picker) => picker.reference(),
                    _ => crate::utils::when::Reference::without_selection(now),
                };
                match crate::utils::when::parse(arg, at, crate::utils::when::DEFAULT_DATE_ORDER) {
                    Some(parsed) => {
                        let nodes = vec![t.node];
                        // **This is where a wall clock becomes a deadline**,
                        // and it is the one place: `to_temporal` crosses the
                        // zone seam for every surface that turns typed words
                        // into a due, so the Tasks page's picker stores what
                        // this stores for the same phrase (ADR-206).
                        let value: AttrValue = crate::utils::when::to_temporal(parsed).into();
                        self.attribute_outcome(
                            ws,
                            Some(t),
                            &Outcome::SetAttr(
                                nodes,
                                koru_core::attributes::KEY_DUE.to_owned(),
                                value,
                            ),
                        );
                    }
                    None => {
                        let nodes = vec![t.node];
                        self.open_due_chooser(ws, t, nodes);
                    }
                }
            }
            command_menu::Command::Priority => match priority_token(arg) {
                Some(level) => {
                    let nodes = vec![t.node];
                    self.attribute_outcome(
                        ws,
                        Some(t),
                        &Outcome::SetAttr(
                            nodes,
                            koru_core::attributes::KEY_PRIORITY.to_owned(),
                            AttrValue::Text(level.as_str().to_owned()),
                        ),
                    );
                }
                None => {
                    let nodes = vec![t.node];
                    self.open_priority_chooser(ws, t, nodes);
                }
            },
            // No other command takes an argument; treat a stray one as the
            // plain choice.
            other => self.choose_command(ws, Some(t), other),
        }
    }

    /// Materialise the command menu's private query as ordinary document text
    /// and capture stable deletion authority for the exact submission.
    fn arm_reminder(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger, phrase: &str) {
        let Some(row) = self.body().row(trigger.node) else {
            self.view_mut().overlay = Overlay::None;
            return;
        };
        let start = trigger
            .stable
            .as_ref()
            .and_then(|range| ws.resolve_text_position(&range.anchor).ok())
            .unwrap_or(trigger.start);
        let end = row.caret().clamp(start, row.text.len());
        if !valid_overlay_span(ws, &trigger, start, end) {
            self.view_mut().overlay = Overlay::None;
            return;
        }
        let phrase = phrase.trim().to_owned();
        let command = if phrase.is_empty() {
            "/remind".to_owned()
        } else {
            format!("/remind {phrase}")
        };
        let Ok(caret) = ws.replace_text(trigger.node, start..end, &command) else {
            self.view_mut().overlay = Overlay::None;
            return;
        };
        let Ok(range) = ws.capture_text_range(trigger.node, start, caret) else {
            self.view_mut().overlay = Overlay::None;
            return;
        };
        let identity = crate::reminders::RequestIdentity {
            token: crate::reminders::RequestToken::new(),
            node: trigger.node,
        };
        let request = crate::reminders::Request {
            identity,
            phrase,
            due_anchor: crate::reminders::due_anchor(ws, trigger.node),
        };
        let view = self.view_mut();
        view.overlay = Overlay::None;
        view.pending_reminder_text.insert(
            identity.token,
            crate::reminders::PendingText {
                identity,
                text: command,
                range,
                origin: if trigger.anchor == Anchor::Insert {
                    crate::reminders::RequestOrigin::Synthetic
                } else {
                    crate::reminders::RequestOrigin::PersonAuthored
                },
            },
        );
        view.wants_reminder.push_back(request);
        self.body_mut().refresh(ws, trigger.node);
        self.body_mut().focus_on(trigger.node);
        self.place_caret(caret);
        self.reflow();
    }

    /// Take the typed `/header2` back out and make the block a heading.
    ///
    /// The front half of [`commit_pill`](Self::commit_pill) without the pill:
    /// what the menu stood in for was command text, and nothing replaces it.
    fn commit_heading(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger, heading: Heading) {
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let (start, end) = match trigger.stable.as_ref() {
            Some(range) => match ws.resolve_text_range(range) {
                Ok((anchor, focus)) => (anchor.min(focus), anchor.max(focus)),
                Err(_) => {
                    self.view_mut().overlay = Overlay::None;
                    self.view_mut().notice = Some("that insertion point changed elsewhere".into());
                    return;
                }
            },
            None => (trigger.start, caret.clamp(trigger.start, len)),
        };
        if !valid_overlay_span(ws, &trigger, start, end) {
            self.view_mut().overlay = Overlay::None;
            self.view_mut().notice = Some("that insertion point changed elsewhere".into());
            return;
        }

        self.body_mut().focus_on(trigger.node);
        let id = trigger.node;
        ws.checkpoint();
        ws.begin_group();
        if end > start {
            let _ = ws.delete_text(id, start..end);
        }
        // Named outright rather than toggled: choosing "header2" from a menu is
        // never a request to turn a heading off.
        let _ = ws.set_heading(id, Some(heading));
        ws.checkpoint();
        // `refresh` compares the heading as well as the text, so the row comes
        // back dirty and the ramp is picked up on the next reflow.
        self.body_mut().refresh(ws, id);
        self.place_caret(start);
    }

    // ------------------------------------------------------------ attributes

    /// Open the picker one of the three attribute commands names, seeded with
    /// the trigger node's current state. Intercepted in
    /// [`choose_command`](Self::choose_command) like a heading, because
    /// building these needs `ws` and the view's tag table, which
    /// `trigger::for_command` deliberately has neither of.
    fn open_attribute_picker(
        &mut self,
        ws: &WorkspaceDoc,
        trigger: Trigger,
        cmd: command_menu::Command,
    ) {
        match cmd {
            command_menu::Command::Attr => {
                let width = attr_menu::AttrMenu::measure_width(&mut self.view_mut().engine);
                let mut menu = attr_menu::AttrMenu::open(trigger);
                menu.width = width;
                self.view_mut().overlay = Overlay::Attr(menu);
            }
            command_menu::Command::Tag => {
                let nodes = vec![trigger.node];
                self.open_tag_chooser(ws, trigger, nodes);
            }
            command_menu::Command::Color => {
                let nodes = vec![trigger.node];
                self.open_color_chooser(ws, trigger, nodes);
            }
            command_menu::Command::Due => {
                let nodes = vec![trigger.node];
                self.open_due_chooser(ws, trigger, nodes);
            }
            command_menu::Command::Priority => {
                let nodes = vec![trigger.node];
                self.open_priority_chooser(ws, trigger, nodes);
            }
            command_menu::Command::TaskList => {
                let nodes = vec![trigger.node];
                self.open_list_chooser(ws, trigger, nodes);
            }
            _ => {}
        }
    }

    /// The list picker over `nodes`: the workspace's lists as the container
    /// last pushed them, the one the (first) node is in checked.
    fn open_list_chooser(&mut self, ws: &WorkspaceDoc, trigger: Trigger, nodes: Vec<NodeId>) {
        let table: Vec<list_picker::ListChoice> = self
            .view()
            .lists
            .listed()
            .iter()
            .map(|(id, name, color)| list_picker::ListChoice {
                id: *id,
                name: name.clone(),
                color: color.clone(),
            })
            .collect();
        let current = current_list(ws, nodes.first().copied());
        let view = self.view_mut();
        let picker =
            list_picker::ListPicker::opened(&mut view.engine, trigger, nodes, table, current);
        view.overlay = Overlay::List(picker);
    }

    /// A press on an attribute chip ([ADR-188]).
    ///
    /// The chip's **label** (or, on a swatch, its dot) opens that attribute's
    /// picker over that chip's node; the trailing `×` clears that attribute
    /// instead. Two regions of one rectangle, both answered by
    /// `chips::chip_at`, on all four kinds since [ADR-188] was amended.
    ///
    /// A tag removes **that one tag** — which is what the `×` was invented for,
    /// a block carrying several — while colour, due and priority each hold one
    /// value and clear the whole key. Either way it is one attribute commit and
    /// therefore one undo step, and the neighbouring chips are untouched.
    ///
    /// The node is the chip's row and nothing else — never
    /// [`attribute_targets`](Self::attribute_targets), which reads the block
    /// selection. A chip names one node's attribute; applying an edit to a
    /// selection somebody made earlier, because a chip happened to be inside
    /// it, is not what pressing that chip said. It is also why a chip on the
    /// **title row** needs no special case: there is no mixed set to refuse.
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    fn press_chip(
        &mut self,
        ws: &mut WorkspaceDoc,
        row: usize,
        hit: crate::chips::ChipHit,
    ) -> bool {
        let Some(node) = self.body().rows.get(row).map(|r| r.id) else {
            return false;
        };
        if hit.on_close {
            // **The due chip's `×` removes the task, not the date.**
            //
            // It is the one chip whose value is not the point: a tag, a colour
            // and a priority are values on a row and their `×` empties one, but
            // the due chip *is* the task marker (SPEC §4), so the thing you
            // take off at it is the task. Clearing only the date is still
            // offered — *Clear due date*, in the chip's menu, through
            // [`clear_chip`](Self::clear_chip) — and that row and this `×` are
            // now deliberately **two** gestures on the due chip where they are
            // still one everywhere else.
            //
            // It asks rather than acts: what goes is the marker, the canonical
            // lifecycle and the due date, none of which is visible from the
            // chip you pressed, so the host raises its confirmation first.
            if matches!(hit.spec, crate::chips::ChipSpec::Due(_)) {
                self.view_mut().wants_remove_from_tasks = Some(node);
                return true;
            }
            return self.clear_chip(ws, row, hit);
        }
        // **The due chip is the task's own control, so pressing it goes to the
        // task** — the same place `Ctrl+Shift+E` lands, and the return leg of
        // *View in origin*.
        //
        // The other three chips still open their picker, because a tag, a
        // colour and a priority are *values on a row* and their chip is the
        // control for that value. A due chip is not only that: it **is** the
        // task marker (SPEC §4 — a task with no date wears it dashed), so the
        // most useful thing behind it is the task, not the date. Editing the
        // date moved one door along, into the chip's menu, which is what
        // [`edit_chip`](Self::edit_chip) exists for.
        //
        // **With the Tasks view hidden** (`koru_features::tasks()` off) there
        // is no task to go to, so the due chip behaves like the other three
        // and opens its picker — the date is then the most useful thing
        // behind it.
        if matches!(hit.spec, crate::chips::ChipSpec::Due(_)) && koru_features::tasks() {
            self.view_mut().wants_task_view = Some(node);
            return true;
        }
        self.edit_chip(ws, row, hit)
    }

    /// **Empty this chip's one value**, off this one node.
    ///
    /// Through the ordinary attribute commit, so it is one undo step and
    /// refreshes exactly this row; `trigger: None`, so nothing is taken out of
    /// the text — a chip is presentation of node state and there is no typed
    /// command behind it. A tag removes **that one tag**, which is what the `×`
    /// was invented for on a block carrying several; colour, due and priority
    /// each hold one value and clear the whole key.
    ///
    /// Reached two ways: every chip's `×` except the due one, and the *Clear*
    /// row of every chip's menu — the due chip included, which is the whole
    /// reason this is a call of its own rather than the body of `press_chip`'s
    /// close branch.
    fn clear_chip(
        &mut self,
        ws: &mut WorkspaceDoc,
        row: usize,
        hit: crate::chips::ChipHit,
    ) -> bool {
        let Some(node) = self.body().rows.get(row).map(|r| r.id) else {
            return false;
        };
        let nodes = vec![node];
        let outcome = match &hit.spec {
            crate::chips::ChipSpec::Tag(id) => Outcome::RemoveTag(nodes, *id),
            crate::chips::ChipSpec::Due(_) => {
                Outcome::ClearAttr(nodes, koru_core::attributes::KEY_DUE.to_owned())
            }
            crate::chips::ChipSpec::Priority(_) => {
                Outcome::ClearAttr(nodes, koru_core::attributes::KEY_PRIORITY.to_owned())
            }
            crate::chips::ChipSpec::Color(_) => {
                Outcome::ClearAttr(nodes, koru_core::attributes::KEY_COLOR.to_owned())
            }
        };
        self.attribute_outcome(ws, None, &outcome);
        true
    }

    /// **Open the picker this chip's kind owns**, over the chip's own row.
    ///
    /// Split out of [`press_chip`](Self::press_chip) when a left-click on the
    /// due chip stopped meaning *edit* and started meaning *go to the task*.
    /// The chip menu's *Edit* row calls this directly, so the row that says it
    /// opens the date grid still opens the date grid — and the three chips
    /// whose click never changed reach it through `press_chip` exactly as
    /// before.
    fn edit_chip(&mut self, ws: &mut WorkspaceDoc, row: usize, hit: crate::chips::ChipHit) -> bool {
        let Some(node) = self.body().rows.get(row).map(|r| r.id) else {
            return false;
        };
        self.view_mut().chip_anchor = Some(hit.rect);
        let trigger = Trigger {
            kind: TriggerKind::Slash,
            start: 0,
            node,
            anchor: Anchor::Chip,
            stable: None,
        };
        let nodes = vec![node];
        match hit.spec {
            // **The tag picker, with that tag's row selected** — so the chip
            // that was pressed is the one the keyboard is already on. Choosing
            // it is a no-op (a tag is applied or it is not); taking it *off* is
            // the `×`, which is the only removal gesture.
            crate::chips::ChipSpec::Tag(id) => {
                self.open_tag_chooser_focused(ws, trigger, nodes, Some(id))
            }
            crate::chips::ChipSpec::Due(_) => self.open_due_chooser(ws, trigger, nodes),
            crate::chips::ChipSpec::Priority(_) => self.open_priority_chooser(ws, trigger, nodes),
            crate::chips::ChipSpec::Color(_) => self.open_color_chooser(ws, trigger, nodes),
        }
        self.view_mut().active = true;
        self.reflow();
        true
    }

    fn open_tag_chooser(&mut self, ws: &WorkspaceDoc, trigger: Trigger, nodes: Vec<NodeId>) {
        self.open_tag_chooser_focused(ws, trigger, nodes, None);
    }

    /// [`open_tag_chooser`](Self::open_tag_chooser), with `focus`'s row
    /// pre-selected when the table still holds it.
    fn open_tag_chooser_focused(
        &mut self,
        ws: &WorkspaceDoc,
        trigger: Trigger,
        nodes: Vec<NodeId>,
        focus: Option<koru_core::ids::TagId>,
    ) {
        let table: Vec<tag_picker::TagChoice> = self
            .view()
            .tags
            .sorted()
            .into_iter()
            .map(|(id, face)| tag_picker::TagChoice {
                id,
                name: face.name,
                color: face.color,
            })
            .collect();
        let applied = applied_tags(ws, nodes.first().copied());
        let view = self.view_mut();
        let mut picker =
            tag_picker::TagPicker::opened(&mut view.engine, trigger, nodes, table, applied);
        // `hits` is table order with an empty query, so the hit index *is* the
        // table index — but ask through `hits` anyway, or a later default
        // filter would silently select the wrong row.
        if let Some(id) = focus
            && let Some(row) = picker.hits.iter().position(|&i| picker.table[i].id == id)
        {
            picker.sel = row;
        }
        view.overlay = Overlay::Tag(picker);
    }

    fn open_color_chooser(&mut self, ws: &WorkspaceDoc, trigger: Trigger, nodes: Vec<NodeId>) {
        let current = current_color(ws, nodes.first().copied());
        let width = color_picker::ColorPicker::measure_width(&mut self.view_mut().engine);
        let mut picker = color_picker::ColorPicker::open_for(trigger, nodes, current);
        picker.width = width;
        self.view_mut().overlay = Overlay::Color(picker);
    }

    /// The date grid, armed for `koru.due` (A3): opens on the node's current
    /// due day when one is set — which is also what puts the Clear row on —
    /// and on `seed` (a parsed argument) or today otherwise.
    fn open_due_chooser_seeded(
        &mut self,
        ws: &WorkspaceDoc,
        trigger: Trigger,
        nodes: Vec<NodeId>,
        seed: Option<chrono::NaiveDate>,
    ) {
        let (current, has_due) = current_due(ws, nodes.first().copied());
        // The one clock read on this path, handed to the picker so its phrase
        // field resolves against the same instant the grid opened on.
        let now = chrono::Local::now().naive_local();
        let selected = seed.or(current).unwrap_or_else(|| now.date());
        let view = self.view_mut();
        let picker = date_picker::DatePicker::for_due(
            &mut view.engine,
            trigger,
            nodes,
            selected,
            has_due,
            now,
        );
        view.overlay = Overlay::Date(picker);
    }

    fn open_due_chooser(&mut self, ws: &WorkspaceDoc, trigger: Trigger, nodes: Vec<NodeId>) {
        self.open_due_chooser_seeded(ws, trigger, nodes, None);
    }

    fn open_priority_chooser(&mut self, ws: &WorkspaceDoc, trigger: Trigger, nodes: Vec<NodeId>) {
        let current = current_priority(ws, nodes.first().copied());
        let width = priority_picker::PriorityPicker::measure_width(&mut self.view_mut().engine);
        let mut picker = priority_picker::PriorityPicker::open_for(trigger, nodes, current);
        picker.width = width;
        self.view_mut().overlay = Overlay::Priority(picker);
    }

    /// The context menus' Tags… row: open the tag chooser over `nodes` — the
    /// block selection, or the row that was right-clicked. No trigger text
    /// exists, so nothing is deleted on commit ([`Anchor::Insert`]).
    fn open_tag_picker_over(&mut self, ws: &WorkspaceDoc, nodes: Vec<NodeId>) -> bool {
        let Some(trigger) = attribute_menu_trigger(&nodes) else {
            return false;
        };
        if self.view().overlay.is_open() {
            return false;
        }
        self.open_tag_chooser(ws, trigger, nodes);
        true
    }

    /// The context menus' Color… row, same shape as
    /// [`open_tag_picker_over`](Self::open_tag_picker_over).
    fn open_color_picker_over(&mut self, ws: &WorkspaceDoc, nodes: Vec<NodeId>) -> bool {
        let Some(trigger) = attribute_menu_trigger(&nodes) else {
            return false;
        };
        if self.view().overlay.is_open() {
            return false;
        }
        self.open_color_chooser(ws, trigger, nodes);
        true
    }

    /// The context menus' Due… row (A3), ditto.
    fn open_due_picker_over(&mut self, ws: &WorkspaceDoc, nodes: Vec<NodeId>) -> bool {
        let Some(trigger) = attribute_menu_trigger(&nodes) else {
            return false;
        };
        if self.view().overlay.is_open() {
            return false;
        }
        self.open_due_chooser(ws, trigger, nodes);
        true
    }

    /// The context menus' Priority… row (A4), ditto.
    fn open_priority_picker_over(&mut self, ws: &WorkspaceDoc, nodes: Vec<NodeId>) -> bool {
        let Some(trigger) = attribute_menu_trigger(&nodes) else {
            return false;
        };
        if self.view().overlay.is_open() {
            return false;
        }
        self.open_priority_chooser(ws, trigger, nodes);
        true
    }

    /// One entry for the four menu-opened attribute pickers, so a host names
    /// which with [`AttrPicker`] rather than a spreading set of booleans.
    fn open_attr_picker_over(
        &mut self,
        ws: &WorkspaceDoc,
        nodes: Vec<NodeId>,
        which: AttrPicker,
    ) -> bool {
        match which {
            AttrPicker::Tags => self.open_tag_picker_over(ws, nodes),
            AttrPicker::Color => self.open_color_picker_over(ws, nodes),
            AttrPicker::Due => self.open_due_picker_over(ws, nodes),
            AttrPicker::Priority => self.open_priority_picker_over(ws, nodes),
        }
    }

    /// The rows an attribute gesture from the context menu applies to: every
    /// block-selected row when `anchor` is one of them, else `anchor` alone —
    /// and **`None` when that set mixes the document's own title row with rows
    /// of its body**, which is the one shape with no honest answer.
    ///
    /// It used to filter the title out (`selected && !row.is_title`) and carry
    /// on, so selecting the name and two paragraphs and choosing *Tags…* tagged
    /// the two paragraphs and silently skipped the document: three things
    /// selected, two of them changed. Extending the apply to the title instead
    /// is the other bad answer — a tag on a document is a fact about the
    /// document, and adding it as a side effect of a drag that happened to
    /// start one row too high is not what was asked either. So the gesture is
    /// offered only for a selection that is unambiguous — the title alone, or
    /// body rows alone — and dimmed with a reason otherwise ([ADR-187]).
    ///
    /// Every attribute follows the same rule, not tags alone: all four pickers
    /// come through here, and a menu where *Tags…* dims and *Due…* does not
    /// would be teaching that one of them is safe on a mixed selection.
    ///
    /// [ADR-187]: ../../../docs/decisions/ADR-201-225.md#adr-210-titleattrs--a-title-row-takes-attributes-but-not-pills-and-date-is-gone
    fn attribute_targets(&self, anchor: NodeId) -> Option<Vec<NodeId>> {
        let selected: Vec<&Paragraph> =
            self.body().rows.iter().filter(|row| row.selected).collect();
        let rows: Vec<(NodeId, bool)> =
            if selected.len() > 1 && selected.iter().any(|row| row.id == anchor) {
                selected.iter().map(|row| (row.id, row.is_title)).collect()
            } else {
                let title = self
                    .body()
                    .rows
                    .iter()
                    .find(|row| row.id == anchor)
                    .is_some_and(|row| row.is_title);
                vec![(anchor, title)]
            };
        let mixed = rows.iter().any(|(_, title)| *title) && rows.iter().any(|(_, title)| !*title);
        (!mixed).then(|| rows.into_iter().map(|(id, _)| id).collect())
    }

    /// Whether the current selection is the mixed title-and-body shape
    /// [`attribute_targets`](Self::attribute_targets) refuses — what a menu
    /// asks in order to dim its attribute rows and say why.
    fn attribute_selection_is_mixed(&self) -> bool {
        self.body()
            .focused_id()
            .is_some_and(|anchor| self.attribute_targets(anchor).is_none())
    }

    /// The commit every attribute outcome shares: take typed trigger text
    /// back out (the [`commit_heading`](Self::commit_heading) front half) and
    /// run `op` — **one undo step for both**, via `edit_group` — then refresh
    /// exactly the touched rows and reflow. A picker opened from a menu has
    /// no trigger text ([`Anchor::Insert`]) and deletes nothing.
    fn commit_attribute(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Option<Trigger>,
        nodes: &[NodeId],
        op: &mut dyn FnMut(&mut WorkspaceDoc) -> koru_core::schema::Result<()>,
    ) {
        self.view_mut().overlay = Overlay::None;
        let span = match trigger.as_ref().filter(|t| t.anchor == Anchor::TriggerText) {
            Some(t) => {
                let Some(row) = self.body_mut().row(t.node) else {
                    return;
                };
                let (len, caret) = (row.text.len(), row.caret());
                let (start, end) = match t.stable.as_ref() {
                    Some(range) => match ws.resolve_text_range(range) {
                        Ok((anchor, focus)) => (anchor.min(focus), anchor.max(focus)),
                        Err(_) => {
                            self.view_mut().notice =
                                Some("that insertion point changed elsewhere".into());
                            return;
                        }
                    },
                    None => (t.start, caret.clamp(t.start, len)),
                };
                if !valid_overlay_span(ws, t, start, end) {
                    self.view_mut().notice = Some("that insertion point changed elsewhere".into());
                    return;
                }
                self.body_mut().focus_on(t.node);
                Some((t.node, start, end))
            }
            None => None,
        };
        // **One undo step for the whole gesture**, canonical commands included.
        // `tasks::compose` is what lets a due over a three-row selection — three
        // `set_due`s and the trigger deletion beside them — stay one Ctrl+Z: a
        // command is its own step *unless* the caller has said it owns the
        // boundary, which is exactly what this is.
        //
        // **The command runs first, and the typed trigger comes out only if it
        // worked.** The other order shipped, and `edit_group` is not a
        // transaction — it brackets undo grouping and rolls nothing back — so a
        // refused command left the deletion committed: `/due 2026-09-04`
        // disappeared and no chip replaced it. That is the failure mode the
        // whole "losing content" section of the orientation is about, and the
        // ordering is the fix rather than a rollback, because there is nothing
        // here that could roll one back. The two writes are independent — the
        // deletion is text on the trigger row, the command is attributes on
        // `nodes` — so nothing is lost by swapping them.
        let applied = koru_core::tasks::compose(ws, |ws| {
            ws.edit_group(|ws| {
                op(ws)?;
                if let Some((node, start, end)) = span
                    && end > start
                {
                    let _ = ws.delete_text(node, start..end);
                }
                Ok(())
            })
        });
        // Content-free and coalesced, the way the host's task counters are: a
        // view crate emits nothing and opens nothing, so what it can honestly
        // do is keep a number whoever owns `KORU_TASKS_DEBUG` can print.
        let refused = applied.is_err();
        if refused {
            crate::tasks::note_refused_commit();
        }
        if let Some((node, start, end)) = span {
            self.body_mut().refresh(ws, node);
            // The typing is still there when the command refused, so the caret
            // belongs after it rather than back at the `/`.
            self.place_caret(if refused { end } else { start });
        }
        for &node in nodes {
            self.body_mut().refresh(ws, node);
        }
        self.reflow();
    }

    /// One attribute outcome, applied. Answers whether it was one.
    fn attribute_outcome(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Option<Trigger>,
        outcome: &Outcome,
    ) -> bool {
        match outcome {
            Outcome::SetAttr(nodes, key, value) => {
                // **Routed per node**, so a selection holding a task and a
                // plain paragraph does the right thing for each rather than one
                // thing for both.
                let mut owed = crate::tasks::ActivitySink::new();
                self.commit_attribute(ws, trigger, nodes, &mut |ws| {
                    koru_core::tasks::routed::set_attribute(
                        ws,
                        nodes,
                        key,
                        value.clone(),
                        &mut owed,
                    )
                });
                for one in owed {
                    crate::tasks::remember(&mut self.body_mut().task_activity, one);
                }
            }
            Outcome::ClearAttr(nodes, key) => {
                let mut owed = crate::tasks::ActivitySink::new();
                self.commit_attribute(ws, trigger, nodes, &mut |ws| {
                    koru_core::tasks::routed::clear_attribute(ws, nodes, key, &mut owed)
                });
                for one in owed {
                    crate::tasks::remember(&mut self.body_mut().task_activity, one);
                }
            }
            Outcome::AddTag(nodes, tag) => {
                // **The tag chooser stays open across several choices**
                // ([ADR-188], amended). Taken out *before* the commit, because
                // `commit_attribute` closes whatever is open, and put back
                // after it with the row ticked.
                let keep = self.take_open_tag_picker();
                self.commit_attribute(ws, trigger, nodes, &mut |ws| {
                    ws.add_tag_to_all(nodes, *tag).map(|_| ())
                });
                if let Some(picker) = keep {
                    self.reopen_tag_picker(picker, *tag, true);
                }
            }
            Outcome::RemoveTag(nodes, tag) => {
                // A ticked row chosen again ([ADR-188], amended twice) — which
                // keeps the panel up exactly as an apply does, because the two
                // are the same visit. The chip's `×` reaches here too and has
                // no picker open, so `keep` is `None` and nothing reopens.
                let keep = self.take_open_tag_picker();
                self.commit_attribute(ws, trigger, nodes, &mut |ws| {
                    ws.remove_tag_from_all(nodes, *tag).map(|_| ())
                });
                if let Some(picker) = keep {
                    self.reopen_tag_picker(picker, *tag, false);
                }
            }
            Outcome::CreateTag(nodes, name) => {
                // The table lives in the workspace system document, which
                // this layer cannot see — take the trigger text out now and
                // hand the creation up; the host creates the tag and applies
                // the fresh id through the document.
                self.commit_attribute(ws, trigger, nodes, &mut |_| Ok(()));
                self.view_mut()
                    .wants_create_tag
                    .push(nodes.clone(), name.clone());
            }
            Outcome::JoinList(nodes, list) => {
                // **A list holds tasks**, so a row that is not one becomes one
                // first — `/task`'s own command, reason Explicit — and then
                // takes the membership, in the one undo step that also takes
                // the trigger out. The in-group attribute door, not `set_list`:
                // that one checkpoints, and would close this bracket.
                let (targets, list) = (nodes.clone(), *list);
                let mut owed = crate::tasks::ActivitySink::new();
                self.commit_attribute(ws, trigger, nodes, &mut |ws| {
                    for &node in &targets {
                        crate::tasks::run(&mut owed, ws, |ws| {
                            koru_core::tasks::ensure_task(
                                ws,
                                node,
                                koru_core::attributes::TaskReason::Explicit,
                            )
                            .map(Some)
                        })?;
                    }
                    ws.set_attribute_on_all(
                        &targets,
                        koru_core::attributes::KEY_LIST,
                        AttrValue::Text(list.to_storage()),
                    )
                    .map(|_| ())
                });
                for one in owed {
                    crate::tasks::remember(&mut self.body_mut().task_activity, one);
                }
            }
            Outcome::CreateList(nodes, name, color) => {
                // As a new tag: the table is the host's, so the trigger comes
                // out here and the creation — and the join that follows it —
                // goes up.
                self.commit_attribute(ws, trigger, nodes, &mut |_| Ok(()));
                self.view_mut().wants_join_new_list =
                    Some((nodes.clone(), name.clone(), color.clone()));
            }
            _ => return false,
        }
        true
    }

    /// Take the typed `/task` back out and **track this row as a task**
    /// ([SPEC §1]: a task-capable entity becomes a task *in place*, its kind,
    /// position and content unchanged).
    ///
    /// It goes through [`commit_attribute`](Self::commit_attribute) rather than
    /// writing anything itself, and that is the load-bearing part: task-ness is
    /// node state, so `/task` owes exactly what `/due` owes — one undo step
    /// covering both the marker and the deletion of the typed trigger, the
    /// command run *first* so a refusal leaves the typing where it was, and the
    /// touched row refreshed so the due chip appears without a rebuild. Every
    /// one of those was learned once already, on the `/due` path.
    ///
    /// **One node, never [`attribute_targets`](Self::attribute_targets).** The
    /// slash commands all commit over the trigger's row alone — see
    /// `open_attribute_picker`, where the same `vec![trigger.node]` is what
    /// seeds each of the four pickers.
    ///
    /// The kind is *not* checked here. `ensure_task` calls `require_capable`
    /// and answers `TaskRejected { why: KindNotCapable }` for a quote, a code
    /// block or a table, which is the enforcement ADR-272 asks for; the menu's
    /// dim row is the explanation beside it, not the guard.
    ///
    /// [SPEC §1]: ../../../docs/features/tasks/SPEC.md
    fn commit_task(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        let node = trigger.node;
        let nodes = [node];
        let mut owed = crate::tasks::ActivitySink::new();
        self.commit_attribute(ws, Some(trigger), &nodes, &mut |ws| {
            crate::tasks::run(&mut owed, ws, |ws| {
                koru_core::tasks::ensure_task(ws, node, koru_core::attributes::TaskReason::Explicit)
                    .map(Some)
            })
            .map(|_| ())
        });
        for one in owed {
            crate::tasks::remember(&mut self.body_mut().task_activity, one);
        }
    }

    /// Take the typed `/completed` back out and **finish the task this row
    /// is** — or reopen it when it is done — through the canonical lifecycle
    /// command, in the one undo step that takes the trigger out: `/task`'s
    /// shape, for the other end of a task's life. On a page whose title is
    /// the task, this is how the page is finished.
    fn commit_complete(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        let node = trigger.node;
        let nodes = [node];
        let done = ws
            .read(node)
            .map(|read| {
                koru_core::tasks::TaskFields::of(&read).lifecycle
                    == koru_core::attributes::Lifecycle::Completed
            })
            .unwrap_or(false);
        let mut owed = crate::tasks::ActivitySink::new();
        self.commit_attribute(ws, Some(trigger), &nodes, &mut |ws| {
            crate::tasks::run(&mut owed, ws, |ws| {
                if done {
                    koru_core::tasks::reopen_task(
                        ws,
                        node,
                        koru_core::tasks::LifecycleReason::ExplicitReopen,
                    )
                } else {
                    koru_core::tasks::complete_task(
                        ws,
                        node,
                        koru_core::tasks::LifecycleReason::ExplicitComplete,
                        crate::tasks::today(),
                    )
                }
                .map(Some)
            })
            .map(|_| ())
        });
        for one in owed {
            crate::tasks::remember(&mut self.body_mut().task_activity, one);
        }
    }

    /// Take the typed `/divider` back out and drop a horizontal rule in.
    ///
    /// An empty line is replaced by the divider; a line with text ahead of the
    /// command keeps that text as a paragraph and gets the divider after it. In
    /// both cases a fresh empty paragraph opens beneath the rule for the caret.
    fn commit_divider(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        let Some(row) = self.body().row(trigger.node) else {
            return;
        };
        // The command text runs from the `/` to the end of the typed line, so
        // delete the whole tail rather than trusting the caret — a stale caret
        // would leave `/divider` behind and skip the replace path below.
        let len = row.text.len();
        let id = trigger.node;
        self.body_mut().focus_on(id);
        ws.checkpoint();
        ws.begin_group();
        if len > trigger.start {
            let _ = ws.delete_text(id, trigger.start..len);
        }
        if ws.text_len(id).unwrap_or(0) == 0 {
            let _ = self.replace_with_divider(ws, id);
        } else {
            let Some(divider) = Node::divider().ok() else {
                ws.checkpoint();
                return;
            };
            let Some(para) = Node::paragraph("").ok() else {
                ws.checkpoint();
                return;
            };
            let div = ws.insert_after(id, divider).unwrap_or(id);
            let next = ws.insert_after(div, para).unwrap_or(div);
            self.body_mut().rebuild(ws);
            self.body_mut().focus_on(next);
            self.place_caret(0);
        }
        ws.checkpoint();
    }

    /// Replace the empty paragraph `id` with a divider in the same slot, then
    /// open a fresh empty paragraph after it for the caret. No undo grouping —
    /// both callers ([`divider_shorthand`](Self::divider_shorthand) and
    /// [`commit_divider`](Self::commit_divider)) bracket this with one group so a
    /// single Ctrl+Z removes the divider and restores what was there.
    fn replace_with_divider(&mut self, ws: &mut WorkspaceDoc, id: NodeId) -> Option<NodeId> {
        let parent = self.body().parent_of(ws, id);
        let at = ws.sibling_index(id).ok()?;
        let divider = Node::divider().ok()?;
        let para = Node::paragraph("").ok()?;
        let _ = ws.delete(id);
        let div = ws.insert(parent, divider).ok()?;
        let _ = ws.move_node(div, parent, at);
        let next = ws.insert_after(div, para).ok()?;
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(next);
        self.place_caret(0);
        Some(next)
    }

    /// Take the typed `/code` back out and make the block a code block. The
    /// twin of [`commit_heading`](Self::commit_heading) for a relabel rather
    /// than a property: `set_kind` swaps the kind in place, keeping the node's
    /// text and identity. No language is pinned, so the block highlights once
    /// the user picks one with the chip — or stays plain.
    fn commit_code(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let end = caret.clamp(trigger.start, len);

        self.body_mut().focus_on(trigger.node);
        let id = trigger.node;
        ws.checkpoint();
        ws.begin_group();
        if end > trigger.start {
            let _ = ws.delete_text(id, trigger.start..end);
        }
        let _ = ws.set_kind(id, Kind::Code);
        ws.checkpoint();
        // A rebuild rather than a refresh: the kind changed, and `adopt` keys
        // the mono font and the dirty flag off it.
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(trigger.start);
    }

    /// Take `/diagram` back out and start a diagram: a `mermaid` code block
    /// holding [`diagram::example`](crate::diagram::example), its text open
    /// with the whole example selected and the drawing below it — so typing
    /// or pasting replaces the example at once, and an arrow keeps it.
    ///
    /// **A line that has words keeps them.** Unlike `/code`, which makes the
    /// line itself code, the diagram goes in a new block below: a sentence is
    /// never Mermaid, and turning it into a block that cannot draw would only
    /// move the person's words somewhere they have to fetch them back from.
    /// An empty line — and the margin `+`'s fresh paragraph — becomes the
    /// diagram in place. One undo group either way.
    fn commit_diagram(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        if !self.view().code_enabled {
            return;
        }
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let end = caret.clamp(trigger.start, len);
        let id = trigger.node;
        let example = crate::diagram::example(koru_i18n::current());
        self.body_mut().focus_on(id);
        ws.checkpoint();
        ws.begin_group();
        if end > trigger.start {
            let _ = ws.delete_text(id, trigger.start..end);
        }
        let block = if ws.text_len(id).unwrap_or(0) == 0 {
            let _ = ws.set_heading(id, None);
            let _ = ws.set_kind(id, Kind::Code);
            let _ = ws.set_text(id, &example);
            id
        } else {
            let Some(below) = Node::code(example.as_str())
                .ok()
                .and_then(|node| ws.insert_after(id, node).ok())
            else {
                ws.checkpoint();
                return;
            };
            below
        };
        let _ = ws.set_language(block, Some(crate::diagram::LANGUAGE));
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        if self.open_diagram_text(block) {
            self.place_selection_pair((0, example.len()));
            self.reflow();
        }
    }

    /// Take the typed `/command` back out and leave the caret where it was.
    ///
    /// [`commit_code`](Self::commit_code) and its neighbours all begin with
    /// these five lines and then do something to the block; `/dictate` is the
    /// first command whose whole effect on the *document* is the deletion, so
    /// it is a function rather than a sixth copy. One group, so the removal and
    /// whatever the dictation later commits are not two undo steps for what
    /// reads as one action.
    fn erase_trigger(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let end = caret.clamp(trigger.start, len);
        let id = trigger.node;
        self.body_mut().focus_on(id);
        if end > trigger.start {
            ws.checkpoint();
            ws.begin_group();
            let _ = ws.delete_text(id, trigger.start..end);
            ws.checkpoint();
            self.body_mut().refresh(ws, id);
        }
        self.place_caret(trigger.start);
        self.reflow();
    }

    /// Turn the command row into one canonical Markdown table source block.
    /// The grid is derived only after this source validates; until then every
    /// typed character remains visible and recoverable in source mode.
    fn commit_table(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        if !self.view().tables_enabled {
            return;
        }
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let end = caret.clamp(trigger.start, len);
        let id = trigger.node;
        self.body_mut().focus_on(id);
        ws.checkpoint();
        ws.begin_group();
        if end > trigger.start {
            let _ = ws.delete_text(id, trigger.start..end);
        }
        let _ = ws.set_heading(id, None);
        let _ = ws.set_kind(id, Kind::Table);
        ws.checkpoint();
        self.view_mut().table_source = Some(id);
        self.view_mut().notice =
            Some("Type a Markdown header and delimiter row; Escape shows the table".into());
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(trigger.start);
    }

    /// Take `/quote` back out and make the row quoted prose without changing
    /// its identity. This mirrors `commit_code`: both are semantic block kinds,
    /// not marks painted over a paragraph.
    fn commit_quote(&mut self, ws: &mut WorkspaceDoc, trigger: Trigger) {
        if !self.view().quotes_enabled {
            return;
        }
        let Some(row) = self.body_mut().row(trigger.node) else {
            return;
        };
        let (len, caret) = (row.text.len(), row.caret());
        let end = caret.clamp(trigger.start, len);
        self.body_mut().focus_on(trigger.node);
        let id = trigger.node;
        ws.checkpoint();
        ws.begin_group();
        if end > trigger.start {
            let _ = ws.delete_text(id, trigger.start..end);
        }
        let _ = ws.set_heading(id, None);
        let _ = ws.set_kind(id, Kind::Quote);
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(trigger.start);
    }

    /// Take the typed `/tasks` back out and wrap the row into a ListBlock.
    fn commit_list(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Trigger,
        style: koru_core::ListStyle,
    ) {
        if !self.view().task_lists_enabled {
            return;
        }
        let Some(row) = self.body().row(trigger.node) else {
            return;
        };
        let (id, len) = (row.id, row.text.len());
        ws.checkpoint();
        ws.begin_group();
        // The trigger text goes first, the way every other commit does — with
        // the query in the panel that is just the `/`, and nothing at all for a
        // menu opened from the `+`.
        if len > trigger.start {
            let _ = ws.delete_text(id, trigger.start..len);
        }
        // Already in a list: re-style the block it is in rather than nesting a
        // second one inside the first.
        let existing = ws
            .ancestors(id)
            .unwrap_or_default()
            .into_iter()
            .find(|&a| ws.read(a).is_ok_and(|n| n.kind == Kind::ListBlock));
        match existing {
            Some(block) => {
                let _ = ws.set_list_style(block, Some(style));
            }
            None => {
                let _ = ws.wrap_in_list_block(id, style);
            }
        }
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(0);
        self.reflow();
    }

    /// Toggle the focused row between a paragraph and a code block (Ctrl+`).
    /// A code block carrying newlines cannot go back to a paragraph — the
    /// one-line rule would refuse it — so in that case `set_kind` declines and
    /// nothing happens. The caret stays where it was.
    fn toggle_code_block(&mut self, ws: &mut WorkspaceDoc) {
        if self.title_focused() {
            return;
        }
        let Some(row) = self.body().focused() else {
            return;
        };
        let (id, caret, kind) = (row.id, row.caret(), row.kind);
        // Only text-shaped prose rows toggle. An image or a contact is not prose
        // to turn into code.
        if !matches!(kind, Kind::Paragraph | Kind::Code) {
            return;
        }
        let target = if kind == Kind::Code {
            Kind::Paragraph
        } else {
            Kind::Code
        };
        ws.checkpoint();
        if ws.set_kind(id, target).is_err() {
            // A multi-line code block cannot become a paragraph — leave it.
            return;
        }
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(id);
        self.place_caret(caret);
        self.reflow();
    }

    /// Queue a caret on the focused line.
    fn place_caret(&mut self, at: usize) {
        if let Some(row) = self.body_mut().focused_mut() {
            row.place_caret(at);
        }
    }

    /// Select a byte range on the focused line.
    ///
    /// Nothing in either view calls it — they select from inside the line that
    /// owns the range — but an embedder building a "select this" command needs
    /// it, and so does every test written against this crate rather than in it.
    fn place_selection_pair(&mut self, pair: (usize, usize)) {
        if let Some(row) = self.body_mut().focused_mut() {
            row.place_selection(pair.0, pair.1);
        }
    }

    /// Open the block menu from row `i`'s `+`, for a block above or below it.
    ///
    /// **Nothing is inserted here.** The menu carries the row and the direction
    /// and makes the node only once a kind is chosen — so `Esc`, or a click
    /// anywhere else, leaves the document untouched.
    fn open_insert_menu(&mut self, i: usize, above: bool) {
        let Some(row) = self.body().rows.get(i) else {
            return;
        };
        let node = row.id;
        let view = self.view_mut();
        view.active = true;
        let allows = command_menu::Allows {
            image: view.images_enabled,
            image_offline: view.images_offline,
            divider: view.dividers_enabled,
            quote: view.quotes_enabled,
            headings: view.headings_enabled,
            code: view.code_enabled,
            table: view.tables_enabled,
            tasks: view.task_lists_enabled,
            blocks: true,
            // The `+` always makes a *new* block, so it is never on a title
            // and every command is on the table.
            pills: true,
            attrs: true,
            // `materialise_new_block` makes a **paragraph**, and a paragraph is
            // task-capable — so the answer is fixed rather than read off the
            // row the `+` hangs beside, which is a different node and may well
            // be a quote.
            task_capable: true,
            task: false,
            voice: koru_voice::available(),
        };
        let menu = command_menu::CommandMenu::opened(
            &mut self.view_mut().engine,
            Trigger {
                kind: TriggerKind::Slash,
                start: 0,
                node,
                anchor: Anchor::NewBlock { above },
                stable: None,
            },
            allows,
        );
        self.view_mut().overlay = Overlay::Command(menu);
    }

    /// Make the block a `+` menu promised, and re-point `trigger` at it.
    ///
    /// Returns a trigger over an empty new paragraph with `start == 0`, so the
    /// `commit_*` paths — which all begin by deleting the typed `/command` —
    /// find an empty range and delete nothing.
    fn materialise_new_block(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Trigger,
        above: bool,
    ) -> Option<Trigger> {
        let Ok(node) = Node::paragraph("") else {
            return None;
        };
        ws.checkpoint();
        ws.begin_group();
        let fresh = if above {
            let parent = self.body().parent_of(ws, trigger.node);
            let at = ws.sibling_index(trigger.node).ok()?;
            let id = ws.insert(parent, node).ok()?;
            let _ = ws.move_node(id, parent, at);
            id
        } else {
            ws.insert_after(trigger.node, node).ok()?
        };
        ws.checkpoint();
        self.body_mut().rebuild(ws);
        self.body_mut().focus_on(fresh);
        self.place_caret(0);
        Some(Trigger {
            kind: trigger.kind,
            start: 0,
            node: fresh,
            anchor: Anchor::Insert,
            stable: None,
        })
    }

    fn overlay_panel_rect(&self) -> Option<Rect> {
        // The language picker is not trigger-based — it opens from the hover
        // chip at the top-right of a code block, so it anchors there as a
        // dropdown rather than to a caret.
        if let Overlay::Language(p) = &self.view().overlay {
            let row = self.body().row(p.node)?;
            let vw = self.view().viewport.0;
            // The chip is measured against the column's width, not the window's,
            // and lives in content-column x — the same geometry `hover_at` and
            // `mouse_down` hit-test with, or the dropdown anchors away from the
            // chip that opened it in Page/Narrow.
            let layout_w = self.view().view_mode.effective_w(vw);
            let chip = rowlayout::lang_chip_rect(row, layout_w);
            let (w, h) = self.view().overlay.panel_size();
            let x = (self.view().content_x + chip.x).clamp(8.0, (vw - w - 8.0).max(8.0));
            let top = chip.y as f32 - self.view().scroll + chip.h as f32 + 2.0;
            let floor = self.view().chrome_h + 8.0;
            let ceiling = (self.view().viewport.1 - h - 8.0).max(floor);
            let y = top.clamp(floor, ceiling);
            return Some(Rect::new(x, y, w, h));
        }

        let trigger = self.view().overlay.trigger()?;
        // The overlay is drawn at `root` (viewport space), so every rect this
        // returns is in viewport space. Rows and carets live in the centred
        // content column, so their x is shifted by `content_x` here.
        let cx = self.view().content_x;
        let vw = self.view().viewport.0;
        let layout_w = self.view().view_mode.effective_w(vw);

        // A rename hangs off the title row. The document node *is* the body's
        // root, so `body().row(node)` would find nothing for it in a view with
        // no title row — hence the fall-back to the top-left of the content
        // column rather than silently drawing no panel.
        //
        // It is still reachable (F2) now that the title is editable in place,
        // because the prompt's icon button is the only way to set a page's
        // emoji. Moving that into the page menu would retire this whole arm.
        if matches!(trigger.anchor, Anchor::Rename(_) | Anchor::PageIcon(_)) {
            let (w, h) = self.view().overlay.panel_size();
            let title = self.body().rows.first().filter(|r| r.is_title);
            let x = title
                .map_or(theme::MARGIN_X + cx, |r| r.text_x() + cx)
                .clamp(8.0, (vw - w - 8.0).max(8.0));
            let y = title
                .map_or(self.view().chrome_h + 4.0, |r| {
                    r.y + r.height - self.view().scroll + 4.0
                })
                .clamp(
                    self.view().chrome_h + 4.0,
                    (self.view().viewport.1 - h - 8.0).max(0.0),
                );
            return Some(Rect::new(x, y, w, h));
        }

        // A picker opened from an attribute chip hangs off that chip, the way
        // the language dropdown hangs off the code block's chip: below it when
        // there is room, above it when there is not.
        if trigger.anchor == Anchor::Chip
            && let Some(chip) = self.view().chip_anchor
        {
            let (w, h) = self.view().overlay.panel_size();
            let x = (cx + chip.x).clamp(8.0, (vw - w - 8.0).max(8.0));
            let top = chip.y - self.view().scroll + chip.h + 4.0;
            let floor = self.view().chrome_h + 8.0;
            let ceiling = (self.view().viewport.1 - h - 8.0).max(floor);
            let y = if top + h <= ceiling {
                top
            } else {
                (chip.y - self.view().scroll - h - 4.0).clamp(floor, ceiling)
            };
            return Some(Rect::new(x, y, w, h));
        }

        // A `+` menu hangs off the button that opened it, in the affordance
        // lane, rather than off a caret — there is no caret in a block that
        // does not exist yet.
        if let Anchor::NewBlock { .. } = trigger.anchor {
            let row = self.body().row(trigger.node)?;
            let (plus, _) = rowlayout::gutter_affordances(row, row.y);
            let (w, h) = self.view().overlay.panel_size();
            let x = (plus.x + cx).clamp(8.0, (vw - w - 8.0).max(8.0));
            let top = plus.y - self.view().scroll + plus.h + 4.0;
            let floor = self.view().chrome_h + 8.0;
            let ceiling = (self.view().viewport.1 - h - 8.0).max(floor);
            let y = if top + h <= ceiling {
                top
            } else {
                (plus.y - self.view().scroll - h - 4.0).clamp(floor, ceiling)
            };
            return Some(Rect::new(x, y, w, h));
        }

        // An image rename belongs to the picture rather than the title. Prefer
        // below it, then above it; a picture taller than the viewport gets the
        // prompt inside its visible top edge.
        if let Anchor::RenameImage(node) = trigger.anchor {
            let row = self.body().row(node)?;
            let image = row.image.as_ref()?;
            let (image_x, image_w, _) = rowlayout::image_display_box(image, row.align, layout_w);
            let (w, h) = self.view().overlay.panel_size();
            let image_x = image_x + cx;
            let right = (vw - w - 8.0).max(8.0);
            let x = image_x.min(image_x + image_w - w).clamp(8.0, right);
            let top = row.y - self.view().scroll;
            let below = top + row.height + 4.0;
            let floor = self.view().chrome_h + 8.0;
            let ceiling = (self.view().viewport.1 - h - 8.0).max(floor);
            let y = if below <= ceiling {
                below
            } else if top - h - 4.0 >= floor {
                top - h - 4.0
            } else {
                (top + 8.0).clamp(floor, ceiling)
            };
            return Some(Rect::new(x, y, w, h));
        }

        let row = self.body().row(trigger.node)?;
        let cursor = Cursor::from_byte_index(&row.layout, trigger.start, Affinity::Downstream);
        let geo = cursor.geometry(&row.layout, 1.0);
        let (w, h) = self.view().overlay.panel_size();

        // The anchor can be far outside the viewport: the move picker anchors
        // at the *first* selected row, and a long mouse selection ends screens
        // below it. A panel pinned to an invisible caret is a panel nobody can
        // see — re-anchor it to the **last selected block**, which is where
        // the selection (and the eye) ended. A pointer anchor was tried first
        // and felt unmoored: the panel belongs to the selection, not to
        // wherever the mouse happened to rest.
        let anchor_top = row.y + geo.y0 as f32 - self.view().scroll;
        let anchor_bottom = row.y + geo.y1 as f32 - self.view().scroll;
        let floor = self.view().chrome_h + 8.0;
        let ceiling = (self.view().viewport.1 - h - 8.0).max(floor);
        if anchor_bottom < self.view().chrome_h || anchor_top > self.view().viewport.1 {
            let last = self.body().rows.iter().rev().find(|r| r.selected);
            let (ax, top, bottom) = match last {
                Some(last) => (
                    cx + last.text_x(),
                    last.y - self.view().scroll,
                    last.y + last.height - self.view().scroll,
                ),
                // No row selection (the anchor row itself was deleted under
                // an inline move): keep the original anchor, clamped below.
                None => (cx + row.text_x() + geo.x0 as f32, anchor_top, anchor_bottom),
            };
            let x = ax.clamp(8.0, (vw - w - 8.0).max(8.0));
            let below = bottom + 4.0;
            let y = if below <= ceiling {
                below.max(floor)
            } else {
                (top - h - 4.0).clamp(floor, ceiling)
            };
            return Some(Rect::new(x, y, w, h));
        }

        // An embedded editor names the room its panels may use, which reaches
        // past its own viewport — a composer's picker floats above it.
        let room = self
            .view()
            .embedded
            .map_or(Rect::new(0.0, 0.0, vw, self.view().viewport.1), |e| {
                e.panels
            });
        let (left, top, right, bottom) = (room.x, room.y, room.x + room.w, room.y + room.h);
        let x = (cx + row.text_x() + geo.x0 as f32)
            .clamp(left + 8.0, (right - w - 8.0).max(left + 8.0));
        let below = anchor_bottom + 4.0;
        let y = if below + h > bottom - 8.0 {
            (anchor_top - h - 4.0).max(top + 8.0)
        } else {
            below
        };
        Some(Rect::new(x, y, w, h))
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                let step = theme::FONT_SIZE * theme::LINE_HEIGHT * 3.0;
                (x * step, y * step)
            }
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        if self.view_mut().overlay.scroll(dy) {
            return;
        }
        // Extended tables own horizontal wheel motion while the pointer is
        // over them. Shift+wheel is the conventional fallback for mice with no
        // horizontal axis. Vertical document scrolling remains untouched.
        let horizontal = if dx.abs() > f32::EPSILON {
            Some(dx)
        } else if self.view().mods.shift_key() {
            Some(dy)
        } else {
            None
        };
        if let Some(amount) = horizontal {
            let (pointer_x, pointer_y) = self.view().pointer;
            let content_x = pointer_x - self.view().content_x;
            let document_y = pointer_y + self.view().scroll;
            if let Some(index) = self.body().row_at_y(document_y) {
                let row = &self.body().rows[index];
                if row.kind == Kind::Table
                    && self.view().table_source != Some(row.id)
                    && let Some(table) = row.table.as_ref()
                    && table.layout == koru_core::TableLayout::Extended
                    && let Some(grid) = table.grid.as_ref()
                    && grid.width > grid.viewport_width
                    && content_x >= row.text_x()
                    && content_x <= row.text_x() + grid.viewport_width
                {
                    let id = row.id;
                    let maximum = grid.width - grid.viewport_width;
                    let current = self.view().table_scroll.get(&id).copied().unwrap_or(0.0);
                    self.view_mut()
                        .table_scroll
                        .insert(id, (current - amount).clamp(0.0, maximum));
                    return;
                }
            }
        }
        let content_h = rowlayout::content_height(self.body());
        let view = self.view_mut();
        view.scroll_anim = None;
        view.scroll -= dy;
        view.clamp_scroll(content_h);
    }

    /// Forget formatting armed for the next keystroke.
    ///
    /// Arming belongs to an insertion point, so moving that point cancels it —
    /// otherwise pressing bold, changing your mind and arrowing away would
    /// embolden whatever you typed next, somewhere you had stopped looking.
    ///
    /// **A selected pill goes with it**, for exactly the same reason and to one
    /// rule rather than a clear at every call site: a ring left behind on a pill
    /// you have arrowed away from would make the next Enter open a document
    /// nobody was looking at.
    fn disarm(&mut self) {
        self.view_mut().selected_pill = None;
        self.view_mut().search_flash = None;
        if let Some(row) = self.body_mut().focused_mut() {
            row.disarm();
        }
    }

    /// Drop the caret: no active line, nothing selected, no open picker.
    fn deactivate(&mut self) {
        self.view_mut().active = false;
        self.view_mut().dragging = false;
        self.view_mut().overlay = Overlay::None;
        self.disarm();
        // Including the *row* selection, which is what the line above has always
        // claimed. Rows are drawn as selected whether or not a line is active,
        // so leaving the marks behind left rows visibly highlighted while every
        // key that acts on them — Ctrl+X, Ctrl+D, Tab — was being refused by
        // `key`'s inactive gate. Highlighted and dead is the worst of both.
        self.body_mut().clear_selection();
        if let Some(row) = self.body_mut().focused_mut() {
            row.selection = row.selection.collapse();
        }
    }

    /// If a press lands on a resize handle of the selected image, the drag to
    /// start. `None` when the focus is not on an image, the image is not
    /// selected, or the press is not on a handle.
    fn image_resize_start(&self, x: f32, y: f32) -> Option<ImageResize> {
        if !self.view().active {
            return None;
        }
        let view = self.view();
        let (node, r) = view.focused_image?;
        // The cached rect is in document coordinates; offset it to the window.
        let handle = image_handle_at(Rect::new(r.x, r.y - view.scroll, r.w, r.h), x, y)?;
        let row = self.body().rows.get(self.body().focus)?;
        let img = row.image.as_ref()?;
        let aspect = match (img.width, img.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => w as f32 / h as f32,
            _ => 0.0,
        };
        let layout_w = view.view_mode.effective_w(view.viewport.0);
        let column_w = (layout_w - 2.0 * theme::MARGIN_X).max(160.0);
        let start = rowlayout::image_display_size(img, layout_w);
        Some(ImageResize {
            node,
            handle,
            start,
            anchor: (x, y),
            aspect,
            column_w,
        })
    }

    /// The URL of the link pill under a point, if the pointer is over one.
    fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        // `x` is in the centred content column's space — callers subtract
        // `content_x` before handing it in.
        let doc_y = y + self.view().scroll;
        let i = self.body().row_at_y(doc_y)?;
        let row = &self.body().rows[i];
        let lx = x - row.text_x();
        let ly = doc_y - row.y;
        let attr = rowlayout::pill_at(row, &self.view().engine.pills, lx, ly)?;
        // The URL is on the cached row, so following a link never reads the
        // document — and a dangling one simply has none.
        row.pills
            .get(&attr)
            .filter(|info| info.kind == PillKind::Link)
            .and_then(|info| info.url.as_deref())
    }

    /// The link pill under a point: row, attribute, and URL.
    fn link_pill_at(&self, x: f32, y: f32) -> Option<(usize, AttrId, &str)> {
        let doc_y = y + self.view().scroll;
        let i = self.body().row_at_y(doc_y)?;
        let row = &self.body().rows[i];
        let lx = x - row.text_x();
        let ly = doc_y - row.y;
        let attr = rowlayout::pill_at(row, &self.view().engine.pills, lx, ly)?;
        let url = row
            .pills
            .get(&attr)
            .filter(|info| info.kind == PillKind::Link)
            .and_then(|info| info.url.as_deref())?;
        Some((i, attr, url))
    }

    fn pill_viewport_rect(&self, row: usize, attr: AttrId) -> Option<koru_draw::geometry::Rect> {
        let r = self.body().rows.get(row)?;
        let ox = self.view().content_x + r.text_x();
        let oy = r.y - self.view().scroll;
        let mut found = None;
        rowlayout::visit_pills(r, &self.view().engine.pills, |a, _, _, px, py, pw, ph| {
            if a == attr {
                found = Some(koru_draw::geometry::Rect::new(ox + px, oy + py, pw, ph));
            }
        });
        found
    }

    fn pill_at(&self, x: f32, y: f32) -> Option<(usize, AttrId)> {
        let doc_y = y + self.view().scroll;
        let i = self.body().row_at_y(doc_y)?;
        let row = &self.body().rows[i];
        let lx = x - row.text_x();
        let ly = doc_y - row.y;
        let attr = rowlayout::pill_at(row, &self.view().engine.pills, lx, ly)?;
        Some((i, attr))
    }

    fn document_pill_at(&self, ws: &WorkspaceDoc, x: f32, y: f32) -> Option<(usize, AttrId)> {
        let (i, attr) = self.pill_at(x, y)?;
        if self.entity_document_behind(ws, i, attr).is_some()
            || self.document_behind(ws, i, attr).is_some()
        {
            Some((i, attr))
        } else {
            None
        }
    }

    /// **The delegated run under a point, when a run pill is there.**
    ///
    /// The same rectangle [`document_pill_at`](Self::document_pill_at) hits
    /// and the same role check the click path makes, so a pill that opens a
    /// run on the left button and a pill that answers the right one cannot be
    /// two different pills.
    ///
    /// `x` is content-column x and `y` is **view** y.
    fn run_pill_at(&self, ws: &WorkspaceDoc, x: f32, y: f32) -> Option<koru_core::DocumentId> {
        let (row, attr) = self.pill_at(x, y)?;
        self.run_behind(ws, row, attr)
    }

    /// A schedule pill under a point, as its opaque storage id.
    fn schedule_pill_at(&self, ws: &WorkspaceDoc, x: f32, y: f32) -> Option<String> {
        let (row, attr) = self.pill_at(x, y)?;
        self.schedule_behind(ws, row, attr)
    }

    /// The visible pill for one schedule, in this editor's viewport. The
    /// container uses this as the inline rename anchor when the Agents column
    /// is not showing; identity, never the displayed label, chooses the pill.
    fn schedule_pill_rect(&self, schedule: &str) -> Option<koru_draw::geometry::Rect> {
        let key = crate::pillinfo::schedule_entity_key(schedule);
        let (row, attr) = self
            .body()
            .rows
            .iter()
            .enumerate()
            .find_map(|(row, paragraph)| {
                paragraph
                    .pills
                    .iter()
                    .find(|(_, info)| {
                        matches!(info.kind, PillKind::Schedule | PillKind::SchedulePaused)
                            && info.entity.as_deref() == Some(key.as_str())
                    })
                    .map(|(attr, _)| (row, *attr))
            })?;
        self.pill_viewport_rect(row, attr)
    }

    /// The attribute chip under a point, for a host that needs to know before
    /// it builds a menu.
    ///
    /// **The same `chips::chip_at` hover and click go through**, which is the
    /// whole reason it is here rather than a second copy of the arithmetic in
    /// the host: a chip that opens a picker on the left button and a chip that
    /// answers the right one have to be the same rectangle.
    ///
    /// `&mut self` for [`chip_at_point`](crate::Editor::chip_at_point)'s
    /// reason: measuring goes through the engine's chip cache, which is a
    /// `&mut` even when every label in it is already shaped.
    ///
    /// `x` is content-column x and `y` is **view** y, matching
    /// [`document_pill_at`] and the mouse_down path.
    fn attribute_chip_at(&mut self, x: f32, y: f32) -> Option<crate::chips::ChipHit> {
        let doc_y = y + self.view().scroll;
        let i = self.body().row_at_y(doc_y)?;
        let (body, view) = self.parts();
        let row = body.rows.get(i)?;
        if row.chips.is_empty() {
            return None;
        }
        crate::chips::chip_at(row, &mut view.engine, &view.tags, &view.lists, x, doc_y)
    }

    /// `x` is content-column x (viewport x minus `content_x`), matching
    /// [`document_pill_at`] and the mouse_down path.
    fn attachment_pill_at(&self, ws: &WorkspaceDoc, x: f32, y: f32) -> Option<(usize, AttrId)> {
        let doc_y = y + self.view().scroll;
        let i = self.body().row_at_y(doc_y)?;
        let row = &self.body().rows[i];
        let lx = x - row.text_x();
        let ly = doc_y - row.y;
        let attr = rowlayout::pill_at(row, &self.view().engine.pills, lx, ly)?;
        if self.file_behind(ws, i, attr).is_some() {
            return Some((i, attr));
        }
        // Already classified as a file pill during rebuild (either dialect).
        row.pills
            .get(&attr)
            .filter(|info| info.kind == koru_draw::pill::PillKind::File)
            .map(|_| (i, attr))
    }

    /// Delete the attachment pill's anchor character from the body. The file
    /// itself stays in the workspace Files listing.
    fn remove_attachment_pill(&mut self, ws: &mut WorkspaceDoc, row: usize, attr: AttrId) -> bool {
        let Some(r) = self.body().rows.get(row) else {
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!("[koru:attach] remove_attachment_pill missing row={row}");
            }
            return false;
        };
        let id = r.id;
        let Some(start) = r
            .runs
            .iter()
            .find(|run| run.pill == Some(attr))
            .map(|run| run.start.get())
        else {
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!(
                    "[koru:attach] remove_attachment_pill no anchor run row={row} attr={attr}"
                );
            }
            return false;
        };
        let end = start + ANCHOR_LEN;
        if ws.delete_text(id, start..end).is_err() {
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!(
                    "[koru:attach] remove_attachment_pill delete_text failed row={row} range={start}..{end}"
                );
            }
            return false;
        }
        if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
            eprintln!(
                "[koru:attach] remove_attachment_pill ok row={row} attr={attr} range={start}..{end}"
            );
        }
        self.body_mut().rebuild(ws);
        self.reflow();
        true
    }

    fn ask_file_action(
        &mut self,
        ws: &WorkspaceDoc,
        row: usize,
        attr: AttrId,
        action: crate::FileAction,
    ) -> bool {
        let Some(file) = self.file_behind(ws, row, attr) else {
            return false;
        };
        self.view_mut().wants_file_action = Some((file, action));
        true
    }

    fn follow_document_pill(
        &mut self,
        ws: &WorkspaceDoc,
        row: usize,
        attr: AttrId,
        where_: OpenWhere,
    ) -> bool {
        // **A run is not a page.** A run pill's reference resolves to a
        // `koru-run` document, which has no node tree: opening it in a
        // document tab would show an empty page. It goes out on its own
        // request, which the container answers with the run's own views. This
        // check comes first because `as_document_reference` answers for both.
        if let Some(schedule) = self.schedule_behind(ws, row, attr) {
            self.view_mut().wants_schedule_open = Some((schedule, where_));
            return true;
        }
        if let Some(run) = self.run_behind(ws, row, attr) {
            self.view_mut().wants_run_open = Some((run, where_));
            return true;
        }
        if let Some(target) = self.entity_document_behind(ws, row, attr) {
            self.view_mut().wants_entity_open = Some((target, where_));
            return true;
        }
        if let Some(target) = self.document_behind(ws, row, attr) {
            self.view_mut().wants_open = Some((target, where_));
            return true;
        }
        false
    }

    /// The delegated run a pill points at, when the pill is a run pill.
    ///
    /// Keyed on the role, not on the value: the value is a document reference
    /// either way, and it is the role that says the document is a run.
    fn run_behind(
        &self,
        ws: &WorkspaceDoc,
        row: usize,
        attr: AttrId,
    ) -> Option<koru_core::DocumentId> {
        let node = ws.read(self.body().rows.get(row)?.id).ok()?;
        let found = node
            .content
            .attrs()
            .iter()
            .find(|a| a.id == attr)
            .filter(|a| a.role == koru_core::attributes::ROLE_RUN)?;
        Some(found.as_document_reference()?.document)
    }

    fn schedule_behind(&self, ws: &WorkspaceDoc, row: usize, attr: AttrId) -> Option<String> {
        let node = ws.read(self.body().rows.get(row)?.id).ok()?;
        let found = node
            .content
            .attrs()
            .iter()
            .find(|a| a.id == attr)
            .filter(|a| a.role == koru_core::attributes::ROLE_SCHEDULE)?;
        match &found.value {
            AttrValue::Text(schedule) if !schedule.is_empty() => Some(schedule.clone()),
            _ => None,
        }
    }

    /// The run a click or an Enter asked to open, if any. Taken by the
    /// container, which is the only thing that can put a run on screen.
    fn take_wants_run_open(&mut self) -> Option<(koru_core::DocumentId, OpenWhere)> {
        self.view_mut().wants_run_open.take()
    }

    /// The run a right-click asked for a menu about, if any. Taken by the
    /// container, which is the only thing that knows what a run produced.
    fn take_wants_run_menu(&mut self) -> Option<koru_core::DocumentId> {
        self.view_mut().wants_run_menu.take()
    }

    fn take_wants_schedule_open(&mut self) -> Option<(String, OpenWhere)> {
        self.view_mut().wants_schedule_open.take()
    }

    fn take_wants_schedule_menu(&mut self) -> Option<String> {
        self.view_mut().wants_schedule_menu.take()
    }

    fn take_wants_toast(&mut self) -> Option<String> {
        self.view_mut().wants_toast.take()
    }

    fn take_wants_stay_focused(&mut self) -> bool {
        std::mem::take(&mut self.view_mut().wants_stay_focused)
    }

    /// Recompute what the pointer is over. Returns true if it changed, so the
    /// caller only redraws when the hover actually moved.
    /// How far into its appearance the hovered thing's tooltip is: `0.0` while
    /// the pointer has not rested long enough, rising to `1.0` over the fade.
    ///
    /// One function, so the frame that draws it and the `animating` that asks
    /// for that frame cannot disagree about whether it is up yet.
    fn tooltip_progress(&self) -> f32 {
        let waited = self.view().hover_since.elapsed().as_millis() as f32
            - theme::motion::tooltip().hover_delay.as_millis() as f32;
        (waited / theme::motion::tooltip().enter_duration.as_millis() as f32).clamp(0.0, 1.0)
    }

    /// Whether a tooltip is mid-wait or mid-fade, so the shell keeps waking
    /// this view. Returns to `false` at the end of the fade (ADR-26).
    fn tooltip_animating(&self) -> bool {
        self.hovered_plus_rect().is_some() && self.tooltip_progress() < 1.0
    }

    fn table_controls_alpha(&self, id: NodeId, now: Instant) -> f32 {
        if self.view().table_controls_node == Some(id) {
            self.view().table_controls.visibility(now)
        } else {
            0.0
        }
    }

    /// Only the shared tooltip clock decides when table furniture exists.
    /// Waiting is one maintenance deadline; appearing and disappearing are the
    /// only phases that request animation frames.
    fn table_controls_animating(&self, now: Instant) -> bool {
        self.view().table_controls.animating(now)
    }

    fn table_controls_maintenance_deadline(&self) -> Option<Instant> {
        self.view().table_controls.maintenance_deadline()
    }

    fn update_table_controls_hover(&mut self, hover: Option<Hover>, now: Instant) -> bool {
        let target = hover.and_then(Hover::row).and_then(|index| {
            let row = self.body().rows.get(index)?;
            if row.diagram_shows_picture() {
                // A drawn diagram's one control rides the same clock, so the
                // reveal feels the same on every block.
                return Some((row.id, self.diagram_control_content(row)));
            }
            (row.kind == Kind::Table).then(|| {
                let raw = row.id.as_uuid().as_u128();
                let key = raw as u64 ^ (raw >> 64) as u64;
                let anchor = Rect::new(row.text_x(), row.y, self.view().viewport.0, row.height);
                (
                    row.id,
                    koru_ui::TooltipContent::new(key, anchor, "Table controls"),
                )
            })
        });

        let view = self.view_mut();
        let old_node = view.table_controls_node;
        let changed = match target {
            Some((id, content)) => {
                view.table_controls_node = Some(id);
                view.table_controls.set_hover(Some(content), now)
            }
            None => {
                let changed = view.table_controls.set_hover(None, now);
                if !view.table_controls.is_present() {
                    view.table_controls_node = None;
                }
                changed
            }
        };
        changed || old_node != view.table_controls_node
    }

    fn update_copy_hover(&mut self, hover: Option<Hover>, now: Instant) -> bool {
        let slot = match hover {
            Some(Hover::Copy(i)) => self.body().rows.get(i).map(|row| {
                let layout_w = self.view().view_mode.effective_w(self.view().viewport.0);
                let chip = rowlayout::copy_chip_rect(row, layout_w);
                Rect::new(
                    self.view().content_x + chip.x,
                    chip.y - self.view().scroll,
                    chip.w,
                    chip.h,
                )
            }),
            _ => None,
        };
        self.view_mut().copy.set_hover(slot, now)
    }

    fn update_task_warning_hover(&mut self, hover: Option<Hover>, now: Instant) -> bool {
        let content = match hover {
            Some(Hover::TaskWarning(i)) => {
                let (content_x, scroll) = (self.view().content_x, self.view().scroll);
                let (body, view) = self.parts();
                body.rows.get(i).and_then(|row| {
                    let rect = rowlayout::task_warning_rect(
                        row,
                        &mut view.engine,
                        &view.tags,
                        &view.lists,
                    )?;
                    // A key per *row*, from its node id, so moving between two
                    // warned rows re-opens the tooltip rather than sliding one.
                    let raw = row.id.as_uuid().as_u128();
                    let key = raw as u64 ^ (raw >> 64) as u64;
                    let anchor = Rect::new(content_x + rect.x, rect.y - scroll, rect.w, rect.h);
                    Some(koru_ui::TooltipContent::new(
                        key,
                        anchor,
                        TASK_WARNING_TOOLTIP,
                    ))
                })
            }
            _ => None,
        };
        self.view_mut().task_warning.set_hover(content, now)
    }

    fn task_warning_animating(&self, now: Instant) -> bool {
        self.view().task_warning.animating(now)
    }

    fn task_warning_maintenance_deadline(&self) -> Option<Instant> {
        self.view().task_warning.maintenance_deadline()
    }

    fn maintain_task_warning(&mut self, now: Instant) -> bool {
        self.view_mut().task_warning.maintain(now)
    }

    fn copy_affordance_animating(&self, now: Instant) -> bool {
        self.view().copy.animating(now)
    }

    fn copy_affordance_maintenance_deadline(&self) -> Option<Instant> {
        self.view().copy.maintenance_deadline()
    }

    fn maintain_copy_affordance(&mut self, now: Instant) -> bool {
        self.view_mut().copy.maintain(now)
    }

    fn maintain_table_controls(&mut self, now: Instant) -> bool {
        let changed = self.view_mut().table_controls.maintain(now);
        if !self.view().table_controls.is_present() {
            self.view_mut().table_controls_node = None;
        }
        if changed {
            // The reveal may have made a control appear under a stationary
            // pointer. Re-run hit-testing so its cursor and hover tint agree
            // without requiring a one-pixel mouse move.
            let (x, y) = self.view().pointer;
            let cx = x - self.view().content_x;
            self.update_hover(cx, y) || changed
        } else {
            false
        }
    }

    fn update_hover(&mut self, x: f32, y: f32) -> bool {
        let doc_y = y + self.view_mut().scroll;
        let mut next = self.hover_at(x, doc_y);
        if !matches!(next, Some(Hover::Pill { .. }))
            && self.view().link_chip.as_ref().is_some_and(|chip| {
                chip.rect
                    .contains(self.view().pointer.0, self.view().pointer.1)
            })
        {
            next = self.view().hover;
        }
        let changed = next != self.view_mut().hover;
        if changed {
            self.view_mut().hover_since = Instant::now();
        }
        self.view_mut().hover = next;
        let now = Instant::now();
        let table_controls_changed = self.update_table_controls_hover(next, now);
        let copy_changed = self.update_copy_hover(next, now);
        let warning_changed = self.update_task_warning_hover(next, now);
        if let Some(Hover::PanelRow(item)) = next {
            self.view_mut().overlay.hover_item(item);
        }
        // Tone-strip hover is separate from grid `sel`: update it whenever the
        // pointer is over the emoji panel so cells light without moving the grid.
        let tone_moved = if let Some(rect) = self.overlay_panel_rect() {
            let (px, py) = (x - rect.x, (doc_y - self.view().scroll) - rect.y);
            match &mut self.view_mut().overlay {
                Overlay::Emoji(picker) if picker.tone_strip.is_some() => {
                    picker.set_tone_hover(px, py)
                }
                Overlay::Emoji(picker) => {
                    let cleared = picker.tone_hover.take().is_some();
                    cleared
                }
                _ => false,
            }
        } else {
            false
        };
        changed || tone_moved || table_controls_changed || copy_changed || warning_changed
    }

    /// The attribute chip under a point on row `i`, in content-column x and
    /// document y.
    ///
    /// **The one seam between the pointer and `chips::chip_at`.** It takes
    /// `&mut self` for one reason: measuring goes through the text engine's
    /// chip cache, which is a `&mut` even when every label in it is already
    /// shaped — so hover and click share the geometry rather than input
    /// growing a read-only copy of it that would drift. `parts` is what makes
    /// the row and the engine reachable at once.
    ///
    /// Free when the row has no chips, which is almost every row.
    fn chip_at_point(&mut self, i: usize, x: f32, doc_y: f32) -> Option<crate::chips::ChipHit> {
        let (body, view) = self.parts();
        let row = body.rows.get(i)?;
        if row.chips.is_empty() {
            return None;
        }
        crate::chips::chip_at(row, &mut view.engine, &view.tags, &view.lists, x, doc_y)
    }

    /// Whether a point is on row `i`'s unfinished-items triangle, in the same
    /// coordinates [`chip_at_point`](Self::chip_at_point) takes.
    ///
    /// `&mut self` for the same reason: the rect is measured past the row's
    /// chips, and measuring a chip goes through the engine's cache. Free on
    /// every row that is not a completed task carrying unfinished work — the
    /// rect function returns `None` before it measures anything.
    fn task_warning_at(&mut self, i: usize, x: f32, doc_y: f32) -> bool {
        let (body, view) = self.parts();
        let Some(row) = body.rows.get(i) else {
            return false;
        };
        rowlayout::task_warning_rect(row, &mut view.engine, &view.tags, &view.lists)
            .is_some_and(|rect| rect.contains(x, doc_y))
    }

    /// The exact host-supplied reminder pill under a point on row `i`.
    fn reminder_at_point(&mut self, i: usize, x: f32, doc_y: f32) -> Option<usize> {
        let (body, view) = self.parts();
        let row = body.rows.get(i)?;
        let reminders = view.reminder_chips.get(&row.id)?;
        crate::draw::reminder_indicator_rects(
            row,
            &mut view.engine,
            &view.tags,
            &view.lists,
            reminders,
            view.view_mode.effective_w(view.viewport.0),
            row.y,
        )
        .iter()
        .position(|rect| rect.contains(x, doc_y))
    }

    fn hover_at(&mut self, x: f32, doc_y: f32) -> Option<Hover> {
        // An open panel owns the pointer: hovering a row pre-selects it.
        if self.view().overlay.is_open() {
            if let Some(rect) = self.overlay_panel_rect() {
                // **Map into the rect's space before asking.** `x` arrives in
                // content-column coordinates — the space `plus_rect` and the
                // row furniture below are hit-tested in — but a panel rect is
                // viewport space, the frame `draw_overlay` paints and
                // `mouse_down` clicks in. Compared unmapped, `contains` missed
                // by exactly `content_x`: on the narrow language dropdown that
                // is most of its width, so the pointer "left" the panel, fell
                // through to the document under it, and hovered text wearing
                // the I-beam over entries that lit nothing.
                let vx = x + self.view().content_x;
                let vy = doc_y - self.view().scroll;
                if rect.contains(vx, vy) {
                    let (px, py) = (vx - rect.x, vy - rect.y);
                    // A field is text and wears the I-beam. Asked before the row
                    // arithmetic, and with its *own* un-padded y: `field_rect`
                    // lives in the space that includes `PANEL_PAD`, while the
                    // row index below has already taken the pad off.
                    if self.view().overlay.field_at(px, py, rect.w) {
                        return Some(Hover::PanelField);
                    }
                    // A control before a row: the emoji picker's category strip
                    // (and its tone strip) are not grid cells, and calling them
                    // one would drag the selection underneath as the pointer
                    // crossed them.
                    if let Some(part) = self.view().overlay.control_at(px, py) {
                        return Some(match part {
                            PanelPart::Button => Hover::PanelControl,
                            PanelPart::Thumb => Hover::Thumb,
                        });
                    }
                    return self
                        .view()
                        .overlay
                        .item_at(px, py)
                        .map(Hover::PanelRow)
                        .or_else(|| {
                            self.view()
                                .overlay
                                .pointer_surface()
                                .then_some(Hover::PanelControl)
                        });
                }
            }
        }

        if self.title_menu_rect().is_some_and(|r| r.contains(x, doc_y)) {
            return Some(Hover::TitleMenu);
        }
        let i = self.body().row_at_y(doc_y)?;
        // The lane before the row: `+`, then grip, then the checkbox column.
        if self.plus_rect(i).is_some_and(|r| r.contains(x, doc_y)) {
            return Some(Hover::Plus(i));
        }
        if self.grip_rect(i).is_some_and(|r| r.contains(x, doc_y)) {
            return Some(Hover::Grip(i));
        }
        if self.checkbox_rect(i).is_some_and(|r| r.contains(x, doc_y)) {
            return Some(Hover::Checkbox(i));
        }
        if let Some(reminder) = self.reminder_at_point(i, x, doc_y) {
            return Some(Hover::Reminder { row: i, reminder });
        }
        // An attribute chip trails the row's text, past the last line's
        // advance — so it overlaps nothing else and can be asked here, where
        // `self` is still free to be borrowed mutably. Routed through the same
        // `chips::chip_at` the press uses, which is what keeps the pointing
        // hand and the picker over one rectangle rather than two copies of the
        // arithmetic.
        if let Some(hit) = self.chip_at_point(i, x, doc_y) {
            return Some(Hover::Chip {
                row: i,
                chip: hit.index,
            });
        }
        // The unfinished-items triangle sits past the last chip, in the same
        // space and asked through the same geometry function the renderer used.
        if self.task_warning_at(i, x, doc_y) {
            return Some(Hover::TaskWarning(i));
        }
        let row = &self.body().rows[i];
        if row.image.is_some() || row.kind == koru_core::node::Kind::Divider {
            // A non-text block belongs to its row for hover and dragging, but
            // wears the grab hand rather than an I-beam (reuses `Hover::Image`).
            return Some(Hover::Image(i));
        }
        // A drawn diagram is a picture too — except for its controls at the
        // panel's corner, once the clock has revealed them. Asked before the
        // code block's chips, which a picture does not wear.
        if row.diagram_shows_picture() {
            let layout_w = self.view().view_mode.effective_w(self.view().viewport.0);
            if self.table_controls_alpha(row.id, Instant::now()) > 0.0
                && let Some(control) = crate::diagram::Control::ALL.into_iter().find(|&control| {
                    rowlayout::diagram_control_rect(row, layout_w, control).contains(x, doc_y)
                })
            {
                return Some(Hover::DiagramControl { row: i, control });
            }
            return Some(Hover::Image(i));
        }
        if row.kind == Kind::Table
            && let Some(table) = row.table.as_ref()
        {
            let source_visible = self.view().table_source == Some(row.id) || table.grid.is_none();
            let controls_visible = self.table_controls_alpha(row.id, Instant::now()) > 0.0;
            if controls_visible
                && crate::table::mode_control_rect(row, table, row.y).contains(x, doc_y)
            {
                return Some(Hover::TableMode(i));
            }
            if controls_visible
                && !source_visible
                && crate::table::control_rect(row, row.y).contains(x, doc_y)
            {
                return Some(Hover::TableLayout(i));
            }
        }
        let lx = x - row.text_x();
        let ly = doc_y - row.y;
        if let Some(attr) = rowlayout::pill_at(row, &self.view().engine.pills, lx, ly) {
            return Some(Hover::Pill { row: i, attr });
        }
        // The language chip at the corner of a code block opens the picker.
        // The column's width, not the window's — the same number `draw_document`
        // draws the chip against, or the two disagree by `content_x` in Page
        // and Narrow.
        let layout_w = self.view().view_mode.effective_w(self.view().viewport.0);
        if row.kind == Kind::Code && rowlayout::copy_chip_rect(row, layout_w).contains(x, doc_y) {
            return Some(Hover::Copy(i));
        }
        // The *Done* check, left of the copy icon, only while a diagram's text
        // is open: it is the way back to the picture for a hand on the mouse.
        if row.diagram.as_ref().is_some_and(|state| state.open)
            && rowlayout::diagram_done_rect(row, layout_w).contains(x, doc_y)
        {
            return Some(Hover::DiagramDone(i));
        }
        if row.kind == Kind::Code && rowlayout::lang_chip_rect(row, layout_w).contains(x, doc_y) {
            return Some(Hover::Lang(i));
        }
        // Tone-capable emoji: still text under the caret, and a long-press
        // opens the skin-tone strip.
        if row.tone_emoji_at(lx, ly).is_some() {
            return Some(Hover::Emoji(i));
        }
        Some(Hover::Text(i))
    }

    /// Returns true if a redraw is needed.
    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        // A new press is a new drag, of whichever of the three kinds. Cleared
        // here rather than in each arm, so a press that lands on a row cannot
        // inherit the last field drag.
        self.view_mut().drag_field = false;
        self.view_mut().drag_scroll_tick = None;
        self.view_mut().overlay.stop_scrollbar_drag();
        // Body tone strip owns the pointer while open.
        if self.view().body_tone_strip.is_some() {
            return self.body_tone_strip_click(ws, x, y);
        }
        // Rows and everything anchored to them live in the centred content
        // column; the scrollbar lives at the viewport's right edge. Map the
        // pointer into the column once, and keep `x` for the scrollbar gutter.
        let cx = x - self.view().content_x;
        self.view_mut().table_cell = None;
        if self.view_mut().overlay.is_open() {
            if let Some(rect) = self.overlay_panel_rect() {
                if rect.contains(x, y) {
                    return self.overlay_click(ws, rect, x, y);
                }
            }
            // A click outside the panel dismisses it and then falls through to
            // placing the caret, which is what every editor does.
            self.view_mut().overlay = Overlay::None;
        }

        // The scrollbar claims presses in its right-edge column before the rows
        // do — a click on the thumb starts a drag, a click in the band above or
        // below it pages. Skipped when nothing scrolls, since `scrollbar_geom`
        // is `None` then.
        if let Some(g) = self
            .view()
            .scrollbar_geom(rowlayout::content_height(self.body()))
        {
            if g.in_gutter(x, y) {
                if g.on_thumb(x, y) {
                    // Grab the thumb where it is, so it does not jump under the
                    // pointer. The stored value is the offset from thumb top.
                    self.view_mut().scroll_drag = Some(y - g.thumb_top);
                } else {
                    let content_h = rowlayout::content_height(self.body());
                    let view = self.view_mut();
                    view.scroll_anim = None;
                    // A click in the band pages towards wherever the pointer is.
                    view.scroll += if y < g.thumb_top {
                        -view.content_viewport_h()
                    } else {
                        view.content_viewport_h()
                    };
                    view.clamp_scroll(content_h);
                }
                return true;
            }
        }

        // A resize handle on the selected image claims the press before the rows
        // do — but only once the image is selected (focused), so the first click
        // on a picture selects it rather than starting a resize.
        if let Some(drag) = self.image_resize_start(cx, y) {
            self.view_mut().image_resize = Some(drag);
            return true;
        }

        let doc_y = y + self.view_mut().scroll;

        if let Some(chip) = self.view().link_chip.clone() {
            match chip.hit(x, y) {
                LinkChipHit::Copy => {
                    let now = Instant::now();
                    self.view_mut().copy.click(&chip.url, now);
                    self.view_mut().link_chip = None;
                    self.view_mut().wants_toast = Some(koru_ui::CopyAffordance::TOAST.to_string());
                    return true;
                }
                LinkChipHit::Edit => {
                    self.view_mut().link_chip = None;
                    if let Some(open) = self.open_pill_picker(ws, chip.row, chip.attr) {
                        self.view_mut().overlay = open;
                        self.view_mut().active = true;
                        self.reflow();
                    }
                    return true;
                }
                LinkChipHit::Body | LinkChipHit::None => {}
            }
        }

        if let Some((i, attr)) = self.document_pill_at(ws, cx, y) {
            let where_ = if ctrl {
                OpenWhere::BackgroundTab
            } else if self.view().mods.shift_key() {
                OpenWhere::NewWindow
            } else {
                OpenWhere::SameTab
            };
            let followed = self.follow_document_pill(ws, i, attr, where_);
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] editor press hit=document-pill followed={followed} where={where_:?}"
            ));
            return followed;
        }
        if let Some(url) = self.link_at(cx, y).map(str::to_owned) {
            let opened = if ctrl {
                self.view_mut().wants_stay_focused = true;
                crate::browser::open_without_activating(&url)
            } else {
                crate::browser::open(&url)
            };
            let scheme = url.split(':').next().unwrap_or("?");
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] editor press hit=link scheme={scheme} opened={}",
                opened.is_ok(),
            ));
            if let Err(e) = opened {
                eprintln!("{e}");
            }
            return true;
        }

        // Reminder metadata opens management for this exact object and never
        // places a caret in the row underneath it.
        if let Some(i) = self.body().row_at_y(doc_y)
            && let Some(reminder) = self.reminder_at_point(i, cx, doc_y)
        {
            let row = &self.body().rows[i];
            let Some(chip) = self
                .view()
                .reminder_chips
                .get(&row.id)
                .and_then(|chips| chips.get(reminder))
            else {
                return false;
            };
            self.view_mut().wants_reminder_management = Some((row.id, chip.reminder_id.clone()));
            return true;
        }

        // An attribute chip opens the picker for the attribute it shows, over
        // the node whose row it trails — the same panel the `/`-command opens,
        // seeded from the same document state. A tag chip's trailing `×` is
        // the exception and removes that one tag outright.
        //
        // It claims the press outright and returns: a chip is presentation of
        // node state hanging off the row, never text, so also placing the
        // caret would move the insertion point every time somebody edited a due
        // date, and a drag from a chip would start selecting the line under it.
        if let Some(i) = self.body().row_at_y(doc_y)
            && let Some(hit) = self.chip_at_point(i, cx, doc_y)
        {
            return self.press_chip(ws, i, hit);
        }

        let Some(i) = self.body_mut().row_at_y(doc_y) else {
            // Clicking nothing drops the caret, the way clicking a canvas
            // deselects. Both views want the opposite of that in the empty page
            // *below* the rows and intercept before delegating here — see
            // `BuiltInDocument::mouse_down` and `TaskOutline::mouse_down`. What
            // is left for this arm is the chrome, and the gaps between rows.
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] editor press hit=empty-deactivate"
            ));
            self.deactivate();
            return true;
        };
        let prior_focus = self.body().focus;
        self.view_mut().desired_x = None;
        self.disarm();
        ws.checkpoint();

        // Shift extends from the existing insertion point. Inside one text row
        // that is a character selection; across rows it becomes the same block
        // range a drag or Shift+Arrow creates.
        if self.view().mods.shift_key() {
            let same_text_row = i == prior_focus
                && !self.body().has_row_selection()
                && self.body().rows[i].image.is_none()
                && self.body().rows[i].kind.shape() == koru_core::node::Shape::Text;
            if same_text_row {
                let row = &mut self.body_mut().rows[i];
                let lx = cx - row.text_x();
                let ly = (doc_y - row.y).clamp(0.0, row.height.max(1.0) - 0.5);
                row.selection = row.selection.extend_to_point(&row.layout, lx, ly);
                row.snap_selection_to_graphemes();
            } else {
                self.body_mut().focus = prior_focus;
                self.body_mut().extend_selection_to(i);
            }
            let view = self.view_mut();
            view.active = true;
            view.dragging = false;
            view.drag_scroll_tick = None;
            return true;
        }

        // Ctrl toggles whole blocks. URL pills keep their established
        // Ctrl+click-to-open behavior because they were handled above.
        if ctrl && !self.body().is_title_row(i) {
            koru_ui::keys::debug::write(format_args!(
                "[Koru][input] editor press hit=block-toggle row={i}"
            ));
            self.body_mut().toggle_selection(i);
            let view = self.view_mut();
            view.active = true;
            view.dragging = false;
            view.drag_scroll_tick = None;
            self.reflow();
            return true;
        }

        self.body_mut().focus = i;

        // A table exposes its source/grid projection directly on the block. Its
        // other control changes only the synced width policy. Cell presses
        // select the canonical source range behind that cell, so typing still
        // replaces precisely one cell without a second table model.
        if self.body().rows[i].kind == Kind::Table {
            let row = &self.body().rows[i];
            let Some(table) = row.table.as_ref() else {
                return true;
            };
            let id = row.id;
            let source_visible = self.view().table_source == Some(id) || table.grid.is_none();
            let mode_control = crate::table::mode_control_rect(row, table, row.y);
            let controls_visible = self.table_controls_alpha(id, Instant::now()) > 0.0;
            if controls_visible && mode_control.contains(cx, doc_y) {
                return self.toggle_table_view();
            }
            if controls_visible
                && !source_visible
                && crate::table::control_rect(row, row.y).contains(cx, doc_y)
            {
                let id = row.id;
                let next = match row.table_layout {
                    koru_core::TableLayout::Compact => koru_core::TableLayout::Extended,
                    koru_core::TableLayout::Extended => koru_core::TableLayout::Compact,
                };
                ws.checkpoint();
                let _ = ws.set_table_layout(id, next);
                ws.checkpoint();
                if next == koru_core::TableLayout::Compact {
                    self.view_mut().table_scroll.remove(&id);
                }
                self.body_mut().refresh(ws, id);
                self.reflow();
                return true;
            }
            if !source_visible {
                let scroll_x = self
                    .view()
                    .table_scroll
                    .get(&row.id)
                    .copied()
                    .unwrap_or(0.0);
                if let Some(visual) = row.table.as_ref()
                    && let Some((table_row, column)) =
                        crate::table::cell_at(row, visual, cx, doc_y, scroll_x)
                    && let Some(range) = crate::table::source_range(visual, table_row, column)
                {
                    let id = row.id;
                    let row = &mut self.body_mut().rows[i];
                    row.selection =
                        Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream)
                            .extend(Cursor::from_byte_index(
                                &row.layout,
                                range.end,
                                Affinity::Upstream,
                            ));
                    self.view_mut().active = true;
                    self.view_mut().table_cell = Some((id, table_row, column));
                    self.view_mut().notice = Some(format!(
                        "Editing table cell {}:{}; type to replace, Ctrl+Enter adds a paragraph below",
                        table_row + 1,
                        column + 1
                    ));
                    self.view_mut().table_source = None;
                    self.body_mut().focus_on(id);
                    return true;
                }
            }
        }

        // The language chip at the corner of a code block opens the picker. The
        // chip is measured against the column's width and lives in content-column
        // x — so the hit-test matches what `draw_document` draws and `hover_at`
        // reports, rather than disagreeing by `content_x` in Page/Narrow.
        let layout_w = self.view().view_mode.effective_w(self.view().viewport.0);
        // A diagram showing its picture is selected, not edited — the image
        // pattern: focus it, arm a drag from the drawing, place no caret. Its
        // controls open the viewer, copy and save the picture, and open the
        // text; its chips are not drawn, so they are not asked. The caret
        // the picture hid is collapsed, so a Copy takes the whole block
        // rather than a range nobody can see.
        if self.body().rows[i].diagram_shows_picture() {
            let row = &self.body().rows[i];
            let id = row.id;
            if self.table_controls_alpha(id, Instant::now()) > 0.0
                && let Some(control) = crate::diagram::Control::ALL.into_iter().find(|&control| {
                    rowlayout::diagram_control_rect(row, layout_w, control).contains(cx, doc_y)
                })
            {
                return match control {
                    crate::diagram::Control::Open => self.ask_open_diagram(id),
                    crate::diagram::Control::Copy => self.ask_copy_diagram(id),
                    crate::diagram::Control::Save => self.ask_save_diagram(id),
                    crate::diagram::Control::Edit => self.open_diagram_text(id),
                };
            }
            if self.grip_rect(i).is_some_and(|r| r.contains(cx, doc_y)) {
                self.view_mut().drag_row = Some(DragRow { from: i, to: i });
                return true;
            }
            let on_block = match row.diagram.as_ref().and_then(|state| {
                rowlayout::diagram_layout(row, state, row.height, layout_w).drawing
            }) {
                Some(drawing) => Rect::new(drawing.x, row.y + drawing.y, drawing.w, drawing.h)
                    .contains(cx, doc_y),
                // The placeholder is the whole block, like a divider.
                None => true,
            };
            self.body_mut().clear_selection();
            {
                let row = &mut self.body_mut().rows[i];
                row.selection = row.selection.collapse();
            }
            self.view_mut().active = true;
            if on_block {
                self.view_mut().drag_row = Some(DragRow { from: i, to: i });
            }
            self.reflow();
            return true;
        }
        if self.body().rows[i].kind == Kind::Code
            && rowlayout::copy_chip_rect(&self.body().rows[i], layout_w).contains(cx, doc_y)
        {
            let text = self.body().rows[i].text.clone();
            let now = Instant::now();
            self.view_mut().copy.click(&text, now);
            self.view_mut().wants_toast = Some(koru_ui::CopyAffordance::TOAST.to_string());
            return true;
        }
        // *Done* closes the open diagram text, as Escape does: the block is
        // its picture again, still the selected row.
        if self.body().rows[i]
            .diagram
            .as_ref()
            .is_some_and(|state| state.open)
            && rowlayout::diagram_done_rect(&self.body().rows[i], layout_w).contains(cx, doc_y)
        {
            let node = self.body().rows[i].id;
            self.close_diagram_text();
            self.body_mut().clear_selection();
            self.body_mut().focus_on(node);
            self.view_mut().active = true;
            self.reflow();
            return true;
        }
        if self.body().rows[i].kind == Kind::Code
            && rowlayout::lang_chip_rect(&self.body().rows[i], layout_w).contains(cx, doc_y)
        {
            let node = self.body().rows[i].id;
            let current = self.body().rows[i].language.clone();
            let mut picker = language_picker::LanguagePicker::open_for(node, current.as_deref());
            picker.width =
                language_picker::LanguagePicker::measure_width(&mut self.view_mut().engine);
            self.view_mut().overlay = Overlay::Language(picker);
            self.view_mut().active = true;
            self.reflow();
            return true;
        }

        // The grip is block furniture for images too. It must be asked before
        // the image branch returns, or its visible handle cannot begin a drag.
        if self.grip_rect(i).is_some_and(|r| r.contains(cx, doc_y)) {
            self.view_mut().drag_row = Some(DragRow { from: i, to: i });
            return true;
        }

        // A non-text block — an image or a divider — is selected, not edited:
        // focus it (so the resize/select furniture appears) and stop, rather
        // than placing a caret in its empty text.
        let row = &self.body().rows[i];
        let is_block = row.image.is_some() || row.kind == koru_core::node::Kind::Divider;
        if is_block {
            // The whole divider is the block; an image only counts presses on
            // its picture rect as on-block. The picture rect is in content
            // space (centred column), so use the effective page width and the
            // content-mapped pointer `cx`.
            let on_block = match row.image.as_ref() {
                Some(image) => {
                    let (ix, iw, ih) = rowlayout::image_display_box(
                        image,
                        row.align,
                        self.view().view_mode.effective_w(self.view().viewport.0),
                    );
                    Rect::new(ix, row.y, iw, ih).contains(cx, doc_y)
                }
                None => true,
            };
            self.body_mut().clear_selection();
            self.view_mut().active = true;
            if on_block {
                self.view_mut().drag_row = Some(DragRow { from: i, to: i });
            }
            self.reflow();
            return true;
        }

        // Grip: start a reorder drag rather than touching the text.
        if self.checkbox_rect(i).is_some_and(|r| r.contains(cx, doc_y)) {
            let (id, done) = (self.body_mut().rows[i].id, self.body_mut().rows[i].done);
            self.body_mut().set_done(ws, id, !done);
            self.reflow();
            return true;
        }

        // A document pill navigates; editable value pills reopen their picker.
        let (body, view) = self.parts();
        let row = &body.rows[i];
        let (lx, ly) = (cx - row.text_x(), doc_y - row.y);
        if let Some(attr) = rowlayout::pill_at(row, &view.engine.pills, lx, ly) {
            // A document pill opens the document. Its title is the node's own
            // text, edited at the top of its own window — so there is no rename
            // prompt to show here.
            if self.follow_document_pill(ws, i, attr, OpenWhere::SameTab) {
                koru_ui::keys::debug::write(format_args!(
                    "[Koru][input] editor press hit=document-pill followed=true where=SameTab"
                ));
                return true;
            }
            // An attachment opens in the OS-associated app. Never the rename
            // prompt — that path used to overwrite the reference with dead text.
            if self.follow_file_pill(ws, i, attr) {
                return true;
            }
            if let Some(open) = self.open_pill_picker(ws, i, attr) {
                self.view_mut().overlay = open;
                // Put the caret just after the pill so the panel anchors sanely
                // and Escape leaves you somewhere sensible.
                self.view_mut().active = true;
                self.reflow();
                return true;
            }
        }

        self.view_mut().active = true;
        // A press starts a new selection, so whatever the last one covered goes
        // — including a multi-row one built with Shift or a previous drag. Only
        // this arm clears it: a press on a grip is the start of a *reorder*, and
        // that may yet want to know what was selected.
        self.body_mut().clear_selection();
        // Tone-capable emoji: arm a long-press for the skin-tone strip. Short
        // click still places the caret; text-drag is suppressed while armed.
        if let Some((range, g)) = {
            let row = &self.body().rows[i];
            row.tone_emoji_at(lx, ly).map(|(r, s)| (r, s.to_owned()))
        } {
            let cell = self
                .emoji_glyph_rect(i, range.start, range.end)
                .unwrap_or(Rect::new(x, y, 20.0, 20.0));
            let node = self.body().rows[i].id;
            let row = &mut self.body_mut().rows[i];
            row.selection =
                Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                    Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
                );
            self.view_mut().body_tone_strip = None;
            self.view_mut().body_tone_press = Some(crate::view_state::BodyTonePress {
                node,
                range,
                expected: g,
                since: Instant::now(),
                origin: (x, y),
                cell,
            });
            self.view_mut().dragging = false;
            return true;
        }
        let row = &mut self.body_mut().rows[i];
        // Title-gap hits arrive with a negative local y (the gap sits above the
        // row box). Clamp so from_point lands on the first line rather than
        // inventing a caret above the layout.
        let ly = ly.clamp(0.0, row.height.max(1.0) - 0.5);
        row.selection = Selection::from_point(&row.layout, lx, ly);
        row.snap_selection_to_graphemes();
        self.view_mut().body_tone_press = None;
        self.view_mut().dragging = true;
        self.view_mut().drag_scroll_tick = None;
        self.view_mut().scroll_anim = None;
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] editor press hit=caret active={}",
            self.view().active
        ));
        true
    }

    /// Window-space rect covering the grapheme `start..end` on row `i`.
    fn emoji_glyph_rect(&self, i: usize, start: usize, end: usize) -> Option<Rect> {
        let row = self.body().rows.get(i)?;
        let ox = self.view().content_x + row.text_x();
        let oy = row.y - self.view().scroll;
        let sel = Selection::from_byte_index(&row.layout, start, Affinity::Downstream).extend(
            Cursor::from_byte_index(&row.layout, end, Affinity::Upstream),
        );
        let mut out: Option<Rect> = None;
        sel.geometry_with(&row.layout, |bb, _| {
            let r = Rect::new(
                bb.x0 as f32 + ox,
                bb.y0 as f32 + oy,
                (bb.x1 - bb.x0) as f32,
                (bb.y1 - bb.y0) as f32,
            );
            out = Some(match out {
                Some(prev) => Rect::new(
                    prev.x.min(r.x),
                    prev.y.min(r.y),
                    prev.x.max(r.x + r.w) - prev.x.min(r.x),
                    prev.y.max(r.y + r.h) - prev.y.min(r.y),
                ),
                None => r,
            });
        });
        out
    }

    /// The document node a pill points at, if it points at one.
    fn document_behind(&self, ws: &WorkspaceDoc, row: usize, attr: AttrId) -> Option<NodeId> {
        let node = ws.read(self.body().rows.get(row)?.id).ok()?;
        let attr = node
            .content
            .attrs()
            .iter()
            .find(|candidate| candidate.id == attr)?;
        let target = attr.as_ref_target()?;
        // Before Store-per-document, a document reference needed no route:
        // every document lived in this LoroDoc and the target node resolved
        // locally. Those references survive migration, but their target now
        // lives in another document. The role still says exactly what the
        // reference means, and MainView resolves its stable UUID through the
        // manifest before asking Store to open it.
        if attr.role == "document" {
            return Some(target.node);
        }
        (ws.read(target.node).ok()?.kind == koru_core::Kind::Document).then_some(target.node)
    }

    fn entity_document_behind(
        &self,
        ws: &WorkspaceDoc,
        row: usize,
        attr: AttrId,
    ) -> Option<crate::trigger::DocumentTarget> {
        let node = ws.read(self.body().rows.get(row)?.id).ok()?;
        let target = node
            .content
            .attrs()
            .iter()
            .find(|a| a.id == attr)?
            .as_document_reference()?;
        Some(crate::trigger::DocumentTarget {
            document: target.document,
            node: target.node,
        })
    }

    /// The workspace file an attachment pill points at.
    fn file_behind(
        &self,
        ws: &WorkspaceDoc,
        row: usize,
        attr: AttrId,
    ) -> Option<koru_core::DocumentId> {
        let body_row = self.body().rows.get(row)?;
        // Prefer the live attr in the document.
        if let Ok(node) = ws.read(body_row.id)
            && let Some(stored) = node
                .content
                .attrs()
                .iter()
                .find(|candidate| candidate.id == attr)
        {
            if stored.role == "attachment" {
                match &stored.value {
                    AttrValue::Ref(r) => match r.home {
                        Some(DocRef::Document(document)) => return Some(document),
                        Some(DocRef::DocumentAt { document, .. }) => return Some(document),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        // Fall back to the pill's entity key (set by pillinfo for either dialect).
        let info = body_row.pills.get(&attr)?;
        if info.kind != koru_draw::pill::PillKind::File {
            return None;
        }
        let key = info.entity.as_deref()?;
        koru_core::DocumentId::parse(crate::pillinfo::document_of(key))
    }

    fn follow_file_pill(&mut self, ws: &WorkspaceDoc, row: usize, attr: AttrId) -> bool {
        let Some(file) = self.file_behind(ws, row, attr) else {
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!(
                    "[koru:attach] editor left-click file pill unresolved row={row} attr={attr}"
                );
            }
            return false;
        };
        // Permanently gone: broken icon + strikethrough; click does nothing.
        // Trashed: Restore is menu-only — open would just fail.
        let dead = self
            .body()
            .rows
            .get(row)
            .and_then(|r| r.pills.get(&attr))
            .is_some_and(|info| info.dangling || info.trashed);
        if dead {
            if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
                eprintln!(
                    "[koru:attach] editor left-click ignored (trashed/missing) file={}",
                    file.to_storage()
                );
            }
            return true;
        }
        if koru_env::switch(koru_env::name::KORU_ATTACH_DEBUG) {
            eprintln!(
                "[koru:attach] editor left-click Open file={}",
                file.to_storage()
            );
        }
        self.view_mut().wants_file_action = Some((file, crate::FileAction::Open));
        true
    }

    /// The keys a *selected* pill claims for itself.
    ///
    /// `None` means "not mine" and the key map takes it as usual — which is how
    /// the arrows, Escape and Home/End still work and still let go of the pill
    /// (they all reach [`disarm`](Self::disarm)).
    ///
    /// Enter is the whole point:
    ///
    /// | | |
    /// | --- | --- |
    /// | Enter | this tab |
    /// | Ctrl+Enter | a new tab |
    /// | Shift+Enter | a new window |
    ///
    /// ...on a **document** pill. A date, contact or link pill has one thing to
    /// do — reopen the picker that made it — so it ignores the modifiers rather
    /// than pretending there are three of anything.
    ///
    /// Typing is swallowed. A pill is an object here, not a run of text you are
    /// about to overtype, and replacing a whole reference with the letter `k`
    /// because a hand slipped is not recoverable by looking at it.
    fn pill_key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let (row, attr) = self.view().selected_pill?;
        if matches!(key, Key::Named(NamedKey::Enter)) {
            // *Primary*, not Control: the chord is Cmd+Enter on macOS and
            // Ctrl+Enter everywhere else, and `koru-keys` is the one place that
            // decides which. Computed once, because an entity pill and a
            // document pill open in the same three places and two copies of this
            // is how one of them stops answering to Cmd.
            let where_ = if koru_keys::has_primary(mods) {
                OpenWhere::BackgroundTab
            } else if mods.shift_key() {
                OpenWhere::NewWindow
            } else {
                OpenWhere::SameTab
            };
            if self.follow_document_pill(ws, row, attr, where_) {
                return Some(true);
            }
            if self.follow_file_pill(ws, row, attr) {
                return Some(true);
            }
            if let Some(open) = self.open_pill_picker(ws, row, attr) {
                self.view_mut().selected_pill = None;
                self.view_mut().overlay = open;
                self.view_mut().active = true;
                self.reflow();
                return Some(true);
            }
            // A dangling reference with no picker behind it: eaten rather than
            // split into a new paragraph, which is not what Enter looked like it
            // would do here.
            return Some(false);
        }
        // Ordinary typing, swallowed. A control chord is not typing and is left
        // to the key map — Ctrl+C over a selected pill should still copy.
        let typed = text.is_some_and(|t| !t.chars().any(char::is_control));
        (typed && !koru_keys::is_chord(mods)).then_some(true)
    }

    /// Re-open the picker that produced a pill, seeded with its current value.
    fn open_pill_picker(
        &mut self,
        ws: &mut WorkspaceDoc,
        row: usize,
        attr: AttrId,
    ) -> Option<Overlay> {
        let r = self.body_mut().rows.get(row)?;
        let node = r.id;
        let info = r.pills.get(&attr)?.clone();
        // The anchor's byte offset is where a replacement goes.
        let start = r
            .runs
            .iter()
            .find(|run| run.pill == Some(attr))?
            .start
            .get();

        let stored = ws.read(node).ok()?;
        let value = stored.content.attrs().iter().find(|a| a.id == attr)?;
        let date = value.as_date().map(koru_core::time::DateOnly::date);
        let url = match &value.value {
            AttrValue::Url(u) => Some(u.clone()),
            _ => None,
        };

        self.place_caret(start + ANCHOR_LEN);
        Some(trigger::open_for_pill(
            ws,
            &mut self.view_mut().engine,
            node,
            start,
            info.kind,
            &info.label,
            date,
            url.as_deref(),
        ))
    }

    /// Whether a document-space y is past the bottom of the last row, rather
    /// than in a gap between two of them.
    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        // Rows and the overlay sit in the centred content column, so the
        // pointer is mapped into it once here.
        //
        // The document's name used to be tested first, in window space, because
        // it was a label in the toolbar and double-clicking it was the only way
        // to rename. It is row 0 now, so a double click there selects a word
        // like any other — which is the whole point of making it a row.
        let cx = x - self.view().content_x;
        // An open panel owns the pointer, the same way it does for a single
        // click. Before the `active` gate, because a prompt can be open over an
        // inactive view — the rename panel is. Without this, double-clicking a
        // URL in the prompt word-selected an invisible row *underneath* the
        // panel, which then became what the next Ctrl+X acted on.
        if self.view().overlay.is_open() {
            if let Some(rect) = self.overlay_panel_rect() {
                if rect.contains(x, y) {
                    let view = self.view_mut();
                    let (engine, overlay) = (&mut view.engine, &mut view.overlay);
                    overlay.double_click(engine, x - rect.x, y - rect.y, rect.w);
                    // Swallowed even when it missed a field, which is what stops
                    // it reaching the rows — the rule `click` already follows.
                    return true;
                }
            }
        }

        if !self.view_mut().active {
            return false;
        }
        let doc_y = y + self.view_mut().scroll;
        let Some(i) = self.body_mut().row_at_y(doc_y) else {
            return false;
        };
        if let Some(image) = self.body().rows[i].image.as_ref() {
            let row = &self.body().rows[i];
            let (ix, iw, ih) = rowlayout::image_display_box(
                image,
                row.align,
                self.view().view_mode.effective_w(self.view().viewport.0),
            );
            if Rect::new(ix, row.y - self.view().scroll, iw, ih).contains(cx, y) {
                let id = row.id;
                return self.start_image_rename(ws, id);
            }
            return false;
        }
        // A double click on a drawing opens it in the viewer — the picture's
        // counterpart to an image's rename, and the same gesture every
        // image viewer answers. On the panel beside the drawing it is two
        // selecting clicks, which the first already was.
        if self.body().rows[i].diagram_shows_picture() {
            let row = &self.body().rows[i];
            let on_drawing = row
                .diagram
                .as_ref()
                .and_then(|state| {
                    rowlayout::diagram_layout(
                        row,
                        state,
                        row.height,
                        self.view().view_mode.effective_w(self.view().viewport.0),
                    )
                    .drawing
                })
                .is_some_and(|drawing| {
                    Rect::new(drawing.x, row.y + drawing.y, drawing.w, drawing.h)
                        .contains(cx, doc_y)
                });
            if on_drawing {
                let id = row.id;
                return self.ask_open_diagram(id);
            }
            return false;
        }
        self.body_mut().focus = i;
        let row = &mut self.body_mut().rows[i];
        let lx = cx - row.text_x();
        let ly = doc_y - row.y;
        row.selection = Selection::word_from_point(&row.layout, lx, ly);
        row.snap_selection_to_graphemes();
        true
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let was = self.view_mut().pointer;
        // `pointer` stays in viewport space — chrome (toolbar, scrollbar) reads
        // it directly. `cx` is the same pointer mapped into the content column,
        // which is what the rows and the overlay panel work in.
        self.view_mut().pointer = (x, y);
        let cx = x - self.view().content_x;
        let doc_y = y + self.view_mut().scroll;

        // Body tone strip: update hover, claim the pointer so text under it
        // does not light up.
        if self.view().body_tone_strip.is_some() {
            let vp = self.view().viewport;
            let cell = self.view().body_tone_strip.as_ref().unwrap().cell;
            let n = self.view().body_tone_strip.as_ref().unwrap().tones.len();
            let next = crate::components::emoji_picker::tone_index_at_window(cell, n, vp, x, y);
            let strip = self.view_mut().body_tone_strip.as_mut().unwrap();
            let changed = strip.hover != next;
            strip.hover = next;
            let cleared = self.view_mut().hover.take().is_some();
            return changed || cleared;
        }
        // Armed body long-press: cancel if the pointer wanders (start a normal
        // text drag instead).
        if let Some(press) = self.view().body_tone_press.as_ref() {
            let dx = x - press.origin.0;
            let dy = y - press.origin.1;
            if dx * dx + dy * dy > 36.0 {
                // Convert to a text drag from the original press.
                self.view_mut().body_tone_press = None;
                self.view_mut().dragging = true;
            } else {
                return false;
            }
        }

        // An image resize follows the pointer. The new size is written to the
        // focused row's cached display and reflowed, so rows below shift live;
        // the CRDT hears nothing until the release.
        if let Some(drag) = self.view().image_resize {
            let shift = self.view().mods.shift_key();
            let (w, h) = drag.size_at(cx, y, shift);
            let focus = self.body().focus;
            if let Some(img) = self.body_mut().rows[focus].image.as_mut() {
                img.display = Some((w.round().max(1.0) as u32, h.round().max(1.0) as u32));
            }
            self.reflow();
            return true;
        }

        // A thumb drag follows the pointer wherever it goes, like the field and
        // row drags below. The thumb's top is pinned to `pointer_y - grab`, then
        // mapped back to a scroll offset — the caret is not moved, so
        // `scroll_to_caret` has nothing to fight.
        if let Some(grab) = self.view().scroll_drag {
            let content_h = rowlayout::content_height(self.body());
            if let Some(g) = self.view().scrollbar_geom(content_h) {
                let travel = (g.track_bottom - g.track_top - g.thumb_h).max(0.0);
                let new_top = (y - g.track_top - grab).clamp(0.0, travel);
                let view = self.view_mut();
                view.scroll_anim = None;
                view.scroll = if travel > 0.0 {
                    new_top / travel * g.max_scroll
                } else {
                    0.0
                };
                view.clamp_scroll(content_h);
            }
            return true;
        }

        // The emoji picker's thumb is an overlay-local scrollbar, but it has
        // the same capture semantics as the document thumb above: once grabbed
        // it follows the pointer even outside the panel's narrow track.
        if self.view().overlay.is_dragging_scrollbar() {
            let Some(rect) = self.overlay_panel_rect() else {
                return false;
            };
            self.view_mut().overlay.drag_scrollbar(y - rect.y, rect.w);
            return true;
        }

        // A drag that began in a prompt field belongs to that field, wherever
        // the pointer has since gone. Before both row branches, because the
        // panel is drawn *over* the rows — without this the pointer extends a
        // selection in whatever invisible row is underneath it.
        if self.view().drag_field {
            return self.drag_field_to(cx);
        }

        // Reordering: track which row the release would drop onto.
        if self.view_mut().drag_row.is_some() {
            // Clamped past the title: dragging a row above the document's own
            // name has no landing place, and `row_near_y` clamps to row 0.
            let to = self
                .body_mut()
                .row_near_y(doc_y)
                .max(self.body().first_body_row());
            let drag = self.view_mut().drag_row.as_mut().expect("checked above");
            let changed = drag.to != to;
            drag.to = to;
            return changed;
        }

        if self.view_mut().dragging {
            return self.drag_selection_to(cx, doc_y);
        }

        // Thumb darkens in the gutter; that is a paint change even when hover
        // (Text/…) stays the same, so the frame must still run.
        let content_h = rowlayout::content_height(self.body());
        let before = self.view().over_scrollbar_gutter(content_h, was.0, was.1);
        let after = self.view().over_scrollbar_gutter(content_h, x, y);

        self.update_hover(cx, y) || before != after
    }

    /// Carry a prompt field's selection to wherever the pointer has been
    /// dragged.
    ///
    /// Window → panel-local, through the same `overlay_panel_rect` the drawing
    /// uses. `mouse_move` is handed window coordinates and `overlay_panel_rect`
    /// returns them, so there is no scroll term here — adding one would be the
    /// bug.
    ///
    /// The y is not passed on at all: a field is one line, so a drag that
    /// wanders above or below the box keeps selecting along it.
    fn drag_field_to(&mut self, x: f32) -> bool {
        let Some(rect) = self.overlay_panel_rect() else {
            return false;
        };
        let view = self.view_mut();
        let (engine, overlay) = (&mut view.engine, &mut view.overlay);
        overlay.drag(engine, x - rect.x, rect.w);
        true
    }

    /// Carry the selection to wherever the pointer has been dragged.
    ///
    /// Two selections, and which one you get is decided by whether the drag has
    /// left the row it began in. Inside that row it is a **text** selection, by
    /// character. Once it crosses into another row it becomes a **row**
    /// selection — every row from the one pressed to the one under the pointer,
    /// the same set `Shift+↑`/`↓` builds and the same one `Ctrl+D`, indent and
    /// reorder act on.
    ///
    /// That is not a shortcut: in this model a span across rows *is* a set of
    /// whole nodes. Half of one task and half of the next is not something the
    /// document can hold, so it is not something a drag can express.
    ///
    /// Dragging back collapses it again, which is what makes overshooting by a
    /// row harmless.
    fn drag_selection_to(&mut self, x: f32, doc_y: f32) -> bool {
        let press = self.press_row();
        // Clamped rather than optional: a drag that runs off the top or the
        // bottom keeps selecting to that end instead of stopping dead in the
        // margin.
        let mut over = self.body().row_near_y(doc_y);
        // A selection that started inside a code block stays inside it: the
        // drag never reaches another row, and the text selection is clamped to
        // the block's height below.
        if self
            .body()
            .rows
            .get(press)
            .is_some_and(|r| r.kind == Kind::Code)
        {
            over = press;
        }

        if over != press {
            self.body_mut().extend_selection_to(over);
            return true;
        }

        // Back where it started: one row again, and a text selection. Read
        // the stash before `clear_selection` drops it — a detour through
        // other rows collapsed this row's parley anchor, so the press point
        // survives only in `anchor_text`.
        let stash = self.body().anchor_text;
        self.body_mut().clear_selection();
        self.body_mut().focus = press;
        let Some(row) = self.body_mut().focused_mut() else {
            return true;
        };
        let lx = x - row.text_x();
        let ly = (doc_y - row.y).clamp(0.0, row.height.max(1.0) - 0.5);
        match stash {
            Some((id, anchor, _)) if id == row.id => {
                row.select_from_anchor_to_point(anchor, lx, ly);
            }
            // No detour happened (or the rows changed underneath): the live
            // selection still carries the press point.
            _ => {
                row.selection = row.selection.extend_to_point(&row.layout, lx, ly);
                row.snap_selection_to_graphemes();
            }
        }
        true
    }

    /// The row a drag began in: the anchor once one has left it, and the focus
    /// until then — since extending to another row is what *sets* the anchor.
    fn press_row(&self) -> usize {
        self.body()
            .anchor
            .and_then(|id| self.body().index_of(id))
            .unwrap_or(self.body().focus)
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.view_mut().dragging = false;
        self.view_mut().drag_scroll_tick = None;
        // Short-click on a body emoji: drop the armed long-press, leave the caret.
        if let Some(press) = self.view_mut().body_tone_press.take() {
            if self.view().body_tone_strip.is_none() {
                // Collapse selection to end of emoji so typing continues after it.
                let end = press.range.end;
                if let Some(row) = self.body_mut().rows.iter_mut().find(|r| r.id == press.node) {
                    row.place_caret(end);
                }
                self.reflow();
            }
            return true;
        }
        // An image resize commits here: the size lived in the cached row during
        // the drag, and now it is written to the document so peers see it too.
        if let Some(drag) = self.view_mut().image_resize.take() {
            if let Some(img) = self
                .body()
                .rows
                .get(self.body().focus)
                .and_then(|r| r.image.as_ref())
            {
                if let Some(size) = img.display {
                    let _ = ws.set_image_size(drag.node, Some(size));
                }
            }
            self.reflow();
            return true;
        }
        // A thumb drag ends here and goes no further: nothing was selected and
        // nothing was reordered, the scroll just lands where it was released.
        if self.view_mut().scroll_drag.take().is_some() {
            return true;
        }
        if self.view_mut().overlay.stop_scrollbar_drag() {
            return true;
        }
        // A field drag ends here and goes no further: there is no reorder to
        // complete and no document row to drop anything on.
        if std::mem::take(&mut self.view_mut().drag_field) {
            return true;
        }
        if let Some(drag) = self.view_mut().drag_row.take() {
            if drag.from != drag.to {
                self.drop_row(ws, drag.from, drag.to);
                self.reflow();
            }
            return true;
        }
        // Emoji short-click commits on release so a long-press can open the
        // tone strip instead.
        if self.view().overlay.is_open() {
            return self.overlay_release(ws);
        }
        false
    }

    /// Finish an armed emoji-cell press (or a no-op for other overlays).
    fn overlay_release(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let trigger = self.view().overlay.trigger();
        let outcome = self.view_mut().overlay.release();
        self.apply_overlay_outcome(ws, trigger, outcome);
        true
    }

    /// Promote a long-press into a tone strip. Called from draw/animating so the
    /// shell keeps waking while the hold is armed.
    fn tick_overlay_press(&mut self) -> bool {
        let picker = self.view_mut().overlay.tick_long_press();
        let body = self.tick_body_tone_press();
        picker || body
    }

    fn overlay_press_animating(&self) -> bool {
        self.view().overlay.press_animating() || self.body_tone_press_animating()
    }

    fn body_tone_press_animating(&self) -> bool {
        // Stay true for the whole arm, not only until the threshold: if
        // animating dropped the moment the hold crossed 400 ms, the frame that
        // would open the strip would never be asked for.
        self.view().body_tone_press.is_some()
    }

    /// Open the body tone strip when a hold crosses the threshold.
    fn tick_body_tone_press(&mut self) -> bool {
        let Some(press) = self.view().body_tone_press.as_ref() else {
            return false;
        };
        if self.view().body_tone_strip.is_some() {
            return false;
        }
        if press.since.elapsed()
            < std::time::Duration::from_millis(crate::components::emoji_picker::LONG_PRESS_MS)
        {
            return false;
        }
        let tones = crate::emoji::skin_tones_of(&press.expected);
        if tones.len() <= 1 {
            return false;
        }
        let press = self.view_mut().body_tone_press.take().expect("armed");
        self.view_mut().body_tone_strip = Some(crate::view_state::BodyToneStrip {
            node: press.node,
            range: press.range,
            expected: press.expected,
            tones,
            cell: press.cell,
            hover: None,
        });
        true
    }

    /// Click on (or outside) the body tone strip.
    fn body_tone_strip_click(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        let Some(strip) = self.view().body_tone_strip.clone() else {
            return false;
        };
        let vp = self.view().viewport;
        if let Some(i) = crate::components::emoji_picker::tone_index_at_window(
            strip.cell,
            strip.tones.len(),
            vp,
            x,
            y,
        ) {
            let tone = strip.tones[i];
            self.commit_body_tone(ws, &strip, tone);
            return true;
        }
        // Outside: dismiss.
        self.view_mut().body_tone_strip = None;
        true
    }

    fn commit_body_tone(
        &mut self,
        ws: &mut WorkspaceDoc,
        strip: &crate::view_state::BodyToneStrip,
        tone: &str,
    ) {
        self.view_mut().body_tone_strip = None;
        self.view_mut().body_tone_press = None;
        // Stale if the grapheme moved under us.
        let Ok(node) = ws.read(strip.node) else {
            return;
        };
        let text = node.content.as_text().unwrap_or_default();
        if strip.range.end > text.len()
            || text.get(strip.range.clone()) != Some(strip.expected.as_str())
        {
            return;
        }
        ws.checkpoint();
        let caret = match ws.replace_text(strip.node, strip.range.clone(), tone) {
            Ok(c) => c,
            Err(_) => return,
        };
        crate::emoji::remember_tone_from_value(tone);
        crate::emoji::record_used(tone);
        self.body_mut().focus_on(strip.node);
        self.body_mut().refresh(ws, strip.node);
        self.place_caret(caret);
        self.view_mut().desired_x = None;
        self.reflow();
    }

    fn apply_overlay_outcome(
        &mut self,
        ws: &mut WorkspaceDoc,
        trigger: Option<crate::trigger::Trigger>,
        outcome: Outcome,
    ) {
        // The mouse path's half of the same diversion `key` makes: a
        // move-mode commit names a destination, not a pill.
        let outcome = match self.route_move_outcome(outcome) {
            Ok(()) => return,
            Err(outcome) => outcome,
        };
        match outcome {
            Outcome::Become(cmd) => {
                if let Some(t) = trigger {
                    self.choose_command(ws, Some(t), cmd);
                }
            }
            // The mouse path's half of the typed-argument commit: a click on
            // the row the argument kept visible.
            Outcome::CommandArg(cmd, arg) => {
                if let Some(t) = trigger {
                    self.choose_command_arg(ws, Some(t), cmd, &arg);
                }
            }
            Outcome::BecomePageIcon(node) => {
                let resume = self.take_name_prompt();
                self.open_page_icon_picker(node, resume);
            }
            Outcome::Commit(spec) => {
                if let Some(t) = trigger {
                    self.commit_pill(ws, t, spec);
                    self.reflow();
                    self.scroll_to_caret();
                }
            }
            Outcome::NewPage(title) => {
                if title.is_empty() {
                    if let Some(trigger) = trigger {
                        self.commit_unicode(ws, trigger, "");
                    } else {
                        self.view_mut().overlay = Overlay::None;
                    }
                } else {
                    self.view_mut().pending_new_page_trigger = trigger;
                    self.view_mut().overlay = Overlay::None;
                }
                self.view_mut().wants_new_page = Some(title);
            }
            Outcome::SetLanguage(node, lang) => {
                self.view_mut().overlay = Overlay::None;
                let _ = ws.set_language(node, lang.as_deref());
                self.body_mut().refresh(ws, node);
                self.reflow();
            }
            outcome @ (Outcome::SetAttr(..)
            | Outcome::ClearAttr(..)
            | Outcome::AddTag(..)
            | Outcome::RemoveTag(..)
            | Outcome::CreateTag(..)
            | Outcome::JoinList(..)
            | Outcome::CreateList(..)) => {
                self.attribute_outcome(ws, trigger, &outcome);
            }
            Outcome::CommitText(text) => {
                if let Some(t) = trigger {
                    self.commit_unicode(ws, t, &text);
                }
                self.reflow();
                self.scroll_to_caret();
            }
            Outcome::Close => self.view_mut().overlay = Overlay::None,
            Outcome::Ignored | Outcome::Consumed => {}
        }
    }

    /// A click inside an open panel. Outside it dismisses, which is the
    /// caller's business, not this one's.
    fn overlay_click(&mut self, ws: &mut WorkspaceDoc, rect: Rect, x: f32, y: f32) -> bool {
        // Asked *before* the click, not after: `click` focuses whatever field
        // it lands in, so asking afterwards would report a press on the panel's
        // hint text as a field press.
        self.view_mut().drag_field = self.view().overlay.field_at(x - rect.x, y - rect.y, rect.w);

        // The trigger is optional: the language picker has none (it opens from a
        // hover chip), and only the pill/command paths below need it.
        let trigger = self.view().overlay.trigger();

        // Disjoint borrows: the panel is in `overlay`, the shaping contexts a
        // field needs to hit-test its text are in `engine`.
        let outcome = {
            let view = self.view_mut();
            let (engine, overlay) = (&mut view.engine, &mut view.overlay);
            overlay.click(engine, x - rect.x, y - rect.y, rect.w)
        };
        self.apply_overlay_outcome(ws, trigger, outcome);
        true
    }

    // -------------------------------------------------------- row furniture
    //
    // The checkbox and the drag grip belong to the *row*, not to the outline:
    // both are gated on `Paragraph::is_task()`, so a task list inside a document
    // gets them without this layer learning what a document is. That is why they
    // are defaults here rather than methods on `TaskOutline`.

    /// Rect of a row's checkbox, in document coordinates — `None` when the row
    /// has no checkbox to hit.
    ///
    /// Returning `None` rather than gating at each call site is what keeps
    /// drawing and hit-testing in agreement: a paragraph has no checkbox drawn
    /// *and* none to click, from the same function.
    fn checkbox_rect(&self, i: usize) -> Option<Rect> {
        let row = self.body().rows.get(i)?;
        // The arithmetic moved to `rowlayout` when the accessibility projection
        // became a third caller: a control published somewhere other than where
        // it answers is a control a screen reader cannot press.
        rowlayout::checkbox_band(row, row.y)
    }

    /// Rect of a row's drag grip, which lives in the affordance lane.
    ///
    /// `None` on the title: it is the node the body is rooted at, so there is
    /// nowhere in this body it could be dragged *to*.
    fn grip_rect(&self, i: usize) -> Option<Rect> {
        // A page you cannot edit is a page you cannot reorder. Refused here
        // rather than in the drawing, so the *hit target* goes with the glyph —
        // an invisible grip that still drags would be worse than a visible one.
        if self.view().read_only || self.body().is_title_row(i) {
            return None;
        }
        let row = self.body().rows.get(i)?;
        Some(rowlayout::gutter_affordances(row, row.y).1)
    }

    /// Rect of a row's insert `+`, left of the grip in the same lane.
    ///
    /// `None` on the title for the same reason: nothing goes above a document's
    /// own name, and "below" is what the first body row's own `+` already means.
    fn plus_rect(&self, i: usize) -> Option<Rect> {
        if self.view().read_only || !self.view().insert_button || self.body().is_title_row(i) {
            return None;
        }
        let row = self.body().rows.get(i)?;
        Some(rowlayout::gutter_affordances(row, row.y).0)
    }

    /// The hovered row's `+`, in **window** coordinates, when the pointer is on
    /// it — which is what the tooltip anchors to.
    ///
    /// `plus_rect` works in document space, like every other row geometry; the
    /// tooltip is drawn at the viewport's root, so the translation happens here
    /// rather than at the call site.
    fn hovered_plus_rect(&self) -> Option<Rect> {
        // Not while the menu it opens is up: the tooltip hangs directly under
        // the button and the menu hangs directly under that, so the two would
        // sit on top of each other explaining the same control.
        if self.view().overlay.is_open() {
            return None;
        }
        let Some(Hover::Plus(i)) = self.view().hover else {
            return None;
        };
        let r = self.plus_rect(i)?;
        Some(Rect::new(
            r.x + self.view().content_x,
            r.y - self.view().scroll,
            r.w,
            r.h,
        ))
    }

    /// Rect of the `…` beside the title, which opens the page-settings menu.
    ///
    /// It takes the title row's `+` slot — the same lane, so the title's text
    /// starts at exactly the same x as every paragraph under it. Putting it in
    /// the text column instead would indent the document's own name past its
    /// body, which reads as a mistake.
    fn title_menu_rect(&self) -> Option<Rect> {
        if self.view().read_only {
            return None;
        }
        let row = self
            .body()
            .rows
            .first()
            .filter(|_| self.body().is_title_row(0))?;
        Some(rowlayout::gutter_affordances(row, row.y).0)
    }

    /// Finish a grip drag: land `from` where `to` sits, taking `to`'s parent.
    ///
    /// Dropping onto a row at a different depth re-parents rather than just
    /// reordering, which is what makes drag-and-drop work in a tree at all. In a
    /// flat body every row shares one parent, so it degrades to a reorder.
    fn drop_row(&mut self, ws: &mut WorkspaceDoc, from: usize, to: usize) {
        // Belt and braces with the clamp in `mouse_move`: neither end of a
        // reorder may be the title. Dropping *onto* it would move a paragraph
        // before the document node inside the library.
        if self.body().is_title_row(from) || self.body().is_title_row(to) {
            return;
        }
        let body = self.body();
        let (Some(src), Some(dst)) = (body.rows.get(from), body.rows.get(to)) else {
            return;
        };
        let (src, dst) = (src.id, dst.id);
        let parent = self.body().parent_of(ws, dst);
        let Ok(mut at) = ws.sibling_index(dst) else {
            return;
        };
        // Moving down, the source vacates a slot above the target first.
        if from < to && matches!(ws.parent(src), Ok(p) if same_parent(p, parent)) {
            at = at.saturating_add(0);
        }
        // What this row would leave, asked before it moves — the one task
        // question that cannot be answered afterwards, because the ancestry
        // that answers it is what the move takes away (T-0c).
        let departing = koru_core::tasks::departing_all(ws, &[src]);
        if ws.move_node(src, parent, at).is_ok() {
            // **A drop is a structural user command, so it classifies** (SPEC
            // §5, ADR-272): dragging unfinished work under a completed task
            // reopens it, reason `UNFINISHED_CHILD_ADDED`. Told as a *move*, so
            // the activity reads `task.child.moved` rather than `.added` — the
            // classification cannot know which gesture produced it, and guessing
            // would file half the feed under the wrong event.
            //
            // After the move has landed and before the rebuild, so the rows are
            // built from the reopened state rather than the completed one. It is
            // its own undo step, as every canonical command is; `reclassify`
            // opens no group at all when nothing above `src` was completed,
            // which is every ordinary reorder.
            let _ = crate::tasks::run(&mut self.body_mut().task_activity, ws, |ws| {
                koru_core::tasks::reclassify_moved(ws, src)
            });
            // Dragging out of a task is the departure the arrival above is the
            // other half of. A drop within one task answers nothing.
            self.body_mut().note_departures(ws, &departing);
            self.body_mut().rebuild(ws);
            self.body_mut().focus_on(src);
        }
    }
}

/// Two parents naming the same place.
///
/// It was spelled out by hand because there were two roots and the doc comment
/// claimed `Parent` had no `PartialEq` — which it has derived all along, so the
/// match was already only restating it. One root now, and one derive.
/// The title a moved selection's new document defaults to: its first block's
/// text, trimmed, cut at thirty **characters** (the i18n rule — a byte limit
/// is a different length in every script; the store's own ceiling is 200), or
/// "Untitled" when the line is blank. Thirty, because this is a *suggestion*
/// shown in a narrow field — a person keeps or replaces it, and a paragraph
/// posing as a title helps neither.
///
/// A cut lands on the last **word boundary** inside the limit and says so
/// with an ellipsis: "VS Code treats a pinned tab as" reads like a sentence
/// that lost its ending, "VS Code treats a pinned tab…" reads like a title.
fn move_default_title(first_line: String) -> String {
    const MAX_CHARS: usize = 30;
    let trimmed = first_line.trim();
    if trimmed.is_empty() {
        return "Untitled".into();
    }
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(MAX_CHARS).collect();
    // Back up to the last space so no word is torn; a single unbroken run of
    // thirty characters (a URL, CJK prose) keeps the hard cut.
    let head = match cut.rfind(' ') {
        Some(at) if at > 0 => &cut[..at],
        _ => cut.as_str(),
    };
    format!("{}…", head.trim_end())
}

pub fn same_parent(a: koru_core::schema::Parent, b: koru_core::schema::Parent) -> bool {
    a == b
}

/// Whether a row may take bold / highlight / clear-format when multi-selected.
fn row_takes_marks(row: &Paragraph) -> bool {
    !row.is_title && !matches!(row.kind, Kind::Code | Kind::Table)
}

/// Whether a row may take alignment / heading when multi-selected.
fn row_takes_block_format(row: &Paragraph) -> bool {
    !row.is_title && row.kind != Kind::Table
}

/// Run `f` on every multi-selected row that accepts inline marks, as one undo
/// step. Used by formatting chords so five selected blocks all bold.
fn format_selected_rows<E: Editing + ?Sized>(
    e: &mut E,
    ws: &mut WorkspaceDoc,
    mut f: impl FnMut(&mut Paragraph, &mut WorkspaceDoc),
) {
    let ids: Vec<_> = e
        .body()
        .rows
        .iter()
        .filter(|r| r.selected && row_takes_marks(r))
        .map(|r| r.id)
        .collect();
    if ids.is_empty() {
        return;
    }
    ws.checkpoint();
    ws.begin_group();
    for id in ids {
        if let Some(row) = e.body_mut().row_mut(id) {
            f(row, ws);
        }
    }
    ws.checkpoint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::WorkspaceId;
    use koru_core::schema::Parent;

    /// An editor over `rows` flat tasks, laid out in a window.
    ///
    /// Laid out because every one of these tests works in *pixels*: a drag is
    /// only a drag if the rows have a y and a height to be dragged across.
    fn editor_over(rows: &[&str]) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        for text in rows {
            ws.insert(Parent::Root, koru_core::node::Node::task(*text).unwrap())
                .unwrap();
        }
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        (ws, e)
    }

    /// The vertical middle of a row, in window space.
    fn middle_of(e: &Editor, row: usize) -> f32 {
        let r = &e.body.rows[row];
        r.y + r.height / 2.0 - e.view.scroll
    }

    fn selected(e: &Editor) -> Vec<usize> {
        e.body
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selected)
            .map(|(i, _)| i)
            .collect()
    }

    /// `@` means move-and-leave-a-trail for any selection. Inline selections
    /// used to miss the row-selection-only dispatch guard, so `@` replaced the
    /// chosen words and opened an empty mention picker rather than pre-filling
    /// the destination search.
    #[test]
    fn at_over_inline_selection_opens_a_prefilled_move_picker() {
        let (mut ws, mut e) = editor_over(&["Very early tasks and calendar"]);
        e.view.active = true;
        e.place_selection_pair((0, 10));
        // A placed selection is pending until a reflow applies it, the same
        // as every other selection test here.
        e.reflow();

        assert!(e.key(
            &mut ws,
            &Key::Character("@".into()),
            Some("@"),
            ModifiersState::empty(),
        ));

        let Overlay::Entity(picker) = &e.view.overlay else {
            panic!("@ over a text selection opens the document picker");
        };
        assert!(picker.move_mode);
        assert_eq!(
            picker.own_query.as_ref().map(|field| field.raw_text()),
            Some("Very early"),
            "the selected text becomes the destination search"
        );
        assert!(e.view.pending_selection_move.is_some());
        assert_eq!(
            e.body.focused().map(|row| row.text.as_str()),
            Some("Very early tasks and calendar")
        );
    }

    /// An editor over a document that contains one image block, focused, so the
    /// image-only paths (delete, undo) can be exercised.
    fn editor_over_image() -> (WorkspaceDoc, Editor) {
        use koru_core::node::{BlobHash, Kind, Node};
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        ws.insert(
            Parent::Node(doc),
            Node::image(BlobHash("h".into()), "image/png", Some(1), Some(1)).unwrap(),
        )
        .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.view.images_enabled = true;
        e.set_viewport(900.0, 560.0);
        let img = e
            .body
            .rows
            .iter()
            .position(|r| r.kind == Kind::Image)
            .expect("an image row");
        e.body.focus = img;
        (ws, e)
    }

    /// Backspace on a focused image removes the block (it has no text to delete
    /// within and no neighbour to merge with), landing focus on the row above.
    #[test]
    fn backspace_on_a_focused_image_deletes_it() {
        let (mut ws, mut e) = editor_over_image();
        assert!(
            e.body
                .rows
                .iter()
                .any(|r| r.kind == koru_core::node::Kind::Image)
        );
        let changed = e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert!(changed, "Backspace on an image should delete it");
        assert!(
            !e.body
                .rows
                .iter()
                .any(|r| r.kind == koru_core::node::Kind::Image),
            "the image row was deleted"
        );
    }

    /// An editor over a document whose body is one divider, focused.
    fn editor_over_divider() -> (WorkspaceDoc, Editor) {
        use koru_core::node::Kind;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, koru_core::node::Node::document("d").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), koru_core::node::Node::divider().unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.set_viewport(900.0, 560.0);
        let divider = e
            .body
            .rows
            .iter()
            .position(|r| r.kind == Kind::Divider)
            .expect("a divider row");
        e.body.focus = divider;
        (ws, e)
    }

    /// A divider is a non-text leaf, so Backspace removes the block rather than
    /// trying to merge its (empty) text.
    #[test]
    fn backspace_on_a_focused_divider_deletes_it() {
        let (mut ws, mut e) = editor_over_divider();
        let changed = e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert!(changed, "Backspace on a divider should delete it");
        assert!(
            !e.body
                .rows
                .iter()
                .any(|r| r.kind == koru_core::node::Kind::Divider),
            "the divider row was deleted"
        );
    }

    /// Enter on a selected divider opens a fresh empty paragraph *below* it;
    /// the row that followed is untouched, and the caret lands in the new one.
    #[test]
    fn enter_on_a_divider_opens_a_paragraph_below_it() {
        use koru_core::node::Kind;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, koru_core::node::Node::document("d").unwrap())
            .unwrap();
        let div = ws
            .insert(Parent::Node(doc), koru_core::node::Node::divider().unwrap())
            .unwrap();
        ws.insert(
            Parent::Node(doc),
            koru_core::node::Node::paragraph("below").unwrap(),
        )
        .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.set_viewport(900.0, 560.0);
        let divider_row = e
            .body
            .rows
            .iter()
            .position(|r| r.kind == Kind::Divider)
            .expect("a divider row");
        e.body.focus = divider_row;

        assert!(e.act(&mut ws, Action::SplitLine));

        // The divider stayed put; its node identity survived.
        let at = e
            .body
            .rows
            .iter()
            .position(|r| r.id == div)
            .expect("the divider is still present");
        assert_eq!(at, divider_row, "the divider did not move");
        // A new empty paragraph sits directly below it, with the caret in it.
        let new = &e.body.rows[at + 1];
        assert_eq!(new.kind, Kind::Paragraph);
        assert!(
            new.text.is_empty(),
            "an empty paragraph opened below the divider"
        );
        assert_eq!(
            e.body.focus,
            at + 1,
            "the caret landed in the new paragraph"
        );
        assert_eq!(
            e.body.rows[at + 2].text,
            "below",
            "the row that followed the divider is unchanged"
        );
    }

    /// Shift+Enter on a selected divider opens an empty paragraph *above* it,
    /// pushing the rule down — the mirror of Enter's below.
    #[test]
    fn shift_enter_on_a_divider_opens_a_paragraph_above_it() {
        use koru_core::node::Kind;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, koru_core::node::Node::document("d").unwrap())
            .unwrap();
        let div = ws
            .insert(Parent::Node(doc), koru_core::node::Node::divider().unwrap())
            .unwrap();
        ws.insert(
            Parent::Node(doc),
            koru_core::node::Node::paragraph("below").unwrap(),
        )
        .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.set_viewport(900.0, 560.0);
        let divider_row = e
            .body
            .rows
            .iter()
            .position(|r| r.kind == Kind::Divider)
            .expect("a divider row");
        e.body.focus = divider_row;

        assert!(e.act(&mut ws, Action::SplitLineAbove));

        // The divider moved down one row; its node identity survived the move.
        let at = e
            .body
            .rows
            .iter()
            .position(|r| r.id == div)
            .expect("the divider is still present");
        assert!(at > 0, "the divider was pushed down");
        // An empty paragraph sits above it, and the caret is in it.
        let above = &e.body.rows[at - 1];
        assert_eq!(above.kind, Kind::Paragraph);
        assert!(
            above.text.is_empty(),
            "an empty paragraph opened above the divider"
        );
        assert_eq!(
            e.body.focus,
            at - 1,
            "the caret landed in the new paragraph"
        );
        assert_eq!(
            e.body.rows[at + 1].text,
            "below",
            "the row that followed the divider is unchanged"
        );
    }

    /// An editor over a document with one paragraph holding `text`, focused with
    /// the caret at its end.
    fn doc_with_text(text: &str, dividers: bool) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, koru_core::node::Node::document("d").unwrap())
            .unwrap();
        let p = ws
            .insert(
                Parent::Node(doc),
                koru_core::node::Node::paragraph(text).unwrap(),
            )
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.view.dividers_enabled = dividers;
        e.set_viewport(900.0, 560.0);
        let row = e.body.rows.iter().position(|r| r.id == p).expect("the row");
        e.body.focus = row;
        let _ = p;
        e.place_caret(text.len());
        // `place_caret` only queues; the caret lands when the next reflow applies
        // it, so drive one before the shorthand reads `caret()`.
        e.reflow();
        (ws, e)
    }

    /// The third dash on an otherwise-empty line becomes a divider, and the
    /// caret lands in the fresh paragraph beneath it.
    #[test]
    fn three_dashes_make_a_divider() {
        use koru_core::node::Kind;
        let (mut ws, mut e) = doc_with_text("--", true);
        e.type_text(&mut ws, "-");

        assert!(
            e.body.rows.iter().any(|r| r.kind == Kind::Divider),
            "a divider was inserted"
        );
        let focused = e.body.focused().expect("a focused row");
        assert_eq!(focused.kind, Kind::Paragraph, "caret is in a paragraph");
        assert!(focused.text.is_empty(), "the paragraph beneath is empty");
    }

    /// `--` mid-sentence is not a divider: only a line whose entire text is the
    /// two dashes converts.
    #[test]
    fn dashes_with_other_text_do_not_make_a_divider() {
        use koru_core::node::Kind;
        let (mut ws, mut e) = doc_with_text("note --", true);
        e.type_text(&mut ws, "-");
        assert!(
            !e.body.rows.iter().any(|r| r.kind == Kind::Divider),
            "a divider must not appear from prose"
        );
    }

    /// A view that does not offer dividers leaves the dashes as text.
    #[test]
    fn the_shorthand_does_nothing_in_a_view_that_does_not_offer_dividers() {
        use koru_core::node::Kind;
        let (mut ws, mut e) = doc_with_text("--", false);
        e.type_text(&mut ws, "-");
        assert!(
            !e.body.rows.iter().any(|r| r.kind == Kind::Divider),
            "no divider without dividers_enabled"
        );
    }

    #[test]
    fn greater_than_and_space_make_a_quote_and_empty_enter_leaves_it() {
        let (mut ws, mut e) = doc_with_text(">", false);
        e.view.quotes_enabled = true;

        e.type_text(&mut ws, " ");
        assert_eq!(e.body.focused().unwrap().kind, Kind::Quote);
        assert!(e.body.focused().unwrap().text.is_empty());

        e.type_text(&mut ws, "quoted words");
        e.reflow();
        assert!(e.act(&mut ws, Action::SplitLine));
        assert_eq!(e.body.focused().unwrap().kind, Kind::Quote);
        assert!(e.body.focused().unwrap().text.is_empty());

        assert!(e.act(&mut ws, Action::SplitLine));
        assert_eq!(e.body.focused().unwrap().kind, Kind::Paragraph);
        assert!(e.body.focused().unwrap().text.is_empty());
    }

    #[test]
    fn greater_than_stays_text_when_quotes_are_not_offered() {
        let (mut ws, mut e) = doc_with_text(">", false);
        e.type_text(&mut ws, " ");
        assert_eq!(e.body.focused().unwrap().kind, Kind::Paragraph);
        assert_eq!(e.body.focused().unwrap().text, "> ");
    }

    #[test]
    fn dash_and_number_markers_become_lists_only_after_space() {
        for (marker, style) in [("-", ListStyle::Bullet), ("1.", ListStyle::Numbered)] {
            let (mut ws, mut e) = doc_with_text(marker, false);
            e.view.task_lists_enabled = true;
            let id = e.body.focused_id().unwrap();
            assert_eq!(
                e.body.focused().unwrap().list_style,
                None,
                "the marker alone stays ordinary text"
            );

            e.type_text(&mut ws, " ");

            assert!(e.body.focused().unwrap().text.is_empty());
            let Parent::Node(list) = ws.parent(id).unwrap() else {
                panic!("the marker row was wrapped in a list")
            };
            assert_eq!(ws.read(list).unwrap().meta.list_style, Some(style));
        }
    }

    #[test]
    fn a_typed_marker_restyles_one_list_item_without_nesting_it() {
        let (mut ws, mut e) = doc_editor(&["one", "1.two", "three"]);
        e.view.task_lists_enabled = true;
        let ids = [e.body.rows[0].id, e.body.rows[1].id, e.body.rows[2].id];
        let Parent::Node(document) = ws.parent(ids[0]).unwrap() else {
            panic!("the rows belong to a document")
        };
        ws.wrap_in_list_block_many(&ids, ListStyle::Bullet).unwrap();
        e.body.rebuild(&ws);
        e.body.focus_on(ids[1]);
        e.place_caret(2);
        e.reflow();

        e.type_text(&mut ws, " ");

        assert_eq!(e.body.focused().unwrap().text, "two");
        let Parent::Node(numbered) = ws.parent(ids[1]).unwrap() else {
            panic!("the middle row is still a list item")
        };
        assert_eq!(
            ws.read(numbered).unwrap().meta.list_style,
            Some(ListStyle::Numbered)
        );
        assert_eq!(
            ws.parent(numbered).unwrap(),
            Parent::Node(document),
            "the numbered list is beside the bullet fragments, not nested"
        );
        let depths: Vec<_> = e.body.rows.iter().map(|row| row.depth).collect();
        assert!(
            depths.iter().all(|depth| *depth == depths[0]),
            "all three outer list fragments align: {depths:?}"
        );
    }

    #[test]
    fn prefixing_an_extracted_item_reconnects_bullet_and_numbered_lists() {
        for (style, prefixed, caret) in [
            (ListStyle::Bullet, "-two", 1),
            (ListStyle::Numbered, "1.two", 2),
        ] {
            let (mut ws, mut e) = doc_editor(&["one", prefixed, "three", "four"]);
            e.view.task_lists_enabled = true;
            let ids = [
                e.body.rows[0].id,
                e.body.rows[1].id,
                e.body.rows[2].id,
                e.body.rows[3].id,
            ];
            let Parent::Node(document) = ws.parent(ids[0]).unwrap() else {
                panic!("the rows belong to a document")
            };
            ws.wrap_in_list_block_many(&ids, style).unwrap();
            ws.extract_list_item(ids[1]).unwrap();
            e.body.rebuild(&ws);
            e.body.focus_on(ids[1]);
            e.place_caret(caret);
            e.reflow();

            e.type_text(&mut ws, " ");

            assert_eq!(e.body.focused().unwrap().text, "two");
            let children = ws.children(Parent::Node(document)).unwrap();
            assert_eq!(children.len(), 1, "the list fragments reconnected");
            assert_eq!(ws.read(children[0]).unwrap().meta.list_style, Some(style));
            assert_eq!(ws.children(Parent::Node(children[0])).unwrap(), ids);
        }
    }

    #[test]
    fn a_single_quote_rule_trims_the_line_box_above_the_text_ink() {
        let (mut ws, mut e) = doc_with_text("one quoted line", false);
        let id = e.body.focused_id().unwrap();
        ws.set_kind(id, Kind::Quote).unwrap();
        e.body.rebuild(&ws);
        e.reflow();

        let row = e.body.focused().unwrap();
        let bar = rowlayout::quote_bar_rect(row, row.y);
        let ink_top = rowlayout::quote_ink_top(row, row.y);
        let top_pad = ink_top - bar.y;
        assert!(bar.y > row.y, "the asymmetric line-box leading is trimmed");
        assert!((top_pad - 8.0).abs() < 0.01);
        assert_eq!(bar.y + bar.h, row.y + row.height);
    }

    /// **A quote is text-shaped prose, so it takes every inline mark.**
    ///
    /// Highlight included — a value mark like any other. The quote bar and the
    /// inset are presentation, not a formatting veto: `marks_refused` names
    /// exactly three places (title, code, table), so any future refusal has to
    /// name itself there rather than growing a Quote special case somewhere
    /// along the dispatch chain, where it would be invisible.
    #[test]
    fn a_quote_row_takes_marks_and_highlight() {
        use koru_core::node::Kind;
        let (mut ws, mut e) = doc_with_text("quoted words", false);
        let id = e.body.focused_id().unwrap();
        ws.set_kind(id, Kind::Quote).unwrap();
        e.body.rebuild(&ws);
        {
            let row = e.body.focused_mut().unwrap();
            row.place_selection(7, 12); // "words"
        }
        e.reflow();

        assert!(e.act(&mut ws, Action::CycleHighlight));
        let style = ws.style_in(id, 7..12).unwrap_or_default();
        assert!(
            style.highlight.is_some(),
            "highlight applied: {:?}",
            style.highlight
        );

        assert!(e.act(&mut ws, Action::ToggleMark(koru_core::Mark::Bold)));
        assert!(
            ws.marks_in(id, 7..12)
                .unwrap_or_default()
                .has(koru_core::Mark::Bold),
            "bold applied"
        );
    }

    #[test]
    fn ctrl_a_selects_the_block_then_the_full_document() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three"]);
        e.body.focus = 1;
        e.body.rows[1].place_caret(1);

        assert!(e.act(&mut ws, Action::SelectAll));
        assert_eq!(e.body.rows[1].range(), 0..3);
        assert!(
            !e.body.has_row_selection(),
            "first press stays in the block"
        );

        assert!(e.act(&mut ws, Action::SelectAll));
        assert_eq!(selected(&e), vec![0, 1, 2]);
    }

    #[test]
    fn ctrl_a_twice_selects_the_document_body_but_not_its_title() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("one").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("two").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        let one = e
            .body
            .rows
            .iter()
            .position(|row| row.text == "one")
            .expect("the authored paragraph is visible");
        e.body.focus = one;
        e.body.rows[one].place_caret(1);

        assert!(e.act(&mut ws, Action::SelectAll));
        assert_eq!(e.body.rows[one].range(), 0..3);
        assert!(e.act(&mut ws, Action::SelectAll));
        assert_eq!(
            selected(&e),
            (e.body.first_body_row()..e.body.rows.len()).collect::<Vec<_>>()
        );
        assert!(!e.body.rows[0].selected);
    }

    #[test]
    fn deleting_the_selected_document_body_lands_in_a_fresh_first_paragraph() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("one").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        let written = e
            .body
            .rows
            .iter()
            .position(|row| row.text == "one")
            .expect("the authored paragraph is visible");
        e.body.focus = written;
        e.body.rows[written].place_caret(1);

        assert!(e.act(&mut ws, Action::SelectAll));
        assert!(e.act(&mut ws, Action::SelectAll));
        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert!(e.body.is_title_row(0));
        assert_eq!(e.body.focus, e.body.first_body_row());
        assert_eq!(e.body.focused().map(|row| row.text.as_str()), Some(""));
        assert_eq!(e.body.focused().map(Paragraph::caret), Some(0));
    }

    /// Empty first body under the title: Backspace collapses onto the title.
    #[test]
    fn backspace_on_empty_first_body_lands_on_the_title() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        e.body.focus = 1;
        if let Some(row) = e.body.focused_mut() {
            row.place_caret(0);
        }
        e.reflow();
        assert!(
            e.body.empty_first_body_at_start(),
            "precondition: empty first body"
        );
        assert!(e.act(&mut ws, Action::DeleteBack { by_word: false }));
        assert_eq!(e.body.rows.len(), 1, "only the title remains");
        assert!(e.body.focus_is_title());
        assert_eq!(e.body.focused().map(|r| r.caret()), Some(4)); // "Spec"
    }

    /// A document with a title row and `body` paragraphs, the empty one
    /// focused — the setup every Delete-above-a-block test starts from.
    fn titled_doc_on_empty(body: &[&str]) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        for text in body {
            ws.insert(Parent::Node(doc), Node::paragraph(*text).unwrap())
                .unwrap();
        }
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        let empty = e
            .body
            .rows
            .iter()
            .position(|row| !row.is_title && row.text.is_empty())
            .expect("the empty paragraph is visible");
        e.body.focus = empty;
        e.body.rows[empty].place_caret(0);
        (ws, e)
    }

    /// Delete in an empty paragraph above a heading removes the *paragraph*;
    /// the heading keeps its level and its text, and the caret lands at its
    /// start. Merging instead would turn the heading into body text.
    #[test]
    fn delete_on_an_empty_paragraph_above_a_heading_keeps_the_heading() {
        let (mut ws, mut e) = titled_doc_on_empty(&["", "Testing Notes"]);
        let heading = e
            .body
            .rows
            .iter()
            .find(|row| row.text == "Testing Notes")
            .map(|row| row.id)
            .expect("the heading row is visible");
        ws.set_heading(heading, Some(Heading::H2)).unwrap();
        e.body.rebuild(&ws);

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert_eq!(e.body.rows.len(), 2, "title and heading, paragraph gone");
        let row = e.body.focused().expect("a row is focused");
        assert_eq!(row.id, heading, "the heading node survived, unchanged");
        assert_eq!(row.heading, Some(Heading::H2));
        assert_eq!(row.text, "Testing Notes");
        assert_eq!(row.caret(), 0, "the caret lands at the block's start");
    }

    /// The same above a task: the paragraph goes, the checkbox stays.
    #[test]
    fn delete_on_an_empty_paragraph_above_a_task_keeps_the_task() {
        let (mut ws, mut e) = titled_doc_on_empty(&[""]);
        let doc = e.body.root.expect("rooted at the document");
        let task = ws
            .insert(Parent::Node(doc), Node::task("ship it").unwrap())
            .unwrap();
        e.body.rebuild(&ws);

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert_eq!(e.body.rows.len(), 2, "title and task, paragraph gone");
        let row = e.body.focused().expect("a row is focused");
        assert_eq!(row.id, task, "the task node survived, unchanged");
        assert_eq!(row.kind, Kind::Task);
        assert_eq!(row.text, "ship it");
    }

    /// Above another plain paragraph the rule is the same: the empty block
    /// goes and the paragraph below keeps its node rather than being merged
    /// away.
    #[test]
    fn delete_on_an_empty_paragraph_keeps_the_paragraph_below() {
        let (mut ws, mut e) = titled_doc_on_empty(&["", "body text"]);
        let empty = e.body.focused().expect("the empty paragraph").id;
        let below = e
            .body
            .rows
            .iter()
            .find(|row| row.text == "body text")
            .map(|row| row.id)
            .expect("the paragraph below is visible");

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert_eq!(e.body.rows.len(), 2, "title and the paragraph below");
        assert!(
            e.body.rows.iter().all(|row| row.id != empty),
            "the empty paragraph is gone"
        );
        let row = e.body.focused().expect("a row is focused");
        assert_eq!(row.id, below, "the paragraph below survived, not merged");
        assert_eq!(row.text, "body text");
    }

    /// The rule is kind-agnostic on the deleted side too: an empty *heading*
    /// above a task loses the heading and keeps the task.
    #[test]
    fn delete_on_an_empty_heading_above_a_task_keeps_the_task() {
        let (mut ws, mut e) = titled_doc_on_empty(&[""]);
        let doc = e.body.root.expect("rooted at the document");
        let task = ws
            .insert(Parent::Node(doc), Node::task("ship it").unwrap())
            .unwrap();
        let empty = e.body.focused().expect("the empty row").id;
        ws.set_heading(empty, Some(Heading::H2)).unwrap();
        e.body.rebuild(&ws);
        e.body.focus_on(empty);
        e.body.rows[e.body.focus].place_caret(0);

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert!(
            e.body.rows.iter().all(|row| row.id != empty),
            "the empty heading is gone"
        );
        let row = e.body.focused().expect("a row is focused");
        assert_eq!(row.id, task, "the task node survived, unchanged");
        assert_eq!(row.kind, Kind::Task);
        assert_eq!(row.text, "ship it");
    }

    /// And mirrored: an empty *task* above a heading loses the task and
    /// keeps the heading.
    #[test]
    fn delete_on_an_empty_task_above_a_heading_keeps_the_heading() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let empty_task = ws
            .insert(Parent::Node(doc), Node::task("").unwrap())
            .unwrap();
        let heading = ws
            .insert(Parent::Node(doc), Node::paragraph("Testing Notes").unwrap())
            .unwrap();
        ws.set_heading(heading, Some(Heading::H2)).unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        e.body.focus_on(empty_task);
        e.body.rows[e.body.focus].place_caret(0);

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        assert!(
            e.body.rows.iter().all(|row| row.id != empty_task),
            "the empty task is gone"
        );
        let row = e.body.focused().expect("a row is focused");
        assert_eq!(row.id, heading, "the heading node survived, unchanged");
        assert_eq!(row.heading, Some(Heading::H2));
        assert_eq!(row.text, "Testing Notes");
    }

    /// An empty paragraph with a nested row under it is not disposable: the
    /// guard must not fire, or Delete would take the whole subtree with it.
    /// The ordinary merge runs instead, which pulls the *child* up and leaves
    /// the heading below alone.
    #[test]
    fn delete_on_an_empty_paragraph_with_children_keeps_the_subtree() {
        let (mut ws, mut e) = titled_doc_on_empty(&["", "Testing Notes"]);
        let heading = e
            .body
            .rows
            .iter()
            .find(|row| row.text == "Testing Notes")
            .map(|row| row.id)
            .expect("the heading row is visible");
        ws.set_heading(heading, Some(Heading::H2)).unwrap();
        let empty = e.body.focused().expect("the empty paragraph").id;
        ws.insert(Parent::Node(empty), Node::paragraph("nested").unwrap())
            .unwrap();
        e.body.rebuild(&ws);
        e.body.focus_on(empty);

        assert!(e.act(&mut ws, Action::DeleteForward { by_word: false }));

        let kept = e
            .body
            .rows
            .iter()
            .find(|row| row.id == empty)
            .expect("the paragraph itself was not deleted");
        assert_eq!(kept.text, "nested", "the child merged up, as before");
        let below = e
            .body
            .rows
            .iter()
            .find(|row| row.id == heading)
            .expect("the heading was not touched either");
        assert_eq!(below.heading, Some(Heading::H2));
    }

    /// Grouped image inserts (what paste does) undo/redo via Loro alone.
    /// Extra redo at head is a no-op — no compensatory-delete pollution.
    #[test]
    fn successive_image_inserts_undo_and_redo_via_loro() {
        use koru_core::node::BlobHash;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let anchor = ws
            .insert(Parent::Node(doc), Node::paragraph("p").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut ids = Vec::new();
        for label in ["a", "b", "c"] {
            // Mirror paste_images: close any open group, then one grouped step.
            ws.checkpoint();
            ws.begin_group();
            let after = ids.last().copied().unwrap_or(anchor);
            let id = ws
                .insert_after(
                    after,
                    Node::image(BlobHash(label.into()), "image/png", Some(1), Some(1)).unwrap(),
                )
                .unwrap();
            ws.checkpoint();
            ids.push(id);
        }
        assert!(ws.can_undo());
        assert_eq!(ids.len(), 3);
        assert!(ws.undo().unwrap());
        assert!(ws.read(ids[2]).is_err());
        assert!(ws.undo().unwrap());
        assert!(ws.read(ids[1]).is_err());
        assert!(ws.undo().unwrap());
        assert!(ws.read(ids[0]).is_err());
        assert!(ws.redo().unwrap());
        assert!(ws.read(ids[0]).is_ok());
        assert!(ws.redo().unwrap());
        assert!(ws.read(ids[1]).is_ok());
        assert!(ws.redo().unwrap());
        assert!(ws.read(ids[2]).is_ok());
        // At head: further redo must not delete.
        assert!(!ws.redo().unwrap());
        assert!(!ws.can_redo());
        assert!(ws.read(ids[0]).is_ok() && ws.read(ids[1]).is_ok() && ws.read(ids[2]).is_ok());
    }

    /// A new edit after undo truncates forward history (classic stack pointer).
    #[test]
    fn a_new_edit_after_undo_clears_loro_redo() {
        use koru_core::node::BlobHash;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let anchor = ws
            .insert(Parent::Node(doc), Node::paragraph("p").unwrap())
            .unwrap();
        ws.enable_undo();
        ws.checkpoint();
        ws.begin_group();
        let b = ws
            .insert_after(
                anchor,
                Node::image(BlobHash("b".into()), "image/png", Some(1), Some(1)).unwrap(),
            )
            .unwrap();
        ws.checkpoint();
        assert!(ws.undo().unwrap());
        assert!(ws.read(b).is_err());
        assert!(ws.can_redo());
        // Forward edit after undo drops redo.
        ws.checkpoint();
        ws.begin_group();
        let _ = ws
            .insert_after(anchor, Node::paragraph("new").unwrap())
            .unwrap();
        ws.checkpoint();
        assert!(!ws.can_redo(), "new edit truncates forward history");
        assert!(ws.read(b).is_err(), "undone paste stays gone");
    }

    /// Paste-batch stack: undo inverse-deletes; redo restores via undo-of-delete.
    #[test]
    fn paste_batch_undo_removes_nodes_and_redo_restores() {
        use koru_core::node::BlobHash;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let anchor = ws
            .insert(Parent::Node(doc), Node::paragraph("p").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.view.images_enabled = true;
        e.set_viewport(900.0, 560.0);

        let mut batches = Vec::new();
        for label in ["a", "b", "c"] {
            ws.checkpoint();
            ws.begin_group();
            let after = batches
                .last()
                .and_then(|b: &Vec<NodeId>| b.last().copied())
                .unwrap_or(anchor);
            let id = ws
                .insert_after(
                    after,
                    Node::image(BlobHash(label.into()), "image/png", Some(1), Some(1)).unwrap(),
                )
                .unwrap();
            ws.checkpoint();
            batches.push(vec![id]);
            e.view_mut()
                .last_image_pastes
                .push(crate::PasteBatch::inserted(vec![id]));
        }
        e.body.rebuild(&ws);
        e.view.active = true;

        assert_eq!(e.view().last_image_pastes.len(), 3);
        assert!(e.act(&mut ws, Action::Undo));
        assert!(ws.read(batches[2][0]).is_err());
        assert_eq!(e.view().last_image_redos.len(), 1);
        assert!(e.act(&mut ws, Action::Undo));
        assert!(ws.read(batches[1][0]).is_err());
        assert!(e.act(&mut ws, Action::Undo));
        assert!(ws.read(batches[0][0]).is_err());
        assert!(e.view().last_image_pastes.is_empty());
        assert_eq!(e.view().last_image_redos.len(), 3);

        assert!(e.act(&mut ws, Action::Redo));
        assert!(ws.read(batches[0][0]).is_ok());
        assert!(e.act(&mut ws, Action::Redo));
        assert!(ws.read(batches[1][0]).is_ok());
        assert!(e.act(&mut ws, Action::Redo));
        assert!(ws.read(batches[2][0]).is_ok());
        assert!(e.view().paste_redo_exhausted);
        // Extra redo must not delete (exhausted guard).
        let _ = e.act(&mut ws, Action::Redo);
        assert!(ws.read(batches[0][0]).is_ok() && ws.read(batches[2][0]).is_ok());
    }

    /// Inverse delete when create was not on Loro's stack; redo restores same id.
    #[test]
    fn paste_batch_inverse_delete_when_loro_cannot_undo() {
        use koru_core::node::BlobHash;
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let id = ws
            .insert(
                Parent::Node(doc),
                Node::image(BlobHash("x".into()), "image/png", Some(1), Some(1)).unwrap(),
            )
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.view.active = true;
        e.view_mut()
            .last_image_pastes
            .push(crate::PasteBatch::inserted(vec![id]));
        e.body.rebuild(&ws);
        assert!(ws.read(id).is_ok());
        assert!(e.act(&mut ws, Action::Undo), "inverse delete runs");
        assert!(ws.read(id).is_err(), "batch removed");
        assert_eq!(e.view().last_image_redos.len(), 1);
        assert_eq!(e.view().last_image_redos[0].inserted, vec![id]);
        assert!(e.act(&mut ws, Action::Redo));
        assert!(ws.read(id).is_ok(), "redo restores same id");
        assert_eq!(e.view().last_image_pastes.len(), 1);
        assert_eq!(e.view().last_image_pastes[0].inserted, vec![id]);
    }

    // -------------------------------------------------------------- reveal
    //
    // What the explore column needs: an image or a paragraph is a *place in a
    // document*, so clicking one comes here to land on it.

    /// A tall document, and the id of each row.
    fn tall(rows: usize) -> (WorkspaceDoc, Editor, Vec<NodeId>) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let ids: Vec<NodeId> = (0..rows)
            .map(|i| {
                ws.insert(
                    Parent::Root,
                    koru_core::node::Node::task(format!("row {i}")).unwrap(),
                )
                .unwrap()
            })
            .collect();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 400.0);
        (ws, e, ids)
    }

    /// Whether row `i` is inside the band the rows have to themselves.
    fn on_screen(e: &Editor, i: usize) -> bool {
        let r = &e.body.rows[i];
        let top = r.y - e.view.scroll;
        top >= 0.0 && top + r.height <= e.view.content_viewport_h()
    }

    #[test]
    fn revealing_a_row_focuses_it_and_places_the_caret() {
        let (_ws, mut e, ids) = tall(40);
        assert!(e.reveal(ids[20]));
        assert_eq!(e.body.focused_id(), Some(ids[20]));
        assert!(e.view.active, "a revealed row has to draw its caret");
        assert_eq!(e.body.focused().map(|r| r.caret()), Some(0));
    }

    /// A row below the fold is brought in; one already on screen is left alone,
    /// so clicking something you can already see does not make the page jump.
    #[test]
    fn revealing_scrolls_only_when_it_has_to() {
        let (_ws, mut e, ids) = tall(60);
        assert!(on_screen(&e, 1), "the fixture starts at the top");

        e.reveal(ids[1]);
        assert_eq!(e.view.scroll, 0.0, "already visible — nothing moved");

        e.reveal(ids[50]);
        assert!(e.view.scroll > 0.0, "scrolled down to it");
        assert!(on_screen(&e, 50));

        let settled = e.view.scroll;
        e.reveal(ids[50]);
        assert_eq!(e.view.scroll, settled, "and stayed put on a second ask");
    }

    /// Back up the document again — the other half of the band test.
    #[test]
    fn revealing_something_above_scrolls_back_up() {
        let (_ws, mut e, ids) = tall(60);
        e.reveal(ids[55]);
        assert!(e.view.scroll > 0.0);
        e.reveal(ids[2]);
        assert!(on_screen(&e, 2));
    }

    #[test]
    fn centered_reveal_places_a_search_match_in_the_viewport_middle() {
        let (_ws, mut e, ids) = tall(60);
        assert!(e.reveal_centered(ids[40]));

        let row = &e.body.rows[40];
        let row_center = row.y - e.view.scroll + row.height / 2.0;
        let band_center =
            (e.view.chrome_h + theme::MARGIN_TOP + e.view.viewport.1 - theme::STATUS_H - 8.0) / 2.0;
        assert!((row_center - band_center).abs() < 0.5);
    }

    #[test]
    fn search_reveal_flashes_authored_text_and_puts_the_caret_before_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws
            .insert(Parent::Root, Node::paragraph("before emoji after").unwrap())
            .unwrap();
        let version = ws.version();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 400.0);

        assert!(e.reveal_search_match(id, "emoji"));
        let row = e.body.focused().expect("matched row");
        assert_eq!(row.caret(), "before ".len());
        assert!(row.selection.is_collapsed(), "the flash is not a selection");
        let flash = e.view.search_flash.as_ref().expect("transient cue");
        assert_eq!(flash.node, id);
        assert_eq!(flash.range, "before ".len().."before emoji".len());
        assert_eq!(ws.version(), version, "landing is not a CRDT mutation");
    }

    #[test]
    fn search_flash_expires_on_one_deadline_without_animating() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws
            .insert(Parent::Root, Node::paragraph("find me").unwrap())
            .unwrap();
        let mut e = Editor::over(&ws);
        assert!(e.reveal_search_match(id, "find"));
        let deadline = e.search_flash_deadline().expect("deadline");

        assert!(!e.maintain_search_flash(deadline - Duration::from_millis(1)));
        assert!(e.view.search_flash.is_some());
        assert!(e.maintain_search_flash(deadline));
        assert!(e.view.search_flash.is_none());
        assert_eq!(e.search_flash_deadline(), None);
    }

    /// An image is *selected*, not edited: it has no text to put a caret in, and
    /// `build_row` leaves its layout empty — so this is also the case
    /// `scroll_to_caret` cannot handle, which is why `reveal` exists.
    #[test]
    fn revealing_an_image_focuses_it_without_a_caret() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        for i in 0..40 {
            ws.insert(
                Parent::Root,
                koru_core::node::Node::task(format!("row {i}")).unwrap(),
            )
            .unwrap();
        }
        let img = ws
            .insert(
                Parent::Root,
                koru_core::node::Node::image(
                    koru_core::node::BlobHash("deadbeef".into()),
                    "image/png",
                    Some(800),
                    Some(600),
                )
                .unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 400.0);

        assert!(e.reveal(img));
        assert_eq!(e.body.focused_id(), Some(img));
        let row = e.body.focused().expect("focused");
        assert!(row.image.is_some(), "the fixture row is an image");
        assert!(row.text.is_empty(), "and has no text to put a caret in");
        assert!(e.view.active);
    }

    /// Double-clicking the picture itself opens the image-name prompt and its
    /// commit writes metadata without replacing the blob.
    #[test]
    fn double_clicking_an_image_renames_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let img = ws
            .insert(
                Parent::Root,
                koru_core::node::Node::image(
                    koru_core::node::BlobHash("picture".into()),
                    "image/png",
                    Some(800),
                    Some(600),
                )
                .unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(img); // The first click selects the image.

        let row = e.body.focused().expect("image row");
        let (x, w, h) = rowlayout::image_display_box(
            row.image.as_ref().expect("image"),
            row.align,
            e.view.viewport.0,
        );
        let y = row.y - e.view.scroll;
        assert!(e.mouse_double_click(&ws, x + w / 2.0, y + h / 2.0));
        let Overlay::Name(prompt) = &e.view.overlay else {
            panic!("the image-name prompt did not open");
        };
        assert_eq!(prompt.trigger.anchor, Anchor::RenameImage(img));
        assert_eq!(prompt.input.raw_text(), "Image 1");
        assert!(e.overlay_panel_rect().is_some(), "the prompt is visible");

        for c in "Hero image".chars() {
            let text = c.to_string();
            e.key(
                &mut ws,
                &Key::Character(text.clone().into()),
                Some(&text),
                ModifiersState::empty(),
            );
        }
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );

        let renamed = ws.read(img).unwrap();
        assert_eq!(renamed.meta.name.as_deref(), Some("Hero image"));
        assert!(matches!(renamed.content, Content::Blob { .. }));
        assert!(!e.view.overlay.is_open());
    }

    #[test]
    fn enter_on_an_image_opens_a_paragraph_below_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let image = ws
            .insert(
                Parent::Root,
                Node::image(
                    koru_core::node::BlobHash("picture".into()),
                    "image/png",
                    Some(200),
                    Some(100),
                )
                .unwrap(),
            )
            .unwrap();
        let after = ws
            .insert(Parent::Root, Node::paragraph("after").unwrap())
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(image);

        assert!(e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));
        // A new empty paragraph opens between the image and "after"; the
        // existing block is not reused the way the old advance-path did.
        assert_eq!(e.body.rows.len(), 3, "a new paragraph is inserted");
        let next = e.body.focused().expect("new paragraph");
        assert_eq!(next.kind, koru_core::Kind::Paragraph);
        assert_eq!(next.text, "");
        assert_eq!(next.caret(), 0);
        assert_ne!(next.id, after, "a fresh node, not the row that followed");
        assert_eq!(e.body.rows.last().unwrap().id, after, "after is untouched");
    }

    #[test]
    fn enter_on_a_last_image_creates_a_paragraph_below_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let image = ws
            .insert(
                Parent::Root,
                Node::image(
                    koru_core::node::BlobHash("picture".into()),
                    "image/png",
                    Some(200),
                    Some(100),
                )
                .unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(image);

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );

        assert_eq!(e.body.rows.len(), 2);
        let next = e.body.focused().expect("new paragraph");
        assert_eq!(next.kind, koru_core::Kind::Paragraph);
        assert_eq!(next.text, "");
        assert_eq!(next.caret(), 0);
        assert!(matches!(ws.parent(next.id), Ok(Parent::Root)));
    }

    #[test]
    fn enter_on_a_selected_rendered_table_opens_a_paragraph_below_it() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let table = ws
            .insert(
                Parent::Root,
                Node::table("| hello | world |\n| --- | --- |\n| this | is cool |").unwrap(),
            )
            .unwrap();
        let after = ws
            .insert(Parent::Root, Node::paragraph("after").unwrap())
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(table);
        let table_row = e.body.focus;
        e.body.toggle_selection(table_row);

        assert!(e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));

        let next = e.body.focused().expect("new paragraph");
        assert_eq!(next.kind, Kind::Paragraph);
        assert_eq!(next.text, "");
        assert_eq!(next.caret(), 0);
        assert_eq!(e.body.rows[e.body.focus + 1].id, after);
        assert_eq!(ws.read(table).unwrap().kind, Kind::Table);
    }

    #[test]
    fn enter_in_an_active_table_cell_moves_to_the_next_cell() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let table = ws
            .insert(
                Parent::Root,
                Node::table("| hello | world |\n| --- | --- |\n| this | is cool |").unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(table);
        assert!(e.focus_table_cell(table, 0, 0));
        let rows_before = e.body.rows.len();

        assert!(e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));

        assert_eq!(e.view.table_cell, Some((table, 1, 0)));
        assert_eq!(e.body.rows.len(), rows_before, "no paragraph was inserted");
    }

    #[test]
    fn escape_returns_from_table_source_to_the_grid() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let table = ws
            .insert(
                Parent::Root,
                Node::table("| hello | world |\n| --- | --- |\n| this | is cool |").unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(table);
        assert!(e.toggle_table_view());
        assert_eq!(e.view.table_source, Some(table));

        assert!(e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            ModifiersState::empty(),
        ));
        assert_eq!(e.view.table_source, None);
    }

    #[test]
    fn ctrl_enter_opens_a_fresh_paragraph_after_a_table() {
        for source_visible in [false, true] {
            let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
            let table = ws
                .insert(
                    Parent::Root,
                    Node::table("| hello | world |\n| --- | --- |\n| this | is cool |").unwrap(),
                )
                .unwrap();
            let after = ws
                .insert(Parent::Root, Node::paragraph("after").unwrap())
                .unwrap();
            let mut e = Editor::over(&ws);
            e.set_viewport(900.0, 560.0);
            e.reveal(table);
            if source_visible {
                assert!(e.toggle_table_view());
            }

            assert!(e.key(
                &mut ws,
                &Key::Named(winit::keyboard::NamedKey::Enter),
                None,
                koru_keys::PRIMARY,
            ));

            let paragraph = e.body.focused().expect("new paragraph");
            assert_eq!(paragraph.kind, Kind::Paragraph);
            assert!(paragraph.text.is_empty());
            assert_eq!(paragraph.caret(), 0);
            assert_eq!(e.body.rows[e.body.focus - 1].id, table);
            assert_eq!(e.body.rows[e.body.focus + 1].id, after);
            assert_eq!(ws.read(table).unwrap().kind, Kind::Table);
        }
    }

    #[test]
    fn selected_table_enter_is_source_input_while_markdown_is_visible() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let table = ws
            .insert(
                Parent::Root,
                Node::table("| hello | world |\n| --- | --- |\n| this | is cool |").unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reveal(table);
        assert!(e.toggle_table_view());
        let table_row = e.body.focus;
        e.body.toggle_selection(table_row);
        let rows_before = e.body.rows.len();
        let source_before = ws
            .read(table)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_owned();

        assert!(e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        ));

        assert_eq!(e.body.rows.len(), rows_before, "no paragraph was inserted");
        assert_eq!(
            ws.read(table).unwrap().content.as_text().unwrap().len(),
            source_before.len() + 1,
            "Enter inserted a Markdown newline"
        );
    }

    /// The picture itself carries its block after the initial press selects it.
    #[test]
    fn dragging_an_image_reorders_the_whole_block() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let before = ws
            .insert(Parent::Root, Node::paragraph("before").unwrap())
            .unwrap();
        let image = ws
            .insert(
                Parent::Root,
                Node::image(
                    koru_core::node::BlobHash("picture".into()),
                    "image/png",
                    Some(200),
                    Some(100),
                )
                .unwrap(),
            )
            .unwrap();
        let after = ws
            .insert(Parent::Root, Node::paragraph("after").unwrap())
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);

        let row = &e.body.rows[1];
        let (x, w, h) = rowlayout::image_display_box(
            row.image.as_ref().expect("image"),
            row.align,
            e.view.viewport.0,
        );
        let y = row.y - e.view.scroll;
        e.mouse_down(&mut ws, x + w / 2.0, y + h / 2.0, false);
        assert_eq!(e.body.focused_id(), Some(image), "the press selects it");
        assert!(e.view.drag_row.is_some(), "the picture can carry its block");
        e.mouse_move(x + w / 2.0, middle_of(&e, 0));
        e.mouse_up(&mut ws);

        assert_eq!(
            e.body.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [image, before, after]
        );
    }

    /// The left-hand grip is another handle on the same reorder operation.
    #[test]
    fn an_images_move_grip_reorders_it_down() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let image = ws
            .insert(
                Parent::Root,
                Node::image(
                    koru_core::node::BlobHash("picture".into()),
                    "image/png",
                    Some(200),
                    Some(100),
                )
                .unwrap(),
            )
            .unwrap();
        let middle = ws
            .insert(Parent::Root, Node::paragraph("middle").unwrap())
            .unwrap();
        let last = ws
            .insert(Parent::Root, Node::paragraph("last").unwrap())
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);

        let grip = e.grip_rect(0).expect("a body row has a grip");
        e.mouse_down(
            &mut ws,
            grip.centre().0,
            grip.centre().1 - e.view.scroll,
            false,
        );
        assert!(e.view.drag_row.is_some());
        e.mouse_move(grip.centre().0, middle_of(&e, 2));
        e.mouse_up(&mut ws);

        assert_eq!(
            e.body.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [middle, last, image]
        );
    }

    /// A picture taller than the window is aligned to its **top** — pinning its
    /// bottom edge would show the last few pixels of it and nothing else.
    #[test]
    fn a_row_taller_than_the_band_is_aligned_to_its_top() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        for i in 0..20 {
            ws.insert(
                Parent::Root,
                koru_core::node::Node::task(format!("row {i}")).unwrap(),
            )
            .unwrap();
        }
        // Far taller than it is wide, so its laid-out height exceeds the band.
        let img = ws
            .insert(
                Parent::Root,
                koru_core::node::Node::image(
                    koru_core::node::BlobHash("tall".into()),
                    "image/png",
                    Some(400),
                    Some(4000),
                )
                .unwrap(),
            )
            .unwrap();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 400.0);
        e.reveal(img);

        let row = e.body.focused().expect("focused");
        assert!(
            row.height > e.view.content_viewport_h(),
            "the fixture is taller than the band ({} vs {})",
            row.height,
            e.view.content_viewport_h()
        );
        assert!(
            (row.y - e.view.scroll - theme::MARGIN_TOP).abs() < 1.0,
            "its top should be at the top of the band, not its bottom"
        );
    }

    /// A node from another document, or one that was deleted. Nothing moves.
    #[test]
    fn revealing_something_that_is_not_a_row_changes_nothing() {
        let (_ws, mut e, ids) = tall(30);
        e.reveal(ids[25]);
        let (scroll, focus) = (e.view.scroll, e.body.focus);

        assert!(!e.reveal(NodeId::new()));
        assert_eq!(e.view.scroll, scroll);
        assert_eq!(e.body.focus, focus);
    }

    /// Press in one row, drag into another, and both ends plus everything
    /// between them are selected — the set `Ctrl+D` and indent act on.
    #[test]
    fn dragging_across_rows_selects_whole_rows() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three", "four"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_move(200.0, middle_of(&e, 2));

        assert_eq!(selected(&e), vec![0, 1, 2]);
    }

    /// Upwards is the same selection. The anchor is where the press landed, not
    /// the lower of the two.
    #[test]
    fn dragging_upwards_selects_the_same_rows() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three", "four"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 3), false);
        e.mouse_move(200.0, middle_of(&e, 1));

        assert_eq!(selected(&e), vec![1, 2, 3]);
    }

    /// Overshooting by a row and coming back must not leave the extra one
    /// selected — which it would if the anchor moved with the pointer.
    #[test]
    fn dragging_back_into_the_first_row_collapses_the_selection() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_move(200.0, middle_of(&e, 2));
        e.mouse_move(200.0, middle_of(&e, 0));

        assert!(selected(&e).is_empty(), "back to one row");
        assert_eq!(e.body.focus, 0, "and the caret is back in it");
    }

    /// Running off the bottom selects to the end rather than stopping at
    /// whatever row the pointer last happened to be over.
    #[test]
    fn dragging_past_the_last_row_selects_to_the_end() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_move(200.0, middle_of(&e, 2) + 400.0);

        assert_eq!(selected(&e), vec![0, 1, 2]);
    }

    /// Within one row a drag is still a text selection, and no row is marked —
    /// or every click would light the row it landed in.
    #[test]
    fn dragging_inside_one_row_stays_a_text_selection() {
        let (mut ws, mut e) = editor_over(&["a reasonably long line of text"]);
        // Both x well inside the text column: the checkbox occupies the left
        // margin, and a press on it toggles the task instead of starting a drag.
        let tx = e.body.rows[0].text_x();
        e.mouse_down(&mut ws, tx + 4.0, middle_of(&e, 0), false);
        e.mouse_move(300.0, middle_of(&e, 0));

        assert!(selected(&e).is_empty());
        assert!(
            !e.body.rows[0].selection.is_collapsed(),
            "some text is selected"
        );
    }

    /// A press starts a new selection. Leaving the old one marked was visible
    /// as rows that stayed lit after clicking somewhere else entirely.
    #[test]
    fn a_press_clears_the_selection_the_last_drag_left() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_move(200.0, middle_of(&e, 2));
        e.mouse_up(&mut ws);
        assert_eq!(selected(&e), vec![0, 1, 2]);

        e.mouse_down(&mut ws, 200.0, middle_of(&e, 1), false);

        assert!(selected(&e).is_empty());
        assert_eq!(e.body.focus, 1);
    }

    /// The selection outlives the button: it is what the next `Ctrl+D` or
    /// `Tab` acts on, so releasing must not drop it.
    #[test]
    fn releasing_keeps_what_the_drag_selected() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_move(200.0, middle_of(&e, 1));
        e.mouse_up(&mut ws);

        assert_eq!(selected(&e), vec![0, 1]);
        assert_eq!(e.body.selected_ids().len(), 2);
    }

    #[test]
    fn shift_click_extends_the_block_selection_from_the_previous_click() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three", "four"]);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        e.mouse_up(&mut ws);

        e.view.mods = ModifiersState::SHIFT;
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 2), false);

        assert_eq!(selected(&e), vec![0, 1, 2]);
    }

    /// Leaving a paragraph with Shift+Down promotes to block selection and
    /// clears text ranges. Inside a multi-line paragraph, Shift+Down still
    /// extends the text selection by visual line first.
    #[test]
    fn shift_down_out_of_a_block_clears_text_selection() {
        let (mut ws, mut e) = doc_editor(&["alpha", "beta", "gamma"]);
        e.view.active = true;
        e.body.focus = 0;
        e.place_selection_pair((0, 3));
        e.reflow();
        assert!(!e.body.rows[0].selection.is_collapsed());

        // At the last visual line of a one-line row, Shift+Down leaves the block.
        e.act(&mut ws, Action::MoveDown { extend: true });

        assert_eq!(selected(&e), vec![0, 1], "promoted to block selection");
        assert!(
            e.body.rows[0].selection.is_collapsed(),
            "no text selection on the first block"
        );
        assert!(
            e.body.rows[1].selection.is_collapsed(),
            "no text selection on the second block"
        );

        e.act(&mut ws, Action::MoveDown { extend: true });
        assert_eq!(selected(&e), vec![0, 1, 2]);
    }

    #[test]
    fn shift_click_in_the_same_paragraph_extends_text_selection() {
        let (mut ws, mut e) = editor_over(&["a reasonably long paragraph"]);
        let x = e.body.rows[0].text_x();
        let y = middle_of(&e, 0);
        e.mouse_down(&mut ws, x + 2.0, y, false);
        e.mouse_up(&mut ws);

        e.view.mods = ModifiersState::SHIFT;
        e.mouse_down(&mut ws, x + 150.0, y, false);

        assert!(selected(&e).is_empty(), "this remains a text selection");
        assert!(!e.body.rows[0].selection.is_collapsed());
    }

    #[test]
    fn ctrl_click_toggles_individual_blocks_and_survives_a_rebuild() {
        let (mut ws, mut e) = editor_over(&["one", "two", "three", "four"]);
        e.view.mods = ModifiersState::CONTROL;
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), true);
        e.mouse_up(&mut ws);
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 2), true);
        e.mouse_up(&mut ws);

        assert_eq!(selected(&e), vec![0, 2]);
        e.body.rebuild(&ws);
        assert_eq!(selected(&e), vec![0, 2]);

        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), true);
        assert_eq!(selected(&e), vec![2], "a second Ctrl+click toggles off");
    }

    /// A row dragged by its grip to the bottom edge scrolls the page and the
    /// drop target follows the pointer into the rows that come into view.
    /// It stopped at the edge, so a block could not be moved below what was
    /// on screen.
    #[test]
    fn dragging_a_row_at_the_document_edge_scrolls_and_moves_the_drop_target() {
        let (mut ws, mut e) = overflowing_editor();
        let grip = e.grip_rect(1).expect("a body row has a grip");
        e.mouse_down(
            &mut ws,
            e.view.content_x + grip.x + grip.w * 0.5,
            grip.y + grip.h * 0.5 - e.view.scroll,
            false,
        );
        assert_eq!(
            e.view.drag_row.map(|d| d.from),
            Some(1),
            "the grip starts a drag"
        );
        let floor = e.view.chrome_h + e.view.content_viewport_h() - 1.0;
        e.mouse_move(200.0, floor);
        let to_before = e.view.drag_row.unwrap().to;
        assert!(e.is_scroll_animating(), "the edge keeps requesting frames");

        let now = Instant::now();
        e.advance_drag_autoscroll(now);
        e.advance_drag_autoscroll(now + Duration::from_millis(200));

        assert!(e.view.scroll > 0.0, "the page scrolled under the pointer");
        assert!(
            e.view.drag_row.unwrap().to > to_before,
            "the drop target moved down with the page"
        );
        e.mouse_up(&mut ws);
        assert!(e.view.drag_row.is_none());
        assert!(!e.is_scroll_animating(), "release stops the edge wakeups");
    }

    #[test]
    fn dragging_at_the_document_edge_scrolls_and_grows_the_selection() {
        let (mut ws, mut e) = overflowing_editor();
        e.mouse_down(&mut ws, 200.0, middle_of(&e, 0), false);
        let floor = e.view.chrome_h + e.view.content_viewport_h() - 1.0;
        e.mouse_move(200.0, floor);
        let before = selected(&e).len();
        assert!(e.is_scroll_animating(), "the edge keeps requesting frames");

        let now = Instant::now();
        e.advance_drag_autoscroll(now);
        e.advance_drag_autoscroll(now + Duration::from_millis(100));

        assert!(e.view.scroll > 0.0, "the body scrolled under the pointer");
        assert!(
            selected(&e).len() > before,
            "newly revealed blocks joined the selection"
        );
        e.mouse_up(&mut ws);
        assert!(!e.is_scroll_animating(), "release stops the edge wakeups");
    }

    // ------------------------------------------------------------- scrollbar

    /// Enough short rows that the body overflows the 900x560 window, so a thumb
    /// exists to drag. Returned by value because each test mutates its editor.
    fn overflowing_editor() -> (WorkspaceDoc, Editor) {
        let rows: Vec<&str> = (0..60).map(|_| "x").collect();
        editor_over(&rows)
    }

    /// Advance a smooth scroll to completion, the way the draw loop would over
    /// a few frames. Page-up/down animates rather than jumping, so a test that
    /// checks the *settled* position has to run the clock forward first.
    fn settle_scroll(e: &mut Editor) {
        let later = std::time::Instant::now() + std::time::Duration::from_millis(500);
        e.advance_scroll_anim(later);
    }

    /// Dragging the thumb to the bottom of the band scrolls to the end, and the
    /// drag ends on release. The thumb does not jump to the pointer because the
    /// grab offset is remembered.
    #[test]
    fn dragging_the_thumb_to_the_bottom_reaches_the_end() {
        let (mut ws, mut e) = overflowing_editor();
        let g = e
            .view
            .scrollbar_geom(rowlayout::content_height(&e.body))
            .expect("content overflows");
        assert_eq!(e.view.scroll, 0.0, "starts at the top");

        // Press in the centre of the thumb, then carry it to the band's bottom.
        let cx = g.thumb_x + g.thumb_w / 2.0;
        let cy = g.thumb_top + g.thumb_h / 2.0;
        e.mouse_down(&mut ws, cx, cy, false);
        e.mouse_move(cx, g.track_bottom);

        let max = g.max_scroll;
        assert!(
            (e.view.scroll - max).abs() < 0.5,
            "at the bottom: scroll {} ~= max {}",
            e.view.scroll,
            max
        );

        e.mouse_up(&mut ws);
        assert!(e.view.scroll_drag.is_none(), "release clears the drag");
    }

    /// A click in the band below the thumb pages down by one viewport, and a
    /// click above pages back up — the two directions a track click can mean.
    #[test]
    fn clicking_the_band_pages_up_and_down() {
        let (mut ws, mut e) = overflowing_editor();
        let g = e
            .view
            .scrollbar_geom(rowlayout::content_height(&e.body))
            .expect("content overflows");
        let cx = g.thumb_x + g.thumb_w / 2.0;
        let page = e.view.content_viewport_h();

        // Below the thumb: one page down.
        e.mouse_down(&mut ws, cx, g.track_bottom - 1.0, false);
        assert!(
            (e.view.scroll - page).abs() < 0.5,
            "paged down by one viewport"
        );

        // Above the thumb (which is now lower): one page back up.
        let g = e
            .view
            .scrollbar_geom(rowlayout::content_height(&e.body))
            .unwrap();
        e.mouse_down(&mut ws, cx, g.track_top + 1.0, false);
        assert!(e.view.scroll < 1.0, "paged back up to the top");
    }

    /// A scrollbar press never starts a row drag or places a caret: the thumb's
    /// column is claimed before the rows see the click.
    #[test]
    fn the_scrollbar_claims_its_column_before_the_rows() {
        let (mut ws, mut e) = overflowing_editor();
        let g = e
            .view
            .scrollbar_geom(rowlayout::content_height(&e.body))
            .unwrap();
        let focus_before = e.body.focus;

        e.mouse_down(&mut ws, g.thumb_x + 1.0, g.track_top + 1.0, false);

        assert_eq!(e.body.focus, focus_before, "no row was focused");
        assert!(e.view.scroll_drag.is_some(), "the thumb grabbed instead");
    }

    // ------------------------------------------------------------- page keys

    /// Page Down leaps the caret by a viewport rather than walking lines, and the
    /// row it lands on is on screen — typing picks up where the eye does.
    #[test]
    fn page_down_leaps_the_caret_and_keeps_it_on_screen() {
        let (mut ws, mut e) = overflowing_editor();
        assert_eq!(e.body.focus, 0);
        let vh = e.view.content_viewport_h();

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );
        // The scroll animates — advance it to completion before checking.
        settle_scroll(&mut e);

        assert!(
            e.body.focus > 10,
            "leapt well past the top, got focus {}",
            e.body.focus
        );
        let row = &e.body.rows[e.body.focus];
        let screen_y = row.y - e.view.scroll;
        assert!(
            screen_y >= -1.0 && screen_y <= vh,
            "caret row is on screen, screen_y = {screen_y}"
        );
    }

    /// Shift+Page Down grows the row selection by a page, anchored where the
    /// caret was — the page counterpart of Shift+Arrow.
    #[test]
    fn shift_page_down_extends_the_selection_by_a_page() {
        let (mut ws, mut e) = overflowing_editor();
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::SHIFT,
        );
        let sel = selected(&e);
        assert!(sel.len() > 10, "selection spans a page");
        assert_eq!(sel[0], 0, "anchored at the start");
    }

    /// A page past the end clamps: the caret stays on the last row and the
    /// scroll reaches its maximum.
    #[test]
    fn page_down_at_the_bottom_stays_on_the_last_row() {
        let (mut ws, mut e) = overflowing_editor();
        let last = e.body.rows.len() - 1;
        e.body.focus = last;
        e.place_caret(usize::MAX);

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );
        settle_scroll(&mut e);

        assert_eq!(e.body.focus, last, "the last page stays on the last row");
        // The caret is brought into view rather than left off-screen: the last
        // row sits inside the band (scroll_to_caret leaves its usual 8px margin,
        // so this checks visibility rather than an exact scroll value).
        let row = &e.body.rows[last];
        let vh = e.view.content_viewport_h();
        let screen_top = row.y - e.view.scroll;
        assert!(
            screen_top >= -1.0 && screen_top + row.height <= vh + 1.0,
            "last row on screen: top {screen_top}, height {}, vh {vh}",
            row.height
        );
    }

    /// With no active caret, paging pans the viewport and leaves focus alone —
    /// reading a long list should not need a line to land on.
    #[test]
    fn page_down_without_a_caret_pans_instead() {
        let (mut ws, mut e) = overflowing_editor();
        e.deactivate();
        assert!(!e.view.active);
        let focus_before = e.body.focus;
        let vh = e.view.content_viewport_h();

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );
        settle_scroll(&mut e);

        assert!(!e.view.active, "still no caret");
        assert_eq!(e.body.focus, focus_before, "focus untouched");
        assert!(
            (e.view.scroll - vh).abs() < 1.0,
            "panned one page, scroll {} ~= {}",
            e.view.scroll,
            vh
        );
    }

    /// Page Down starts a smooth scroll: immediately afterwards the scroll has
    /// not yet moved, and `is_scroll_animating` is true so the shell keeps the
    /// fast-wake loop running.
    #[test]
    fn page_down_starts_a_smooth_scroll() {
        let (mut ws, mut e) = overflowing_editor();
        let before = e.view.scroll;
        assert!(!e.is_scroll_animating(), "at rest to begin");

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );

        assert!(e.is_scroll_animating(), "the scroll animation began");
        // The scroll has not moved yet — the animation has not had a frame.
        assert_eq!(e.view.scroll, before, "still at the start position");
    }

    /// The smooth scroll progresses over time and reaches its target, then
    /// stops asking for frames.
    #[test]
    fn a_smooth_scroll_progresses_and_ends() {
        let (mut ws, mut e) = overflowing_editor();

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );

        // Capture the target after `scroll_to_caret` may have nudged it.
        let (started, target) = {
            let a = e.view.scroll_anim.as_ref().expect("animation in flight");
            (a.started, a.target)
        };

        // A fraction of the way through: the scroll has moved but not finished.
        let mid = started + std::time::Duration::from_millis(50);
        let moved = e.advance_scroll_anim(mid);
        assert!(moved, "advanced");
        assert!(
            e.view.scroll > 0.0 && e.view.scroll < target,
            "partway: scroll {} is between 0 and {}",
            e.view.scroll,
            target
        );
        assert!(e.is_scroll_animating(), "still going");

        // Well past the deadline: snapped to the target, animation cleared.
        let later = started + std::time::Duration::from_millis(500);
        e.advance_scroll_anim(later);
        assert!(!e.is_scroll_animating(), "the animation ended");
        assert!(
            (e.view.scroll - target).abs() < 1.0,
            "reached the target, scroll {} ~= {}",
            e.view.scroll,
            target
        );
    }

    /// A wheel scroll cancels a smooth scroll in flight — the user took over.
    #[test]
    fn wheel_scroll_cancels_a_smooth_scroll() {
        let (mut ws, mut e) = overflowing_editor();

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::PageDown),
            None,
            ModifiersState::empty(),
        );
        assert!(e.is_scroll_animating());

        Editing::scroll_by(
            &mut e,
            MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -3.0)),
        );
        assert!(
            !e.is_scroll_animating(),
            "the wheel cancelled the animation"
        );
    }

    // ----------------------------------------------------------- auto-link

    /// Type a run of plain characters, the way a keyboard would, settling after
    /// each so the caret is where the next press expects it.
    fn type_str(e: &mut Editor, ws: &mut WorkspaceDoc, s: &str) {
        for c in s.chars() {
            let t = c.to_string();
            e.key(
                ws,
                &Key::Character(t.as_str().into()),
                Some(&t),
                ModifiersState::empty(),
            );
        }
    }

    // -------------------------------------------------- the content-burst note
    //
    // ADR-273's editor half: a *local* gesture that changes a task's own text
    // leaves a note behind, and nothing else does. The coalescing, the
    // boundaries and the emission are the host's and are not reachable from
    // here.

    /// An editor over one row that is a task, laid out and focused.
    fn editor_over_a_task() -> (WorkspaceDoc, Editor, NodeId) {
        let (mut ws, mut e) = editor_over(&["post the form"]);
        let row = e.body.rows[0].id;
        koru_core::tasks::ensure_task(&mut ws, row, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
        e.body.rebuild(&ws);
        e.reflow();
        e.view.active = true;
        (ws, e, row)
    }

    /// **Never per keystroke, at the source.** A run of typing is one entry:
    /// what the host is handed to coalesce is a set of subjects, not a log of
    /// presses.
    #[test]
    fn typing_in_a_task_notes_it_once() {
        let (mut ws, mut e, row) = editor_over_a_task();
        type_str(&mut e, &mut ws, " today");

        assert_eq!(e.body.task_text_edits.len(), 1);
        let noted = e.body.take_task_text_edits();
        assert_eq!(noted[0].node_id(), row);
        assert_eq!(noted[0].kind(), "task", "the task spelling, not the block");
        assert!(
            e.body.take_task_text_edits().is_empty(),
            "what the host takes, it takes whole"
        );
    }

    /// A document is prose, and prose is not activity. The generic edit path
    /// calls the note on every row it touches, so this is the test that keeps
    /// the burst set from becoming a record of the whole document.
    #[test]
    fn typing_in_a_plain_row_notes_nothing() {
        let (mut ws, mut e) = editor_over(&["a note to self"]);
        e.view.active = true;
        type_str(&mut e, &mut ws, " and more");
        assert!(e.body.task_text_edits.is_empty());
    }

    /// Backspace, Enter and a paste are edits to a task's own text as much as a
    /// keypress is, and each one reaches the document through a different
    /// helper — which is exactly why the note lives on those helpers rather
    /// than at the twenty gestures above them.
    #[test]
    fn deleting_and_splitting_note_the_row_they_changed() {
        let (mut ws, mut e, row) = editor_over_a_task();
        // Both gestures need a caret inside the text: at offset 0 Backspace is
        // a merge with a row that is not there, and at the end Enter splits off
        // an empty tail without taking anything out of the row.
        let end = e.body.focused().unwrap().text.len();
        e.body.focused_mut().unwrap().place_caret(end);
        e.reflow();
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Backspace),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(e.body.take_task_text_edits().len(), 1, "backspace");

        e.body.focused_mut().unwrap().place_caret(4);
        e.reflow();
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        let noted = e.body.take_task_text_edits();
        assert_eq!(
            noted.len(),
            1,
            "a split takes text out of the row that stays"
        );
        assert_eq!(noted[0].node_id(), row);
    }

    /// **A peer's keystrokes are not this person's activity.**
    ///
    /// The note sites are the local edit paths and deliberately never
    /// `defer_pending` / `ChangeTracker`, which drain remote changes on the same
    /// path as local ones. Hooking there would have been the obvious place and
    /// would have minted `task.content.changed` — attributed to this user —
    /// out of somebody else's typing.
    #[test]
    fn a_remote_edit_notes_nothing() {
        let (mut ws, mut e, row) = editor_over_a_task();
        let mut peer = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
        peer.insert_text(row, 0, "peer ").unwrap();
        ws.import(&peer.export_all_updates().unwrap()).unwrap();

        e.adopt(&ws);
        assert_eq!(
            e.body.focused().unwrap().text,
            "peer post the form",
            "the remote edit did arrive"
        );
        assert!(
            e.body.task_text_edits.is_empty(),
            "and it left no local activity behind"
        );
    }

    /// Exercise the reminder semantic seam while the live slash-menu entry is
    /// deliberately gated until its visible confirmation host exists.
    fn arm_test_reminder(e: &mut Editor, ws: &mut WorkspaceDoc, phrase: &str) {
        let row = e.body.focused().expect("focused row");
        let trigger = Trigger {
            kind: TriggerKind::Slash,
            start: row.caret(),
            node: row.id,
            anchor: Anchor::Insert,
            stable: None,
        };
        e.arm_reminder(ws, trigger, phrase);
    }

    fn mark_test_reminder_person_authored(
        e: &mut Editor,
        identity: crate::reminders::RequestIdentity,
    ) {
        e.view
            .pending_reminder_text
            .get_mut(&identity.token)
            .unwrap()
            .origin = crate::reminders::RequestOrigin::PersonAuthored;
    }

    #[test]
    fn shortcode_completion_replaces_the_query_as_one_undo_step() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, ":rocket");
        assert!(matches!(e.view.overlay, Overlay::Emoji(_)));

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(e.body.focused().unwrap().text, "🚀");

        e.act(&mut ws, Action::Undo);
        e.reflow();
        assert_eq!(e.body.focused().unwrap().text, ":rocket");
    }

    #[test]
    fn emoji_picker_scrollbar_drag_is_captured_by_the_overlay() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        assert!(e.act(&mut ws, Action::OpenEmojiPicker));
        let panel = e.overlay_panel_rect().unwrap();
        let thumb = match &e.view.overlay {
            Overlay::Emoji(picker) => crate::components::emoji_picker::scrollbar_thumb(picker),
            _ => panic!("emoji picker"),
        };
        let (tx, ty) = thumb.centre();
        assert!(e.mouse_down(&mut ws, panel.x + tx, panel.y + ty, false));
        assert!(e.mouse_move(panel.x + tx, panel.y + panel.h + 100.0));
        let first = match &e.view.overlay {
            Overlay::Emoji(picker) => picker.first,
            _ => panic!("emoji picker stayed open"),
        };
        assert!(first > 0, "dragging the thumb reached later emoji");
        assert!(e.mouse_up(&mut ws));
    }

    #[test]
    fn document_rename_icon_button_opens_picker_and_writes_metadata() {
        let (mut ws, mut e) = editor_over_image();
        let page = e.body.root.unwrap();
        ws.set_icon(page, Some("🚀")).unwrap();
        assert!(e.start_rename(&ws));
        if let Overlay::Name(prompt) = &mut e.view.overlay {
            prompt.input.set_text(&mut e.view.engine, "Renamed later");
        }
        let prompt = match &e.view.overlay {
            Overlay::Name(prompt) => prompt,
            _ => panic!("rename prompt"),
        };
        assert_eq!(prompt.page_icon.as_deref(), Some("🚀"));

        let panel = e.overlay_panel_rect().unwrap();
        let (ix, iy) = crate::components::name_prompt::icon_rect().centre();
        assert!(e.overlay_click(&mut ws, panel, panel.x + ix, panel.y + iy));
        assert!(matches!(
            e.view.overlay.trigger().map(|trigger| trigger.anchor),
            Some(Anchor::PageIcon(node)) if node == page
        ));
        if let Overlay::Emoji(picker) = &e.view.overlay {
            assert!(
                !picker.hits.is_empty(),
                "page icon picker must offer an emoji before Enter"
            );
            assert!(
                picker.resume_name.is_some(),
                "rename prompt must be retained"
            );
        }

        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(ws.read(page).unwrap().meta.icon.is_some());
        let prompt = match &e.view.overlay {
            Overlay::Name(prompt) => prompt,
            _ => panic!("choosing an icon returns to rename"),
        };
        assert_eq!(prompt.input.value(), "Renamed later");
        assert_eq!(
            prompt.page_icon.as_deref(),
            ws.read(page).unwrap().meta.icon.as_deref()
        );
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            ws.read(page).unwrap().content.as_text(),
            Some("Renamed later")
        );
    }

    /// A press in the chrome band no longer opens a rename prompt, because
    /// there is no title label there to press: the document's name is row 0 of
    /// its own body, and `start_rename` (F2) is what still reaches the prompt —
    /// which exists only for the page-icon button it carries.
    #[test]
    fn f2_opens_rename_and_a_press_in_the_chrome_band_does_not() {
        let (mut ws, mut e) = editor_over_image();
        e.mouse_down(&mut ws, 700.0, 5.0, false);
        assert!(
            matches!(e.view.overlay, Overlay::None),
            "a press up there is an ordinary press now, not a rename"
        );
        assert!(e.start_rename(&ws));
        assert!(matches!(e.view.overlay, Overlay::Name(_)));
    }

    #[test]
    fn remote_insert_before_open_shortcode_does_not_move_its_replacement() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, ":rocket");
        let row = e.body.focused().unwrap().id;
        let mut peer = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
        peer.insert_text(row, 0, "peer ").unwrap();
        ws.import(&peer.export_all_updates().unwrap()).unwrap();

        e.adopt(&ws);
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(e.body.focused().unwrap().text, "peer 🚀");
    }

    #[test]
    fn remote_deletion_of_open_shortcode_closes_without_reinserting() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, ":rocket");
        let row = e.body.focused().unwrap().id;
        let mut peer = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
        peer.delete_text(row, 0..7).unwrap();
        ws.import(&peer.export_all_updates().unwrap()).unwrap();

        e.adopt(&ws);
        assert!(!e.view.overlay.is_open());
        assert_eq!(e.body.focused().unwrap().text, "");
    }

    /// The URL a link pill on `row` points at, if it has one.
    fn link_url(ws: &WorkspaceDoc, e: &Editor, row: usize) -> Option<String> {
        let node = ws.read(e.body.rows[row].id).ok()?;
        node.content.attrs().iter().find_map(|a| match &a.value {
            AttrValue::Url(u) => Some(u.clone()),
            _ => None,
        })
    }

    /// A scheme URL followed by a space becomes a link pill, with the space left
    /// as the gap after it.
    #[test]
    fn a_typed_https_url_becomes_a_link_on_space() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "see https://example.com");
        assert!(
            link_url(&ws, &e, 0).is_none(),
            "no link until the terminator"
        );
        type_str(&mut e, &mut ws, " ");

        assert_eq!(link_url(&ws, &e, 0).as_deref(), Some("https://example.com"),);
        let text = &e.body.rows[0].text;
        assert!(
            !text.contains("example"),
            "the URL text is replaced by the pill: {text:?}"
        );
        assert!(text.ends_with(' '), "the typed space stays as a gap");
    }

    /// A dot mid-URL must not link prematurely — the bug where `https://www.`
    /// linked before the URL was finished typing.
    #[test]
    fn a_dot_inside_a_url_does_not_link_prematurely() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "https://wwww.");
        assert!(
            link_url(&ws, &e, 0).is_none(),
            "a dot is part of a URL, not a terminator"
        );
        // Finishing the URL and spacing links the whole thing in one go.
        type_str(&mut e, &mut ws, "example.com ");
        assert_eq!(
            link_url(&ws, &e, 0).as_deref(),
            Some("https://wwww.example.com"),
        );
    }

    /// Trailing sentence punctuation is stripped off the URL, not included in it.
    #[test]
    fn trailing_sentence_punctuation_is_stripped_not_linked() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "https://example.com. ");
        assert_eq!(
            link_url(&ws, &e, 0).as_deref(),
            Some("https://example.com"),
            "the trailing period is not part of the URL"
        );
        let text = &e.body.rows[0].text;
        assert!(text.contains('.'), "the period stays as text: {text:?}");
    }

    /// A bare host or a filename has no scheme, so it is left as text.
    #[test]
    fn a_bare_host_is_not_auto_linked() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "see example.com and file.txt ");
        assert!(link_url(&ws, &e, 0).is_none(), "no scheme, no auto-link");
    }

    /// Enter links a URL ending at the caret, then splits the row.
    #[test]
    fn enter_links_a_url_at_the_caret_then_splits() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "https://example.com");
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(e.body.rows.len(), 2, "Enter split the row");
        assert_eq!(
            link_url(&ws, &e, 0).as_deref(),
            Some("https://example.com"),
            "the URL linked before the split"
        );
    }

    /// One Ctrl+Z undoes the linkification and brings the raw URL text back.
    #[test]
    fn undo_after_auto_link_restores_the_raw_url() {
        let (mut ws, mut e) = editor_over(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "https://example.com ");
        assert!(link_url(&ws, &e, 0).is_some());
        e.key(
            &mut ws,
            &Key::Character("z".into()),
            None,
            koru_keys::PRIMARY,
        );
        assert!(link_url(&ws, &e, 0).is_none(), "the link is gone");
        assert!(
            e.body.rows[0].text.contains("https://example.com"),
            "the raw URL text is back: {:?}",
            e.body.rows[0].text
        );
    }

    // ----------------------------------------------------------- code blocks

    /// A document editor over `body` paragraph lines, with code enabled and the
    /// first row focused. The twin of `editor_over` for prose.
    fn doc_editor(body: &[&str]) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        for text in body {
            ws.insert(Parent::Node(doc), Node::paragraph(*text).unwrap())
                .unwrap();
        }
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.view.code_enabled = true;
        e.set_viewport(900.0, 560.0);
        (ws, e)
    }

    /// Enter inside a code block is a newline, not a split — the one-line rule
    /// does not apply to `Kind::Code`.
    #[test]
    fn enter_in_a_code_block_inserts_a_newline() {
        let (mut ws, mut e) = doc_editor(&[""]);
        // Make the focused paragraph a code block directly, then type.
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.place_caret(0);
        type_str(&mut e, &mut ws, "ab");
        e.act(&mut ws, Action::SplitLine);
        type_str(&mut e, &mut ws, "cd");
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("ab\ncd"),
            "Enter inserted a newline, not a new node"
        );
        assert_eq!(e.body.rows.len(), 1, "no new row was created");
    }

    /// Ctrl+Enter leaves a code block: it moves into a following paragraph if
    /// there is one, otherwise creates one below. (Shift+Enter is reserved for a
    /// later change and does nothing here.)
    #[test]
    fn ctrl_enter_exits_a_code_block() {
        // No following row — a paragraph is created below.
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "fn main() {}").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(4);
        e.reflow();

        let changed = e.act(&mut ws, Action::ToggleDone);
        assert!(changed);
        assert_eq!(ws.read(id).unwrap().kind, Kind::Code, "the block stays");
        assert_eq!(e.body.rows.len(), 2, "a paragraph appeared below");
        assert_eq!(
            e.body.focused().unwrap().kind,
            Kind::Paragraph,
            "the caret moved into the new paragraph"
        );

        // A following row already exists — move into it instead of creating one.
        let (mut ws, mut e) = doc_editor(&["", "after"]);
        let code = e.body.rows[0].id;
        ws.set_kind(code, Kind::Code).unwrap();
        ws.set_text(code, "x").unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(1);
        e.reflow();
        let rows_before = e.body.rows.len();
        e.act(&mut ws, Action::ToggleDone);
        assert_eq!(e.body.rows.len(), rows_before, "no new row was created");
        assert_eq!(
            e.body.focused().unwrap().text,
            "after",
            "the caret moved into the existing following paragraph"
        );
    }

    // ------------------------------------------------- ticking a box by hand

    /// A checklist of `lines`, made the way a person makes one, with the first
    /// row focused and laid out.
    fn checklist(lines: &[&str]) -> (WorkspaceDoc, Editor) {
        let (mut ws, mut e) = doc_editor(lines);
        let ids: Vec<NodeId> = e.body.rows.iter().map(|row| row.id).collect();
        ws.wrap_in_list_block_many(&ids, ListStyle::Checkbox)
            .unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(0);
        e.reflow();
        assert!(
            e.body.rows[0].is_task(),
            "the fixture is only a fixture if the row draws a box"
        );
        (ws, e)
    }

    /// Where a press on row `i`'s checkbox lands, in the window coordinates
    /// [`Editor::mouse_down`] takes.
    ///
    /// Straight out of `checkbox_rect`, which is what `mouse_down` itself
    /// hit-tests against — so this cannot drift into pressing somewhere the
    /// box is not.
    fn on_the_box(e: &Editor, i: usize) -> (f32, f32) {
        let r = e.checkbox_rect(i).expect("the row draws a box");
        (
            e.view.content_x + r.x + r.w / 2.0,
            r.y + r.h / 2.0 - e.view.scroll,
        )
    }

    /// **The chord and the press are one call, and it is `Body::set_done`.**
    ///
    /// This is the test [Q-T2] asks for. Before it there was exactly one
    /// production caller of `set_done` from a person's own hands — the press —
    /// so a keyboard-only user could not tick a checklist item at all, which
    /// is the keyboard half of the accessibility roadmap's A3 gate. The two
    /// routes are driven here through the *real* entry points: the chord goes
    /// through `Editing::key`, so the registry, the key map and the arm all
    /// have to agree, and the press goes through `mouse_down` at the rectangle
    /// `checkbox_rect` answers with.
    ///
    /// Asserted **both ways round**, because a tick that could not be undone
    /// from the keyboard would be a worse hole than the one this closed.
    ///
    /// [Q-T2]: ../../../docs/features/accessibility/README.md
    #[test]
    fn ticking_from_the_keyboard_is_the_same_call_as_pressing_the_box() {
        let (mut ws, mut e) = checklist(&["milk", "eggs"]);
        let id = e.body.rows[0].id;
        assert!(!e.body.rows[0].done, "nothing is ticked yet");

        // The keyboard, through the chord the registry records.
        let handled = e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            koru_keys::PRIMARY_SHIFT,
        );
        assert!(handled, "the chord resolved and was performed");
        assert!(e.body.rows[0].done, "the box is ticked");
        assert!(
            ws.read(id).unwrap().meta.done,
            "and the document says so, not only the row"
        );

        // The same key again unticks it — one chord, one toggle.
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            koru_keys::PRIMARY_SHIFT,
        );
        assert!(!e.body.rows[0].done, "the same key unticked it");

        // …and the press, which is the call this chord had to reach, leaves the
        // document in the state the chord left it in the first time.
        let (x, y) = on_the_box(&e, 0);
        e.mouse_down(&mut ws, x, y, false);
        e.reflow();
        assert!(e.body.rows[0].done, "the press ticks the same box");
        assert!(ws.read(id).unwrap().meta.done);
    }

    /// **The unshifted form is untouched**, which is the collision this chord
    /// had to avoid: `Ctrl+Enter` on a checklist item opens a paragraph under
    /// it and must keep doing so, or a shortcut somebody uses every day would
    /// have been spent on one that is reached rarely.
    #[test]
    fn the_unshifted_ctrl_enter_still_opens_a_paragraph_under_a_checklist_row() {
        let (mut ws, mut e) = checklist(&["milk"]);
        let rows_before = e.body.rows.len();

        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            koru_keys::PRIMARY,
        );
        assert!(!e.body.rows[0].done, "nothing was ticked");
        assert_eq!(
            e.body.rows.len(),
            rows_before + 1,
            "a paragraph appeared below, as it always did"
        );
    }

    /// **A row with no box is refused, not surprised.**
    ///
    /// The decision Q-T2 asked for out loud: on prose the chord does nothing,
    /// changes nothing, and does not claim the key — so it can never mean two
    /// things depending on where the caret is. `false` is the same answer a
    /// press on empty space gives.
    #[test]
    fn the_tick_chord_does_nothing_at_all_on_a_row_that_draws_no_box() {
        let (mut ws, mut e) = doc_editor(&["just prose", "and more"]);
        e.body.focus = 0;
        e.reflow();
        let before = e
            .body
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>();

        let handled = e.act(&mut ws, Action::TickCheckbox);
        assert!(!handled, "the key is left unhandled rather than swallowed");
        assert_eq!(
            e.body
                .rows
                .iter()
                .map(|r| r.text.clone())
                .collect::<Vec<_>>(),
            before,
            "and the document is untouched"
        );
        assert!(e.checkbox_rect(0).is_none(), "there was no box to tick");
    }

    /// Enter past two trailing blank lines of a block that has content exits —
    /// two, not one, so a single stray Enter at the end does not eject. A fresh
    /// or content-less block never ejects. The exit trims the blank lines behind
    /// it so the block does not keep an empty tail.
    #[test]
    fn enter_past_two_blank_lines_exits_the_block() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();

        // One trailing blank line is not enough — Enter adds another line.
        ws.set_text(id, "x\n").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(2);
        e.reflow();
        e.act(&mut ws, Action::SplitLine);
        assert_eq!(e.body.rows.len(), 1, "one blank line does not exit");
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("x\n\n"));

        // Two trailing blank lines — Enter exits, and the blank lines are trimmed.
        ws.set_text(id, "x\n\n").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(3);
        e.reflow();
        e.act(&mut ws, Action::SplitLine);
        assert_eq!(e.body.rows.len(), 2, "two blank lines exited");
        assert_eq!(
            e.body.focused().unwrap().kind,
            Kind::Paragraph,
            "landed in a paragraph"
        );
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("x"),
            "the trailing blank lines were trimmed"
        );

        // A fresh empty block does not eject on Enter.
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.place_caret(0);
        e.act(&mut ws, Action::SplitLine);
        assert_eq!(e.body.rows.len(), 1, "an empty block did not eject");
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("\n"));
    }

    /// The markdown fence waits for its language: three typed backticks stay
    /// text, so the line can go on to name one. Nothing is a code block yet.
    #[test]
    fn three_typed_backticks_stay_text_until_enter_or_space() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```");
        let id = e.body.focused().unwrap().id;
        let node = ws.read(id).unwrap();
        assert_eq!(
            node.kind,
            Kind::Paragraph,
            "the third backtick converts nothing"
        );
        assert_eq!(
            node.content.as_text(),
            Some("```"),
            "the fence is still text"
        );
    }

    /// A space after a bare fence opens a plain code block, the moment `## `
    /// makes a heading: the backticks are cleared and no language is pinned.
    #[test]
    fn a_fence_and_a_space_open_a_plain_code_block() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "``` ");
        let id = e.body.focused().unwrap().id;
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        assert_eq!(
            node.content.as_text(),
            Some(""),
            "neither the fence nor the space is kept"
        );
        assert_eq!(
            node.meta.language, None,
            "no language until the chip picks one"
        );
        type_str(&mut e, &mut ws, "x");
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("x"),
            "the caret is in the block"
        );
    }

    /// A typed fence keeps the language it names, through a space or Enter.
    #[test]
    fn a_typed_fence_keeps_its_language_through_a_space_or_enter() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```rust ");
        let id = e.body.focused().unwrap().id;
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        assert_eq!(node.content.as_text(), Some(""));
        assert_eq!(node.meta.language.as_deref(), Some("rust"));

        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```python");
        e.act(&mut ws, Action::SplitLine);
        let id = e.body.focused().unwrap().id;
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        assert_eq!(node.meta.language.as_deref(), Some("python"));
        assert_eq!(
            e.body.rows.len(),
            1,
            "Enter converted the line rather than splitting it"
        );
    }

    /// ```` ```mermaid ```` and Enter is how a diagram is started by hand: an
    /// empty `mermaid` code block, the caret in its text and no picture yet,
    /// so the hint is what the block shows.
    #[test]
    fn a_typed_mermaid_fence_starts_an_empty_diagram() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```mermaid");
        e.act(&mut ws, Action::SplitLine);
        let row = e.body.focused().unwrap();
        let id = row.id;
        assert!(
            !row.diagram_shows_picture(),
            "an empty diagram is its text, not a picture"
        );
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        assert_eq!(node.meta.language.as_deref(), Some("mermaid"));
        assert_eq!(node.content.as_text(), Some(""));
        type_str(&mut e, &mut ws, "flowchart LR");
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("flowchart LR"));
    }

    /// Only a line that is the whole fence converts: backticks mid-sentence,
    /// a space with the fence selected, and a view without code blocks all
    /// type as text.
    #[test]
    fn a_fence_converts_only_as_a_whole_line_where_code_is_offered() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "see ``` ");
        let id = e.body.focused().unwrap().id;
        assert_eq!(ws.read(id).unwrap().kind, Kind::Paragraph);
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("see ``` "));

        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```");
        let id = e.body.focused().unwrap().id;
        e.place_selection_pair((0, 3));
        e.reflow();
        assert!(
            !e.body.focused().unwrap().range().is_empty(),
            "the fence is selected"
        );
        type_str(&mut e, &mut ws, " ");
        assert_eq!(
            ws.read(id).unwrap().kind,
            Kind::Paragraph,
            "a selection is replaced, not fenced"
        );

        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        e.view.code_enabled = false;
        type_str(&mut e, &mut ws, "```rust ");
        let id = e.body.focused().unwrap().id;
        assert_eq!(
            ws.read(id).unwrap().kind,
            Kind::Paragraph,
            "no code blocks here"
        );
    }

    /// One Ctrl+Z takes the code block away and gives the typed fence back.
    #[test]
    fn one_undo_brings_the_typed_fence_back() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        type_str(&mut e, &mut ws, "```rust ");
        let id = e.body.focused().unwrap().id;
        assert_eq!(ws.read(id).unwrap().kind, Kind::Code);
        e.key(
            &mut ws,
            &Key::Character("z".into()),
            None,
            koru_keys::PRIMARY,
        );
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Paragraph);
        assert_eq!(node.content.as_text(), Some("```rust"));
        assert_eq!(node.meta.language, None);
    }

    /// A pasted fence with a language converts on Enter and keeps it.
    #[test]
    fn a_pasted_fence_with_language_converts_on_enter() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        // Simulate a paste of "```rust" into the paragraph, then Enter.
        ws.set_text(id, "```rust").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(7);
        e.reflow();
        e.act(&mut ws, Action::SplitLine);
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        assert_eq!(node.content.as_text(), Some(""));
        assert_eq!(node.meta.language.as_deref(), Some("rust"));
    }

    /// Ctrl+` toggles the focused row between a paragraph and a code block.
    /// A multi-line code block cannot go back (the one-line rule refuses it).
    #[test]
    fn ctrl_backtick_toggles_a_code_block() {
        let (mut ws, mut e) = doc_editor(&["let x = 1;"]);
        let id = e.body.focused().unwrap().id;
        e.act(&mut ws, Action::CodeBlock);
        assert_eq!(ws.read(id).unwrap().kind, Kind::Code, "paragraph -> code");
        // Back to a paragraph (single line, so allowed).
        e.act(&mut ws, Action::CodeBlock);
        assert_eq!(
            ws.read(id).unwrap().kind,
            Kind::Paragraph,
            "code -> paragraph"
        );

        // A multi-line code block cannot revert — the toggle is a no-op.
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "a\nb").unwrap();
        e.body.rebuild(&ws);
        e.act(&mut ws, Action::CodeBlock);
        assert_eq!(
            ws.read(id).unwrap().kind,
            Kind::Code,
            "multi-line stays code"
        );
    }

    /// Backspace at the start of an interior line joins it with the line above
    /// by deleting the `\n`, rather than merging the node into its neighbour.
    #[test]
    fn backspace_at_an_interior_line_joins_the_lines() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "a\nb").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(2); // start of "b"
        e.reflow();
        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("ab"));
        assert_eq!(e.body.rows.len(), 1, "no merge happened");
    }

    /// Backspace at the very start of a multi-line code block is a no-op rather
    /// than a merge that would push a `\n` into the paragraph above.
    #[test]
    fn backspace_at_the_start_of_a_multiline_block_is_a_noop() {
        let (mut ws, mut e) = doc_editor(&["above", ""]);
        // Turn the second row into a multi-line code block.
        let code = e.body.rows[1].id;
        ws.set_kind(code, Kind::Code).unwrap();
        ws.set_text(code, "x\ny").unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 1;
        e.place_caret(0);

        let above_text = ws
            .read(e.body.rows[0].id)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();
        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert_eq!(
            ws.read(e.body.rows[0].id)
                .unwrap()
                .content
                .as_text()
                .unwrap(),
            above_text,
            "the paragraph above was untouched"
        );
        assert_eq!(
            ws.read(code).unwrap().content.as_text(),
            Some("x\ny"),
            "the code block was untouched"
        );
    }

    /// Backspace on an empty code block deletes the block and lands on the line
    /// above — there is nothing to merge, so the block goes away.
    #[test]
    fn backspace_on_an_empty_code_block_deletes_it() {
        let (mut ws, mut e) = doc_editor(&["above", ""]);
        let code = e.body.rows[1].id;
        ws.set_kind(code, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 1;
        e.place_caret(0);
        e.reflow();

        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert!(ws.read(code).is_err(), "the code block node was deleted");
        assert_eq!(e.body.rows.len(), 1, "only the line above remains");
        assert_eq!(
            e.body.focused().unwrap().text,
            "above",
            "caret landed on the line above"
        );

        // An empty code block that is the only row becomes an empty paragraph.
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(0);
        e.reflow();
        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert_eq!(e.body.rows.len(), 1, "a line remains");
        assert_eq!(
            e.body.focused().unwrap().kind,
            Kind::Paragraph,
            "the replacement is a paragraph, not a code block"
        );
    }

    /// Deleting a code block marks the body structure-dirty, which is the
    /// signal the container drains to refresh the explore column — so a deleted
    /// block disappears from the tree on the next frame, not when something else
    /// happens.
    #[test]
    fn deleting_a_code_block_marks_the_body_structure_dirty() {
        let (mut ws, mut e) = doc_editor(&["above", ""]);
        let code = e.body.rows[1].id;
        ws.set_kind(code, Kind::Code).unwrap();
        ws.set_text(code, "x").unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 1;
        e.place_caret(0);
        e.reflow();
        let _ = e.body_mut().take_structure_dirty(); // drain the setup.

        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert!(
            e.body_mut().take_structure_dirty(),
            "the delete marked the body structure-dirty so the explore column refreshes"
        );
    }

    /// Pasting multi-line text into a code block stays in the one node — the
    /// newlines are inserted, not turned into separate paragraphs.
    #[test]
    fn pasting_multi_line_text_into_a_code_block_stays_one_node() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "x").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(1); // end of "x"
        e.reflow();

        crate::clipboard::paste_plain_text(&mut e, &mut ws, "line1\nline2\nline3");
        assert_eq!(
            e.body.rows.len(),
            1,
            "the paste stayed in the one code block"
        );
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("xline1\nline2\nline3"),
            "the multi-line text was inserted with its newlines"
        );
    }

    /// Multi-line selection inside a code block: Shift+Down extends across the
    /// block's lines, and Backspace deletes the selected range (newlines
    /// included) without merging the block into a neighbour.
    #[test]
    fn multi_line_select_and_delete_inside_a_code_block() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "line1\nline2\nline3").unwrap();
        e.body.rebuild(&ws);
        e.place_caret(0); // start of line1
        e.reflow();

        // Extend the selection down two lines: line1 + line2 + the newline
        // before line3, i.e. bytes 0..12 ("line1\nline2\n").
        e.act(&mut ws, Action::MoveDown { extend: true });
        e.act(&mut ws, Action::MoveDown { extend: true });
        let sel = e.body.focused().unwrap().range();
        assert!(!sel.is_empty(), "the selection spans multiple lines");

        e.act(&mut ws, Action::DeleteBack { by_word: false });
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("line3"),
            "the selected lines were deleted, the block stayed one node"
        );
        assert_eq!(e.body.rows.len(), 1, "no merge happened");
    }

    /// A selection that starts inside a code block stays inside it: Shift+Down
    /// past the last line clamps to the block's end rather than spilling into
    /// the next row (and ditto for a mouse drag).
    #[test]
    fn selection_inside_a_code_block_does_not_escape() {
        let (mut ws, mut e) = doc_editor(&["", "after"]);
        let id = e.body.rows[0].id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "line1\nline2").unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(0); // start of line1
        e.reflow();

        // Shift+Down to line2 (within the block).
        e.act(&mut ws, Action::MoveDown { extend: true });
        assert_eq!(e.body.focus, 0, "still in the code block");
        // Shift+Down again — at the last line, it must NOT jump to the next row.
        e.act(&mut ws, Action::MoveDown { extend: true });
        assert_eq!(e.body.focus, 0, "selection stayed inside the code block");
        assert!(
            !e.body.rows[1].selected,
            "the following row was not pulled into a row selection"
        );
        assert!(
            !e.body.focused().unwrap().range().is_empty(),
            "the selection extended to the block's end instead"
        );
    }

    /// The language chip wraps its label with padding — its width is measured,
    /// not fixed, so "Auto" is a small chip and a longer language name is a
    /// longer one.
    #[test]
    fn the_language_chip_wraps_its_label() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "x").unwrap();
        e.body.rebuild(&ws);
        e.reflow();
        let auto_w = e.body.rows[0].lang_chip_w;
        assert!(auto_w > 0.0, "the chip width was measured");

        ws.set_language(id, Some("javascript")).unwrap();
        e.body.rebuild(&ws);
        e.reflow();
        let long_w = e.body.rows[0].lang_chip_w;
        assert!(
            long_w > auto_w,
            "a longer label makes a wider chip: {long_w} vs {auto_w}"
        );
    }

    #[test]
    fn copying_a_code_block_sets_the_shared_toast() {
        let (mut ws, mut e) = doc_editor(&["fn main() {}"]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.reflow();
        let chip = rowlayout::copy_chip_rect(
            &e.body.rows[0],
            e.view.view_mode.effective_w(e.view.viewport.0),
        );
        let x = e.view.content_x + chip.centre().0;
        let y = chip.centre().1 - e.view.scroll;
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(
            e.view.wants_toast.as_deref(),
            Some(koru_ui::CopyAffordance::TOAST)
        );
        assert_eq!(e.view.copy.glyph(), koru_draw::icons::CHECK);
    }

    /// `/code` relabels the focused paragraph in place, keeping its text.
    #[test]
    fn slash_code_relabels_a_paragraph() {
        let (mut ws, mut e) = doc_editor(&["let x = 1;"]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        // Drive through the slash menu: type `/code` and Enter.
        type_str(&mut e, &mut ws, "/code");
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        let node = ws.read(id).unwrap();
        assert_eq!(node.kind, Kind::Code);
        // The `/code` trigger text is gone; the original line's text remains.
        assert_eq!(node.content.as_text(), Some("let x = 1;"));
    }

    #[test]
    fn slash_attach_asks_the_host_for_the_focused_row() {
        let (mut ws, mut editor) = doc_editor(&[""]);
        editor.view.active = true;
        let id = editor.body.focused().unwrap().id;
        // Short prefix "attach" still finds Attachment (name is "attachment").
        type_str(&mut editor, &mut ws, "/attach");
        editor.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(editor.view.wants_attachment, Some(id));
        assert!(!editor.view.overlay.is_open());
    }

    /// **`/dictate` asks for a dictation and takes its own text back out.**
    ///
    /// The row exists so somebody who cannot remember the chord can still
    /// reach the feature, so what it leaves behind has to be what the chord
    /// would have left behind: an untouched line, the caret where it was, and
    /// one ask of the shell. A stray `/dictate` in the document would be typed
    /// over by the first word of the transcript.
    #[test]
    fn slash_dictate_asks_for_a_dictation_and_leaves_no_command_behind() {
        let (mut ws, mut e) = doc_editor(&["take a note "]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        // Typed at the end of the line, which is where somebody reaching for
        // `/dictate` mid-sentence would be -- and what makes the caret assertion
        // below say something a caret parked at 0 would not.
        let end = "take a note ".len();
        e.body.focused_mut().unwrap().place_caret(end);
        e.reflow();
        type_str(&mut e, &mut ws, "/dictate");
        e.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(e.view.wants_dictation, "the shell was never asked");
        assert!(!e.view.overlay.is_open(), "the menu stayed open");
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("take a note "),
            "the typed command was left in the document"
        );
        assert_eq!(
            e.body.focused().unwrap().caret(),
            end,
            "the caret moved away from where the words will land"
        );
    }

    #[test]
    fn slash_remind_with_an_argument_emits_a_request_and_removes_text_on_submit() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        ws.set_attribute(id, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
            .unwrap();
        e.body.refresh(&ws, id);

        arm_test_reminder(&mut e, &mut ws, "tomorrow 9am");

        let request = e.take_reminder_request().expect("semantic request");
        assert_eq!(request.identity.node, id);
        assert_eq!(request.phrase, "tomorrow 9am");
        assert_eq!(
            request.due_anchor,
            Some(koru_core::time::Temporal::Date(due))
        );
        assert_eq!(
            ws.read(id).unwrap().content.as_text(),
            Some("/remind tomorrow 9am")
        );
        assert!(!e.view.overlay.is_open());

        assert!(e.reminder_submitted(&mut ws, request.identity));
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some(""));
    }

    #[test]
    fn synthetic_reminder_entry_queues_the_exact_node_without_owning_document_text() {
        let (mut ws, mut editor) = doc_editor(&["keep this text"]);
        let node = editor.body.focused().unwrap().id;
        assert!(editor.request_reminder(&ws, node));
        let request = editor.take_reminder_request().expect("semantic request");
        assert_eq!(request.identity.node, node);
        assert!(request.phrase.is_empty());
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("keep this text")
        );

        assert!(!editor.reminder_submitted(&mut ws, request.identity));
        assert!(!editor.cancel_reminder(&mut ws, request.identity));
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("keep this text")
        );
    }

    #[test]
    fn reminder_cancel_keeps_ordinary_text_and_stale_success_cannot_remove_it() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        arm_test_reminder(&mut e, &mut ws, "");
        let request = e.take_reminder_request().unwrap();
        mark_test_reminder_person_authored(&mut e, request.identity);
        let stale = crate::reminders::RequestIdentity {
            token: crate::reminders::RequestToken::new(),
            node: request.identity.node,
        };
        assert!(!e.reminder_submitted(&mut ws, stale));
        assert!(e.cancel_reminder(&mut ws, request.identity));
        assert!(!e.reminder_submitted(&mut ws, request.identity));
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("/remind"));
    }

    #[test]
    fn reopened_flow_rejects_an_old_submission_for_the_same_node() {
        let (mut ws, mut editor) = doc_editor(&[""]);
        editor.view.active = true;
        arm_test_reminder(&mut editor, &mut ws, "tomorrow");
        let old = editor.take_reminder_request().unwrap().identity;
        assert!(editor.cancel_reminder(&mut ws, old));
        assert_eq!(ws.read(old.node).unwrap().content.as_text(), Some(""));

        // A reconstructed editor lifetime may begin its old local counter at
        // the same value, but its globally unique request token differs.
        arm_test_reminder(&mut editor, &mut ws, "Friday");
        let current = editor.take_reminder_request().unwrap().identity;
        assert_eq!(old.node, current.node);
        assert_ne!(old.token, current.token);
        assert!(!editor.reminder_submitted(&mut ws, old));
        assert_eq!(editor.view.pending_reminder_text.len(), 1);
        assert!(editor.reminder_submitted(&mut ws, current));
        assert_eq!(ws.read(current.node).unwrap().content.as_text(), Some(""));
    }

    #[test]
    fn reopened_flow_rejects_an_old_cancel_for_the_same_node() {
        let (mut ws, mut editor) = doc_editor(&[""]);
        editor.view.active = true;
        arm_test_reminder(&mut editor, &mut ws, "tomorrow");
        let old = editor.take_reminder_request().unwrap().identity;
        assert!(editor.cancel_reminder(&mut ws, old));
        assert_eq!(ws.read(old.node).unwrap().content.as_text(), Some(""));
        arm_test_reminder(&mut editor, &mut ws, "Friday");
        let current = editor.take_reminder_request().unwrap().identity;

        assert!(!editor.cancel_reminder(&mut ws, old));
        assert_eq!(
            editor
                .view
                .pending_reminder_text
                .get(&current.token)
                .map(|pending| pending.identity),
            Some(current)
        );
        assert!(editor.cancel_reminder(&mut ws, current));
        assert_eq!(ws.read(current.node).unwrap().content.as_text(), Some(""));
    }

    #[test]
    fn submit_preserves_command_text_that_changed_after_open() {
        let (mut ws, mut e) = doc_editor(&[""]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        arm_test_reminder(&mut e, &mut ws, "tomorrow");
        let request = e.take_reminder_request().unwrap();
        ws.replace_text(id, 0.."/remind tomorrow".len(), "keep this")
            .unwrap();
        e.body.refresh(&ws, id);

        assert!(!e.reminder_submitted(&mut ws, request.identity));
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("keep this"));
        assert!(e.view.pending_reminder_text.is_empty());
    }

    #[test]
    fn multiple_reminder_requests_keep_independent_text_ownership() {
        let (mut ws, mut e) = doc_editor(&["", ""]);
        e.view.active = true;
        let first = e.body.rows[0].id;
        let second = e.body.rows[1].id;
        arm_test_reminder(&mut e, &mut ws, "tomorrow");
        let request = e.take_reminder_request().expect("first request");
        assert_eq!(request.identity.node, first);

        e.body.focus_on(second);
        e.place_caret(0);
        arm_test_reminder(&mut e, &mut ws, "Friday");
        assert_eq!(
            ws.read(second).unwrap().content.as_text(),
            Some("/remind Friday")
        );
        let second_request = e.take_reminder_request().expect("second request");
        assert_eq!(second_request.identity.node, second);
        assert_ne!(request.identity.token, second_request.identity.token);
        assert_eq!(e.view.pending_reminder_text.len(), 2);
    }

    #[test]
    fn submit_deletes_the_exact_command_after_a_multibyte_prefix() {
        let (mut ws, mut e) = doc_editor(&["🙂 café "]);
        e.view.active = true;
        let id = e.body.focused().unwrap().id;
        e.body.focused_mut().unwrap().selection = parley::editing::Selection::from_byte_index(
            &e.body.rows[0].layout,
            "🙂 café ".len(),
            Affinity::Downstream,
        );
        arm_test_reminder(&mut e, &mut ws, "tomorrow");
        let request = e.take_reminder_request().unwrap();

        assert!(e.reminder_submitted(&mut ws, request.identity));
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("🙂 café "));
    }

    #[test]
    fn reminder_chips_are_host_supplied_per_reminder_state() {
        let (_ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        let reminders = std::collections::HashMap::from([(
            id,
            vec![
                crate::reminders::InlineChip {
                    reminder_id: "first".into(),
                    label: "Today, 4:00 PM".into(),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                },
                crate::reminders::InlineChip {
                    reminder_id: "second".into(),
                    label: "Tomorrow, 9:00 AM".into(),
                    paused: true,
                    snoozed: false,
                    attention: false,
                    handled: false,
                },
            ],
        )]);
        assert!(e.set_reminder_chips(reminders.clone()));
        assert_eq!(e.view.reminder_chips, reminders);
        assert!(!e.set_reminder_chips(reminders.clone()));
        let mut pause_only = reminders;
        pause_only.get_mut(&id).unwrap()[0].paused = true;
        e.body.rows[0].dirty = true;
        assert!(e.set_reminder_chips(pause_only.clone()));
        assert!(
            e.body.rows[0].dirty,
            "a pause-only repaint must not reflow document text"
        );
        assert!(!e.set_reminder_chips(pause_only));
        assert!(e.set_reminder_chips(Default::default()));
        assert!(e.view.reminder_chips.is_empty());
    }

    #[test]
    fn reminder_pills_flow_inline_and_press_requests_the_exact_reminder() {
        let (mut ws, mut e) = doc_editor(&["first", "second"]);
        let id = e.body.rows[1].id;
        assert!(e.set_reminder_chips(std::collections::HashMap::from([(
            id,
            vec![
                crate::reminders::InlineChip {
                    reminder_id: "first-reminder".into(),
                    label: "Today, 4:00 PM".into(),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                },
                crate::reminders::InlineChip {
                    reminder_id: "second-reminder".into(),
                    label: "Tomorrow, 9:00 AM".into(),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                },
            ],
        )])));
        let row = &e.body.rows[1];
        let rects = crate::draw::reminder_indicator_rects(
            row,
            &mut e.view.engine,
            &e.view.tags,
            &e.view.lists,
            &e.view.reminder_chips[&id],
            e.view.view_mode.effective_w(e.view.viewport.0),
            row.y,
        );
        assert_eq!(rects.len(), 2);
        assert!(rects[0].x >= row.text_x() + row.layout.lines().last().unwrap().metrics().advance);
        assert!(rects[1].x > rects[0].x + rects[0].w);
        let (x, y) = rects[1].centre();

        assert!(e.mouse_move(x, y));
        assert_eq!(
            e.view.hover,
            Some(Hover::Reminder {
                row: 1,
                reminder: 1
            })
        );
        assert_eq!(
            e.reminder_at(x, y),
            Some((
                id,
                crate::reminders::InlineChip {
                    reminder_id: "second-reminder".into(),
                    label: "Tomorrow, 9:00 AM".into(),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                }
            ))
        );
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(
            e.take_reminder_management_request(),
            Some((id, "second-reminder".into()))
        );
        assert_eq!(
            e.body.focused().map(|row| row.id),
            e.body.rows.first().map(|row| row.id)
        );
    }

    #[test]
    fn paused_reminder_pills_use_the_pause_icon() {
        let paused = crate::reminders::InlineChip {
            reminder_id: "paused".into(),
            label: "Tomorrow, 9:00 AM".into(),
            paused: true,
            snoozed: false,
            attention: false,
            handled: false,
        };
        let active = crate::reminders::InlineChip {
            paused: false,
            ..paused.clone()
        };
        let snoozed = crate::reminders::InlineChip {
            paused: false,
            snoozed: true,
            ..paused.clone()
        };

        assert_eq!(crate::draw::reminder_icon(&paused), koru_draw::icons::PAUSE);
        assert_eq!(crate::draw::reminder_icon(&active), koru_draw::icons::BELL);
        assert_eq!(
            crate::draw::reminder_icon(&snoozed),
            koru_draw::icons::BELL_Z
        );
    }

    #[test]
    fn task_attention_dot_only_follows_fired_unhandled_reminders() {
        let scheduled = crate::reminders::InlineChip {
            reminder_id: "scheduled".into(),
            label: "Tomorrow, 9:00 AM".into(),
            paused: false,
            snoozed: false,
            attention: false,
            handled: false,
        };
        let fired = crate::reminders::InlineChip {
            reminder_id: "fired".into(),
            attention: true,
            ..scheduled.clone()
        };

        assert!(!crate::draw::task_has_reminder_attention(Some(&vec![
            scheduled
        ])));
        assert!(crate::draw::task_has_reminder_attention(Some(&vec![fired])));
        assert!(!crate::draw::task_has_reminder_attention(None));
    }

    #[test]
    fn reminder_pills_wrap_whole_inside_every_document_mode() {
        for mode in [ViewMode::Full, ViewMode::Narrow, ViewMode::Page] {
            let (_ws, mut e) = doc_editor(&["Reminder owner", "following row"]);
            e.set_viewport(1200.0, 560.0);
            e.set_view_mode(mode);
            let id = e.body.rows[0].id;
            let before_next_y = e.body.rows[1].y;
            let before_content_h = e.view.content_h;
            let chips = (0..12)
                .map(|index| crate::reminders::InlineChip {
                    reminder_id: format!("reminder-{index}"),
                    label: "Tomorrow, 9:00 AM".into(),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                })
                .collect::<Vec<_>>();
            assert!(e.set_reminder_chips(std::collections::HashMap::from([(id, chips)])));

            let row = &e.body.rows[0];
            let layout_w = mode.effective_w(e.view.viewport.0);
            let right = rowlayout::text_right(row, layout_w);
            let rects = crate::draw::reminder_indicator_rects(
                row,
                &mut e.view.engine,
                &e.view.tags,
                &e.view.lists,
                &e.view.reminder_chips[&id],
                layout_w,
                row.y,
            );
            assert_eq!(rects.len(), 12);
            assert!(
                rects.iter().all(|rect| rect.x + rect.w <= right + 0.01),
                "{mode:?} let a reminder escape x={right}: {rects:?}"
            );
            assert!(
                rects.windows(2).any(|pair| pair[1].y > pair[0].y),
                "{mode:?} should move a whole pill to another visual line"
            );
            assert!(row.height > row.layout.height());
            assert!(e.body.rows[1].y > before_next_y);
            assert!(e.body.rows[1].y >= row.y + row.height);
            assert!(e.view.content_h > before_content_h);
            assert!((e.view.content_h - rowlayout::content_height(&e.body)).abs() < 0.01);
        }
    }

    #[test]
    fn a_right_aligned_reminder_wraps_and_keeps_the_content_right_edge() {
        let (_ws, mut e) = doc_editor(&["Right aligned"]);
        e.body.rows[0].align = Align::End;
        e.body.rows[0].dirty = true;
        e.reflow();
        let id = e.body.rows[0].id;
        assert!(e.set_reminder_chips(std::collections::HashMap::from([(
            id,
            vec![crate::reminders::InlineChip {
                reminder_id: "right".into(),
                label: "Tomorrow, 9:00 AM".into(),
                paused: false,
                snoozed: false,
                attention: false,
                handled: false,
            }],
        )])));

        let row = &e.body.rows[0];
        let layout_w = e.view.view_mode.effective_w(e.view.viewport.0);
        let right = rowlayout::text_right(row, layout_w);
        let rect = crate::draw::reminder_indicator_rects(
            row,
            &mut e.view.engine,
            &e.view.tags,
            &e.view.lists,
            &e.view.reminder_chips[&id],
            layout_w,
            row.y,
        )[0];
        let authored_bottom = row.y
            + row
                .layout
                .lines()
                .last()
                .expect("an authored line")
                .metrics()
                .block_max_coord;

        assert!((rect.x + rect.w - right).abs() < 0.01);
        assert!(rect.y >= authored_bottom);
    }

    /// Ctrl+E toggles the inline-code mark on the selection.
    #[test]
    fn inline_code_toggles_on_a_selection() {
        let (mut ws, mut e) = doc_editor(&["run cargo now"]);
        let id = e.body.focused().unwrap().id;
        // Select "cargo" (bytes 4..9).
        e.body.focused_mut().unwrap().selection = parley::editing::Selection::from_byte_index(
            &e.body.rows[0].layout,
            4,
            Affinity::Downstream,
        )
        .extend(parley::editing::Cursor::from_byte_index(
            &e.body.rows[0].layout,
            9,
            Affinity::Upstream,
        ));
        e.act(&mut ws, Action::ToggleMark(Mark::Code));
        assert!(
            ws.runs(id)
                .unwrap()
                .iter()
                .any(|r| r.code && r.text == "cargo")
        );
    }

    /// Ctrl+B with several blocks marked must bold every one of them. Clearing
    /// the row selection before the mark ran left only the focused line bold.
    #[test]
    fn bold_applies_to_every_row_in_a_block_selection() {
        let (mut ws, mut e) = doc_editor(&["alpha", "beta", "gamma"]);
        e.view.active = true;
        e.body.clear_selection();
        e.body.focus = 0;
        e.body.extend_selection_to(2);
        assert!(e.body.has_row_selection());
        assert_eq!(e.body.rows.iter().filter(|r| r.selected).count(), 3);

        e.act(&mut ws, Action::ToggleMark(Mark::Bold));

        for row in &e.body.rows {
            let marks = ws
                .marks_in(row.id, 0..row.text.len())
                .expect("marks readable");
            assert!(
                marks.bold,
                "row {:?} should be bold after multi-select Ctrl+B",
                row.text
            );
        }
        assert!(
            e.body.has_row_selection(),
            "the block selection survives so a second chord can toggle off"
        );
    }

    /// The language picker is not trigger-based, so it has to get a panel rect
    /// of its own (anchored to the chip) — otherwise it neither renders nor
    /// accepts clicks. This is the regression that made the dropdown "broken".
    #[test]
    fn the_language_picker_gets_a_panel_rect() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "x").unwrap();
        e.body.rebuild(&ws);
        e.view_mut().overlay =
            Overlay::Language(language_picker::LanguagePicker::open_for(id, None));
        assert!(
            e.overlay_panel_rect().is_some(),
            "the language panel has a rect, so it renders and accepts clicks"
        );
    }

    /// **Hovering the open dropdown reports the dropdown, not the document
    /// under it.** `update_hover` hands `hover_at` content-column x while a
    /// panel rect is viewport space; compared unmapped, `contains` missed by
    /// exactly `content_x` — most of this narrow panel — so the pointer fell
    /// through to the row underneath: an I-beam over entries, and no hover.
    /// The whole chain is driven here in window coordinates, the way the shell
    /// delivers them: chip press opens, hovering a row pre-selects it and takes
    /// the panel's own cursor, and the padding band stays panel rather than
    /// leaking.
    #[test]
    fn hovering_the_language_dropdown_lights_its_rows_not_the_document() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let id = e.body.focused().unwrap().id;
        ws.set_kind(id, Kind::Code).unwrap();
        ws.set_text(id, "x").unwrap();
        ws.set_language(id, Some("python")).unwrap();
        e.body.rebuild(&ws);
        e.reflow();

        // Press the chip like a user: its centre, in window coordinates.
        let layout_w = e.view.view_mode.effective_w(e.view.viewport.0);
        let chip = rowlayout::lang_chip_rect(&e.body.rows[0], layout_w);
        let press_x = e.view.content_x + chip.centre().0;
        let press_y = chip.centre().1 - e.view.scroll;
        assert!(
            e.mouse_down(&mut ws, press_x, press_y, false),
            "chip claimed"
        );
        let python = language_picker::labels()
            .iter()
            .position(|(n, _)| *n == "Python")
            .unwrap();
        match &e.view.overlay {
            Overlay::Language(picker) => {
                assert_eq!(picker.sel, python, "the chip's value opens active")
            }
            _ => panic!("the chip opened the language picker"),
        }

        let rect = e.overlay_panel_rect().expect("an open panel has a rect");

        // The second row ("Rust"), through window coordinates.
        let hover_y = rect.y + theme::PANEL_PAD + theme::ROW_H * 1.5;
        e.mouse_move(rect.x + 10.0, hover_y);
        assert_eq!(
            e.view.hover,
            Some(Hover::PanelRow(1)),
            "the hovered entry pre-selects"
        );
        match &e.view.overlay {
            Overlay::Language(picker) => assert_eq!(picker.sel, 1),
            _ => unreachable!(),
        }
        assert_eq!(
            e.view.cursor_icon(),
            koru_ui::cursor::PRESS,
            "entries wear the plain arrow, not the hand"
        );

        // The padding band above the first row is still the panel: a press
        // surface, not the I-beam of whatever row sits underneath.
        e.mouse_move(rect.x + 10.0, rect.y + 2.0);
        assert_eq!(e.view.hover, Some(Hover::PanelControl));
        assert_eq!(e.view.cursor_icon(), koru_ui::cursor::PRESS);
    }

    /// Ctrl+Shift+L after Ctrl+Shift+E has to bring the row back to the left.
    ///
    /// `set_align`'s "press it again to undo it" rule reads the *current*
    /// alignment, and left is the value an unaligned row already reports — so
    /// the press that means "put it back" and the press that means "toggle this
    /// off" are the same press, and only one of them can win.
    #[test]
    fn align_left_returns_a_centred_row_to_the_left() {
        use koru_core::node::Align;
        let (mut ws, mut e) = editor_over(&["hello"]);
        e.view.active = true;

        e.act(&mut ws, Action::SetAlign(Align::Center));
        assert_eq!(e.body.rows[0].align, Align::Center, "centred");

        e.act(&mut ws, Action::SetAlign(Align::Start));
        assert_eq!(e.body.rows[0].align, Align::Start, "and back to the left");
    }

    // ------------------------------------------------ arrowing onto a pill

    /// One row reading `task ⁠`, with a document pill at the end and the caret
    /// just before its anchor. Returns the pill's attr and the target document.
    fn editor_over_pill() -> (WorkspaceDoc, Editor, AttrId, NodeId) {
        use koru_core::node::Node;
        use koru_core::{Attr, AttrValue, Ref};

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let target = ws
            .insert(Parent::Root, Node::document("Brief").unwrap())
            .unwrap();
        let row = ws
            .insert(Parent::Root, Node::task("task ").unwrap())
            .unwrap();
        // The pill's target had to be created first to be referenced, and it
        // used to be invisible here because it sat in the library while the
        // rows came from the outline. One tree now, so it is a row too — and it
        // is moved below the task to keep the row under test at index 0, which
        // is what every assertion below names.
        ws.move_node(target, Parent::Root, 1).unwrap();
        let attr = ws
            .insert_pill(row, 5, &Attr::new("ref", AttrValue::Ref(Ref::new(target))))
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.view.active = true;
        e.body.focus = 0;
        e.place_caret(5);
        e.reflow();
        (ws, e, attr, target)
    }

    #[test]
    fn a_routed_document_pill_keeps_its_inner_block_target() {
        use koru_core::node::Node;
        use koru_core::{Attr, AttrValue, DocRef, DocumentId, ProfileId, Ref};

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let row = ws
            .insert(Parent::Root, Node::paragraph("").unwrap())
            .unwrap();
        let inner = NodeId::new();
        let document = DocumentId::new();
        let attr = ws
            .insert_pill(
                row,
                0,
                &Attr::labelled(
                    "document",
                    AttrValue::Ref(Ref::in_doc(
                        inner,
                        DocRef::DocumentAt {
                            profile: ProfileId::new(),
                            workspace: WorkspaceId::new(),
                            document,
                        },
                    )),
                    "Deep target",
                ),
            )
            .unwrap();
        let e = Editor::over(&ws);

        let target = e.entity_document_behind(&ws, 0, attr).unwrap();

        assert_eq!(target.document, document);
        assert_eq!(target.node, Some(inner));
    }

    #[test]
    fn a_legacy_document_pill_can_open_after_documents_move_to_separate_store_objects() {
        use koru_core::node::Node;
        use koru_core::{Attr, AttrValue, Ref};

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let row = ws
            .insert(Parent::Root, Node::paragraph("").unwrap())
            .unwrap();
        let external_document = NodeId::new();
        let attr = ws
            .insert_pill(
                row,
                0,
                &Attr::labelled(
                    "document",
                    AttrValue::Ref(Ref::new(external_document)),
                    "Existing note",
                ),
            )
            .unwrap();
        let e = Editor::over(&ws);

        assert_eq!(e.document_behind(&ws, 0, attr), Some(external_document));
    }

    #[test]
    fn an_empty_routed_match_flashes_the_whole_target_block() {
        let (ws, mut e) = editor_over(&["Deep target"]);
        let id = e.body.rows[0].id;

        assert!(e.reveal_search_match(id, ""));

        let flash = e.view.search_flash.as_ref().expect("whole-block cue");
        assert_eq!(flash.node, id);
        assert_eq!(flash.range, 0.."Deep target".len());
        assert_eq!(ws.read(id).unwrap().content.as_text(), Some("Deep target"));
    }

    fn right(ws: &mut WorkspaceDoc, e: &mut Editor) {
        e.key(
            ws,
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
    }

    /// **The change.** A pill is one cluster, so Right used to cross the whole
    /// thing and there was no way to reach one from the keyboard at all.
    #[test]
    fn arrowing_onto_a_pill_stops_on_it_without_moving_the_caret() {
        let (mut ws, mut e, attr, _) = editor_over_pill();

        right(&mut ws, &mut e);

        assert_eq!(e.view.selected_pill, Some((0, attr)));
        assert_eq!(
            e.body.rows[0].caret(),
            5,
            "the stop is the motion — the caret has not moved yet"
        );
    }

    /// ...and a second press in the same direction steps past it, so nothing has
    /// to be pressed to leave the state.
    #[test]
    fn a_second_press_steps_past_the_pill() {
        let (mut ws, mut e, _, _) = editor_over_pill();

        right(&mut ws, &mut e);
        right(&mut ws, &mut e);

        assert_eq!(e.view.selected_pill, None);
        assert_eq!(e.body.rows[0].caret(), 5 + ANCHOR_LEN);
    }

    /// Shift+Right is building a *text* selection. Silently turning it into an
    /// object selection would make the next Backspace delete something other
    /// than what is highlighted.
    #[test]
    fn shift_arrow_still_selects_text_across_a_pill() {
        let (mut ws, mut e, _, _) = editor_over_pill();

        e.key(
            &mut ws,
            &Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::SHIFT,
        );

        assert_eq!(e.view.selected_pill, None);
        assert_eq!(e.body.rows[0].range(), 5..5 + ANCHOR_LEN);
    }

    /// Typing over a selected pill does nothing. A pill is an object here, and
    /// replacing a whole reference with the letter `k` because a hand slipped is
    /// not recoverable by looking at it.
    #[test]
    fn typing_over_a_selected_pill_is_swallowed() {
        let (mut ws, mut e, _, _) = editor_over_pill();
        right(&mut ws, &mut e);
        let before = e.body.rows[0].text.clone();

        e.key(
            &mut ws,
            &Key::Character("k".into()),
            Some("k"),
            ModifiersState::empty(),
        );

        assert_eq!(e.body.rows[0].text, before);
    }

    /// Enter on a document pill asks to open it, and the modifiers say where.
    /// They cannot be chords: Ctrl+Enter is already Toggle done and Shift+Enter
    /// already opens a paragraph above. Ctrl+Enter matches Ctrl+click: a
    /// background tab.
    #[test]
    fn enter_on_a_selected_pill_asks_to_open_it() {
        for (mods, want) in [
            // `SameTab`, not `Reuse`: "open it here" follows the reference in
            // the tab you are standing in, where `Reuse` would open a *second*
            // tab whenever nothing already showed the document.
            (ModifiersState::empty(), OpenWhere::SameTab),
            (koru_keys::PRIMARY, OpenWhere::BackgroundTab),
            (ModifiersState::SHIFT, OpenWhere::NewWindow),
        ] {
            let (mut ws, mut e, _, target) = editor_over_pill();
            right(&mut ws, &mut e);

            e.key(&mut ws, &Key::Named(NamedKey::Enter), None, mods);

            assert_eq!(e.view.wants_open, Some((target, want)), "{mods:?}");
            assert_eq!(
                e.body.rows[0].text, "task \u{2060}",
                "and the row is untouched — Enter did not split it"
            );
        }
    }

    fn pill_centre(e: &Editor, row: usize, attr: AttrId) -> (f32, f32) {
        let r = &e.body.rows[row];
        let ox = r.text_x();
        let oy = r.y;
        let mut point = None;
        crate::rowlayout::visit_pills(r, &e.view.engine.pills, |a, _, _, px, py, pw, ph| {
            if a == attr {
                point = Some((ox + px + pw * 0.5, oy + py + ph * 0.5));
            }
        });
        point.expect("the pill has a rect")
    }

    #[test]
    fn clicking_a_document_pill_asks_to_follow_in_this_tab() {
        let (mut ws, mut e, attr, target) = editor_over_pill();
        let (x, y) = pill_centre(&e, 0, attr);

        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(e.view.wants_open, Some((target, OpenWhere::SameTab)));
        assert!(selected(&e).is_empty());
    }

    #[test]
    fn ctrl_clicking_a_document_pill_asks_for_a_background_tab() {
        let (mut ws, mut e, attr, target) = editor_over_pill();
        let (x, y) = pill_centre(&e, 0, attr);

        assert!(e.mouse_down(&mut ws, x, y, true));
        assert_eq!(e.view.wants_open, Some((target, OpenWhere::BackgroundTab)));
        assert!(
            selected(&e).is_empty(),
            "the pill took the click, so the row was not toggled"
        );
    }

    #[test]
    fn shift_clicking_a_document_pill_asks_for_a_new_window() {
        let (mut ws, mut e, attr, target) = editor_over_pill();
        e.view.mods = ModifiersState::SHIFT;
        let (x, y) = pill_centre(&e, 0, attr);

        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(e.view.wants_open, Some((target, OpenWhere::NewWindow)));
        assert!(
            !e.body.has_row_selection(),
            "the pill took the click, so the row was not extended"
        );
    }

    #[test]
    fn ctrl_clicking_off_a_document_pill_still_toggles_the_row() {
        let (mut ws, mut e, _, _) = editor_over_pill();
        let y = e.body.rows[0].y + e.body.rows[0].height * 0.5;
        let x = e.body.rows[0].text_x() + 2.0;

        assert!(e.mouse_down(&mut ws, x, y, true));
        assert!(e.view.wants_open.is_none());
        assert_eq!(selected(&e), vec![0]);
    }

    fn editor_over_link() -> (WorkspaceDoc, Editor, AttrId) {
        use koru_core::node::{Attr, AttrValue, Node};

        crate::browser::clear_last_launch();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let row = ws
            .insert(Parent::Root, Node::paragraph("see ").unwrap())
            .unwrap();
        let attr = ws
            .insert_pill(
                row,
                4,
                &Attr::labelled(
                    "link",
                    AttrValue::Url("https://example.com".into()),
                    "Example",
                ),
            )
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.view.active = true;
        e.reflow();
        (ws, e, attr)
    }

    #[test]
    fn clicking_a_link_pill_opens_it_in_the_browser() {
        let (mut ws, mut e, attr) = editor_over_link();
        let (x, y) = pill_centre(&e, 0, attr);

        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(
            crate::browser::last_launch(),
            Some(("https://example.com".into(), true))
        );
        assert!(!e.view.overlay.is_open());
    }

    #[test]
    fn ctrl_clicking_a_link_pill_opens_without_activating() {
        let (mut ws, mut e, attr) = editor_over_link();
        let (x, y) = pill_centre(&e, 0, attr);

        assert!(e.mouse_down(&mut ws, x, y, true));
        assert_eq!(
            crate::browser::last_launch(),
            Some(("https://example.com".into(), false))
        );
        assert!(e.view.wants_stay_focused);
    }

    #[test]
    fn copying_from_the_link_chip_sets_a_toast() {
        let (mut ws, mut e, _) = editor_over_link();
        e.view.link_chip = Some(crate::view_state::LinkChip {
            rect: koru_draw::geometry::Rect::new(10.0, 10.0, 200.0, 30.0),
            copy: koru_draw::geometry::Rect::new(160.0, 12.0, 22.0, 22.0),
            edit: koru_draw::geometry::Rect::new(184.0, 12.0, 22.0, 22.0),
            url: "https://example.com".into(),
            row: 0,
            attr: e.body.rows[0].pills.keys().copied().next().unwrap(),
        });

        assert!(e.mouse_down(&mut ws, 171.0, 23.0, false));
        assert_eq!(e.view.wants_toast.as_deref(), Some("Copied to Clipboard"));
        assert!(e.view.link_chip.is_none());
        assert!(crate::browser::last_launch().is_none());
    }

    #[test]
    fn editing_from_the_link_chip_opens_the_picker() {
        let (mut ws, mut e, attr) = editor_over_link();
        e.view.link_chip = Some(crate::view_state::LinkChip {
            rect: koru_draw::geometry::Rect::new(10.0, 10.0, 200.0, 30.0),
            copy: koru_draw::geometry::Rect::new(160.0, 12.0, 22.0, 22.0),
            edit: koru_draw::geometry::Rect::new(184.0, 12.0, 22.0, 22.0),
            url: "https://example.com".into(),
            row: 0,
            attr,
        });

        assert!(e.mouse_down(&mut ws, 195.0, 23.0, false));
        assert!(e.view.overlay.is_open());
        assert!(e.view.link_chip.is_none());
        assert!(crate::browser::last_launch().is_none());
    }

    /// Helper: document with title, empty body line, then a lone document pill.
    fn editor_over_pill_with_empty_above() -> (WorkspaceDoc, Editor, usize, f32, f32, f32, f32) {
        use koru_core::Ref;
        use koru_core::node::{Attr, AttrValue, Node};

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Title").unwrap())
            .unwrap();
        let target = ws
            .insert(Parent::Root, Node::document("adsfadsf").unwrap())
            .unwrap();
        let _empty = ws
            .insert(Parent::Node(doc), Node::paragraph("").unwrap())
            .unwrap();
        let row_id = ws
            .insert(Parent::Node(doc), Node::paragraph("").unwrap())
            .unwrap();
        let _attr = ws
            .insert_pill(
                row_id,
                0,
                &Attr::new("ref", AttrValue::Ref(Ref::new(target))),
            )
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.title_row = true;
        e.body.zoom_to(&ws, Some(doc));
        e.view.active = true;
        e.set_viewport(900.0, 560.0);
        e.reflow();
        let pill_i = e
            .body
            .rows
            .iter()
            .position(|r| !r.pills.is_empty())
            .expect("pill row");
        let mut pill = None;
        crate::rowlayout::visit_pills(
            &e.body.rows[pill_i],
            &e.view.engine.pills,
            |_, _, _, x, y, w, h| {
                pill = Some((x, y, w, h));
            },
        );
        let (px, py, pw, ph) = pill.expect("visual");
        (ws, e, pill_i, px, py, pw, ph)
    }

    fn caret_meets_pill(e: &Editor, pill_i: usize, py: f32, ph: f32) {
        let row_y = e.body.rows[pill_i].y;
        let scroll = e.view.scroll;
        let screen = e.caret_screen_rect().expect("caret rect");
        let pill_top = row_y - scroll + py;
        let pill_bot = pill_top + ph;
        assert!(
            screen.y + screen.h > pill_top - 2.0 && screen.y < pill_bot + 2.0,
            "caret y={}-{} must meet pill y={pill_top}-{pill_bot}; focus={} caret={:?} selected={:?}",
            screen.y,
            screen.y + screen.h,
            e.body.focus,
            e.body.rows.get(e.body.focus).map(|r| r.caret()),
            e.view.selected_pill,
        );
    }

    /// Clicks just to the right of a document pill must land on that row after
    /// the pill — not on the title through the title gap above it.
    #[test]
    fn clicking_right_of_a_pill_does_not_land_on_the_row_above() {
        let (mut ws, mut e, pill_i, px, py, pw, ph) = editor_over_pill_with_empty_above();
        let row_y = e.body.rows[pill_i].y;
        let text_x = e.body.rows[pill_i].text_x();
        let content_x = e.view.content_x;
        let scroll = e.view.scroll;
        let wx = content_x + text_x + px + pw + 3.0;
        for (label, ly) in [
            ("mid", py + ph * 0.5),
            ("top", py + 1.0),
            ("baseline", 14.0),
            ("bottom_visual", py + ph - 1.0),
        ] {
            // Start on the title so a wrong hit is obvious.
            e.body.focus = 0;
            e.body.rows[0].place_caret(0);
            let wy = row_y - scroll + ly;
            e.mouse_down(&mut ws, wx, wy, false);
            assert_eq!(e.body.focus, pill_i, "{label}: focus on pill row");
            assert!(e.view.selected_pill.is_none(), "{label}: not selected");
            assert!(e.body.rows[pill_i].caret() >= 3, "{label}: after anchor");
            caret_meets_pill(&e, pill_i, py, ph);
        }
    }

    /// A click in the title gap (the 40px air under the name), to the right of
    /// where a body pill sits, must land on the body — not put the caret back
    /// on the title above the pill.
    #[test]
    fn clicking_in_the_title_gap_beside_a_pill_stays_on_the_body() {
        use koru_core::Ref;
        use koru_core::node::{Attr, AttrValue, Node};
        use koru_draw::theme;

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Title").unwrap())
            .unwrap();
        let target = ws
            .insert(Parent::Root, Node::document("adsfadsf").unwrap())
            .unwrap();
        let row_id = ws
            .insert(Parent::Node(doc), Node::paragraph("").unwrap())
            .unwrap();
        ws.insert_pill(
            row_id,
            0,
            &Attr::new("ref", AttrValue::Ref(Ref::new(target))),
        )
        .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.title_row = true;
        e.body.zoom_to(&ws, Some(doc));
        e.view.active = true;
        e.set_viewport(900.0, 560.0);
        e.reflow();

        let pill_i = e
            .body
            .rows
            .iter()
            .position(|r| !r.pills.is_empty())
            .expect("pill");
        assert!(e.body.rows[0].is_title);
        let title_bot = e.body.rows[0].y + e.body.rows[0].height;
        let mut pill_w = 0.0;
        crate::rowlayout::visit_pills(
            &e.body.rows[pill_i],
            &e.view.engine.pills,
            |_, _, _, _, _, w, _| pill_w = w,
        );
        let wx = e.view.content_x + e.body.rows[pill_i].text_x() + pill_w + 8.0;
        let wy = title_bot + theme::TITLE_GAP_BELOW * 0.5 - e.view.scroll;

        e.body.focus = 0;
        e.mouse_down(&mut ws, wx, wy, false);

        assert_eq!(
            e.body.focus, pill_i,
            "title-gap click must not return the caret to the title"
        );
        assert!(e.body.rows[pill_i].caret() >= 3, "after the pill");
    }

    /// Right arrow onto / past a pill keeps the caret on the pill's vertical
    /// band — never a line above it.
    #[test]
    fn right_arrow_past_a_pill_keeps_the_caret_on_its_line() {
        let (mut ws, mut e, pill_i, _px, py, _pw, ph) = editor_over_pill_with_empty_above();
        // Start on the empty row above, at its end, then Right into the pill row.
        let empty_i = pill_i - 1;
        e.body.focus = empty_i;
        e.body.rows[empty_i].place_caret(0);
        e.reflow();

        right(&mut ws, &mut e); // onto pill row start
        assert_eq!(e.body.focus, pill_i, "entered pill row");
        right(&mut ws, &mut e); // select pill
        assert_eq!(e.view.selected_pill.map(|(i, _)| i), Some(pill_i));
        // No caret while selected — skip geometry.
        right(&mut ws, &mut e); // step past
        assert!(e.view.selected_pill.is_none());
        assert_eq!(e.body.focus, pill_i);
        caret_meets_pill(&e, pill_i, py, ph);
        // And clicking right of the pill after that still stays put.
        let row_y = e.body.rows[pill_i].y;
        let wx = e.view.content_x + e.body.rows[pill_i].text_x() + 80.0;
        let wy = row_y - e.view.scroll + py + ph * 0.5;
        e.mouse_down(&mut ws, wx, wy, false);
        assert_eq!(e.body.focus, pill_i);
        caret_meets_pill(&e, pill_i, py, ph);
    }

    /// An editor over one paragraph whose body is a single tone-capable emoji.
    fn editor_over_emoji() -> (WorkspaceDoc, Editor, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, koru_core::node::Node::document("d").unwrap())
            .unwrap();
        let para = ws
            .insert(
                Parent::Node(doc),
                koru_core::node::Node::paragraph("👋").unwrap(),
            )
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.set_viewport(900.0, 560.0);
        (ws, e, para)
    }

    #[test]
    fn long_press_on_a_body_emoji_opens_the_tone_strip() {
        let (mut ws, mut e, _) = editor_over_emoji();
        let row = &e.body.rows[0];
        // Content-local centre of the glyph.
        let lx = row.text_x() + 8.0;
        let ly = row.y + row.height * 0.5;
        let x = e.view.content_x + lx;
        let y = ly - e.view.scroll;
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert!(e.view.body_tone_press.is_some(), "press armed");
        assert!(e.view.body_tone_strip.is_none());
        // Force the hold past the threshold, then tick.
        if let Some(p) = e.view.body_tone_press.as_mut() {
            p.since = Instant::now()
                - std::time::Duration::from_millis(
                    crate::components::emoji_picker::LONG_PRESS_MS + 50,
                );
        }
        assert!(e.tick_overlay_press(), "strip opened");
        assert!(e.view.body_tone_press.is_none());
        let strip = e.view.body_tone_strip.as_ref().expect("strip open");
        assert!(strip.tones.len() > 1);
        assert_eq!(strip.expected, "👋");
    }

    #[test]
    fn choosing_a_body_tone_replaces_the_grapheme() {
        // Committing a tone remembers it process-wide, so this holds the same
        // lock as every test that sets or reads the preferred tone.
        let _guard = crate::emoji::tone_test_lock();
        let (mut ws, mut e, node) = editor_over_emoji();
        let row = &e.body.rows[0];
        let lx = row.text_x() + 8.0;
        let ly = row.y + row.height * 0.5;
        let x = e.view.content_x + lx;
        let y = ly - e.view.scroll;
        e.mouse_down(&mut ws, x, y, false);
        if let Some(p) = e.view.body_tone_press.as_mut() {
            p.since = Instant::now()
                - std::time::Duration::from_millis(
                    crate::components::emoji_picker::LONG_PRESS_MS + 50,
                );
        }
        e.tick_overlay_press();
        let strip = e.view.body_tone_strip.clone().expect("strip");
        let tone = strip
            .tones
            .iter()
            .find(|t| **t != "👋")
            .expect("a non-default tone");
        e.commit_body_tone(&mut ws, &strip, tone);
        let node = ws.read(node).unwrap();
        let text = node.content.as_text().unwrap_or_default();
        assert_eq!(text, *tone);
        assert!(e.view.body_tone_strip.is_none());
        crate::emoji::set_preferred_skin_tone(emojis::SkinTone::Default);
    }

    #[test]
    fn short_click_on_a_body_emoji_does_not_open_the_strip() {
        let (mut ws, mut e, _) = editor_over_emoji();
        let row = &e.body.rows[0];
        let x = e.view.content_x + row.text_x() + 8.0;
        let y = row.y + row.height * 0.5 - e.view.scroll;
        e.mouse_down(&mut ws, x, y, false);
        assert!(e.view.body_tone_press.is_some());
        e.mouse_up(&mut ws);
        assert!(e.view.body_tone_press.is_none());
        assert!(e.view.body_tone_strip.is_none());
    }

    // ---------------------------------------------- the layout/text index split
    //
    // A row is shaped as `text + LAYOUT_STRUT` (`rowlayout.rs`), so a selection
    // taken from the *layout* can sit up to three bytes past the end of the
    // *text*. These pin the rule that such an index never survives the write —
    // see `Paragraph::set_selection_from_layout` and ADR-75.

    /// The guard for the whole class, rather than for one key: after any
    /// selection move, the stored selection is inside the text.
    ///
    /// This is the test whose absence let the strut land with eleven unrelated
    /// -looking failures. It names the invariant, so the next person to add a
    /// layout-only character sees *which rule* they broke.
    #[test]
    fn no_selection_move_leaves_an_index_past_the_end_of_the_text() {
        let moves: &[(&str, Action)] = &[
            ("End", Action::LineEnd { extend: false }),
            ("Shift+End", Action::LineEnd { extend: true }),
            ("Home", Action::LineStart { extend: false }),
            ("Shift+Home", Action::LineStart { extend: true }),
            ("Down", Action::MoveDown { extend: false }),
            ("Shift+Down", Action::MoveDown { extend: true }),
            ("Up", Action::MoveUp { extend: false }),
        ];
        for (name, action) in moves {
            let (mut ws, mut e) = editor_over(&["one two three"]);
            e.act(&mut ws, *action);
            let row = e.body.focused().unwrap();
            let end = row.selection.text_range().end;
            assert!(
                end <= row.text.len(),
                "{name} left the selection at {end}, past {:?} ({} bytes)",
                row.text,
                row.text.len(),
            );
        }
    }

    /// The symptom no panic ever revealed: the keystroke after End was pushed
    /// at an out-of-range offset, Loro refused it, and the error was discarded.
    #[test]
    fn typing_after_end_appends_rather_than_vanishing() {
        let (mut ws, mut e) = editor_over(&["a task"]);
        e.act(&mut ws, Action::LineEnd { extend: false });
        e.type_text(&mut ws, " more");
        assert_eq!(e.body.focused().unwrap().text, "a task more");
    }

    /// End then a slice of `row.text` — the shape that panicked at
    /// `editor.rs`'s link prompt and in `trigger::is_trigger_position`.
    #[test]
    fn end_then_a_text_slice_does_not_panic() {
        let (mut ws, mut e) = editor_over(&["due"]);
        e.act(&mut ws, Action::LineEnd { extend: false });
        let row = e.body.focused().unwrap();
        let _ = &row.text[row.range()];
        let _ = &row.text[..row.caret()];
    }

    // ----------------------------------------------------------- attributes

    /// A tag table for the chooser to list, pushed the way the host would.
    fn seed_tags(e: &mut Editor, names: &[&str]) -> Vec<koru_core::ids::TagId> {
        let ids: Vec<koru_core::ids::TagId> =
            names.iter().map(|_| koru_core::ids::TagId::new()).collect();
        e.view.tags.replace(ids.iter().zip(names).map(|(id, name)| {
            (
                *id,
                crate::chips::TagFace {
                    name: (*name).to_owned(),
                    color: None,
                },
            )
        }));
        ids
    }

    /// A document with a title row and one body paragraph, the **title**
    /// focused with the caret at the end of the name — the setup for anything
    /// typed into a document's own name.
    fn doc_editor_on_title(name: &str) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document(name).unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("body").unwrap())
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        e.body.focus = 0;
        let end = e.body.rows[0].text.len();
        e.body.rows[0].place_caret(end);
        // `place_caret` is *pending* until a layout exists to snap it into.
        e.reflow();
        assert!(e.body.focus_is_title(), "the caret is on the name");
        assert_eq!(e.body.rows[0].caret(), end, "and at the end of the name");
        (ws, e)
    }

    /// `#` after a space, matching a tag's first letter, opens the chooser —
    /// and the guards each keep it shut: line start (heading shorthand),
    /// mid-word, and a one-character query matching nothing.
    #[test]
    fn hash_arms_the_tag_chooser_only_on_the_guarded_grammar() {
        // Mid-line, matching: opens filtered.
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp", "marketing"]);
        type_str(&mut e, &mut ws, "x #m");
        match &e.view.overlay {
            Overlay::Tag(picker) => {
                assert_eq!(picker.hits.len(), 2, "both m-tags listed");
                assert!(!picker.has_search(), "the query is the document text");
            }
            other => panic!("expected the tag chooser, got {}", overlay_name(other)),
        }

        // Line start is the heading shorthand's, so `#` there stays quiet.
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, "#m");
        assert!(matches!(e.view.overlay, Overlay::None));

        // Mid-word is prose, not a trigger.
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, "a#m");
        assert!(matches!(e.view.overlay, Overlay::None));

        // One character matching nothing never opens; the second is an
        // intent, and the create row is the offer.
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, "x #z");
        assert!(matches!(e.view.overlay, Overlay::None));
        type_str(&mut e, &mut ws, "z");
        match &e.view.overlay {
            Overlay::Tag(picker) => {
                assert!(picker.hits.is_empty());
                assert_eq!(picker.offer_create().as_deref(), Some("zz"));
            }
            other => panic!("expected the tag chooser, got {}", overlay_name(other)),
        }

        // **A document's own name is not excluded.** It was, on the argument
        // that every word in a name is part of it — but `/tag` already tags a
        // document from its title row, so refusing the shorthand there left
        // the two doors to one attribute disagreeing (ADR-187).
        let (mut ws, mut e) = doc_editor_on_title("Spec");
        seed_tags(&mut e, &["mvp", "marketing"]);
        type_str(&mut e, &mut ws, " #m");
        match &e.view.overlay {
            Overlay::Tag(picker) => assert_eq!(picker.hits.len(), 2),
            other => panic!("expected the tag chooser, got {}", overlay_name(other)),
        }

        // And the chooser's own contract is what keeps a stray `#` in a name
        // quiet, exactly as it does in prose: `C# notes` is a name, not a tag.
        let (mut ws, mut e) = doc_editor_on_title("C");
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, "# no");
        assert!(matches!(e.view.overlay, Overlay::None));
        assert_eq!(e.body.rows[0].text, "C# no");

        // One typed character matching nothing stays quiet there too.
        let (mut ws, mut e) = doc_editor_on_title("Spec");
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, " #z");
        assert!(matches!(e.view.overlay, Overlay::None));
    }

    fn overlay_name(overlay: &Overlay) -> &'static str {
        match overlay {
            Overlay::None => "None",
            Overlay::Command(_) => "Command",
            Overlay::Entity(_) => "Entity",
            Overlay::Date(_) => "Date",
            Overlay::Name(_) => "Name",
            Overlay::Link(_) => "Link",
            Overlay::Language(_) => "Language",
            Overlay::Emoji(_) => "Emoji",
            Overlay::Attr(_) => "Attr",
            Overlay::Tag(_) => "Tag",
            Overlay::Color(_) => "Color",
            Overlay::Priority(_) => "Priority",
            Overlay::List(_) => "List",
        }
    }

    /// Committing from `#tag` applies the tag, takes the trigger text back
    /// out, and the whole gesture is **one** undo step.
    #[test]
    fn committing_a_hash_tag_is_one_undo_step() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let ids = seed_tags(&mut e, &["mvp"]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "ship it #m");
        assert!(matches!(e.view.overlay, Overlay::Tag(_)));
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(matches!(e.view.overlay, Overlay::None));
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("ship it "),
            "the trigger text went with the commit"
        );
        assert!(ws.read(node).unwrap().tag_ids().any(|t| t == ids[0]));
        assert!(
            !e.body.focused().unwrap().chips.is_empty(),
            "the chip is on"
        );

        assert!(ws.undo().unwrap());
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("ship it #m"),
            "one Ctrl+Z restores the typed text"
        );
        assert!(
            !ws.read(node).unwrap().tag_ids().any(|t| t == ids[0]),
            "and takes the tag with it"
        );
    }

    /// **The three selection shapes an attribute gesture can be given**, and
    /// only two of them have an honest answer.
    ///
    /// Body rows alone apply to every one of them; the title alone applies to
    /// the document. A selection covering both is refused outright rather than
    /// applied to the part that happens to be easy — which is what it did:
    /// `attribute_targets` filtered the title out and tagged the paragraphs,
    /// so three rows were selected and two of them changed (ADR-187).
    #[test]
    fn an_attribute_gesture_refuses_a_mixed_title_and_body_selection() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        for text in ["one", "two"] {
            ws.insert(Parent::Node(doc), Node::paragraph(text).unwrap())
                .unwrap();
        }
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.title_row = true;
        e.body.rebuild(&ws);
        e.set_viewport(900.0, 560.0);
        let ids: Vec<NodeId> = e.body.rows.iter().map(|row| row.id).collect();
        assert_eq!(ids.len(), 3, "the title and two paragraphs");

        // The title alone: the document, offered.
        e.body.clear_selection();
        e.body.focus = 0;
        assert_eq!(e.attribute_targets(ids[0]), Some(vec![ids[0]]));
        assert!(!e.attribute_selection_is_mixed());

        // Both body rows, no title: all of them, offered.
        e.body.focus = 2;
        e.body.anchor = Some(ids[1]);
        e.body.mark_selection();
        assert_eq!(e.attribute_targets(ids[2]), Some(vec![ids[1], ids[2]]));
        assert!(!e.attribute_selection_is_mixed());

        // The title and a body row together: refused.
        e.body.focus = 1;
        e.body.anchor = Some(ids[0]);
        e.body.mark_selection();
        assert_eq!(e.attribute_targets(ids[1]), None);
        assert!(e.attribute_selection_is_mixed());

        // …and the whole document, title included, likewise.
        e.body.focus = 2;
        e.body.mark_selection();
        assert_eq!(e.attribute_targets(ids[2]), None);
        assert!(e.attribute_selection_is_mixed());
    }

    /// `/tag` reaches the same chooser through the command menu, and choosing
    /// the **ticked** row takes that tag off ([ADR-188], amended twice) — the
    /// panel is drawing the checkmark, so the click that reverses it is the
    /// obvious one.
    ///
    /// The commit runs either way, and that is what this pins beyond the tag
    /// count: the typed `/tag` has to come back out of the row on a removal
    /// exactly as on an apply.
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    #[test]
    fn slash_tag_opens_the_chooser_and_a_ticked_row_takes_the_tag_off() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let ids = seed_tags(&mut e, &["mvp"]);
        let node = e.body.focused().unwrap().id;
        ws.add_tag(node, ids[0]).unwrap();
        e.body.refresh(&ws, node);

        type_str(&mut e, &mut ws, "/tag");
        assert!(matches!(e.view.overlay, Overlay::Command(_)));
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        let Overlay::Tag(picker) = &e.view.overlay else {
            panic!("choosing /tag should open the tag chooser");
        };
        assert!(picker.has_search(), "the /tag chooser has its own field");
        assert!(picker.applied.contains(&ids[0]), "shown checked");

        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            ws.read(node)
                .unwrap()
                .tag_ids()
                .filter(|t| *t == ids[0])
                .count(),
            0,
            "the ticked row was chosen again, so the tag came off"
        );
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some(""),
            "the /tag trigger text went too"
        );

        // The panel is still up with the row unticked — so the same row now
        // adds, and the block is back where it started.
        let Overlay::Tag(picker) = &e.view.overlay else {
            panic!("removing through the chooser leaves it open");
        };
        assert!(!picker.applied.contains(&ids[0]), "and unticked");
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            ws.read(node)
                .unwrap()
                .tag_ids()
                .filter(|t| *t == ids[0])
                .count(),
            1,
            "and choosing the unticked row puts it back"
        );
    }

    /// `/color` sets the token, the picker preselects it next time, and Clear
    /// takes it away.
    #[test]
    fn slash_color_sets_preselects_and_clears() {
        use koru_core::attributes::{AccentColor, KEY_COLOR};
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;

        type_str(&mut e, &mut ws, "/color");
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(matches!(e.view.overlay, Overlay::Color(_)));
        // Row 0 with nothing set is Red.
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        let color = |ws: &WorkspaceDoc| {
            ws.read(node).unwrap().content.attrs().iter().find_map(|a| {
                (a.role == KEY_COLOR)
                    .then(|| a.as_text().map(str::to_owned))
                    .flatten()
            })
        };
        assert_eq!(color(&ws).as_deref(), Some("red"));
        assert!(!e.body.focused().unwrap().chips.is_empty());

        // Reopening preselects the set token and offers Clear at row 0.
        e.open_color_picker_over(&ws, vec![node]);
        let Overlay::Color(picker) = &e.view.overlay else {
            panic!("expected the color picker");
        };
        assert_eq!(picker.current, Some(AccentColor::Red));
        assert!(picker.offers_clear());
        assert_eq!(picker.sel, 1, "red sits past the Clear row");
        // Choose Clear by clicking row 0.
        let trigger = e.view.overlay.trigger();
        let outcome = match &e.view.overlay {
            Overlay::Color(p) => p.click(theme::ROW_H * 0.5),
            _ => unreachable!(),
        };
        e.apply_overlay_outcome(&mut ws, trigger, outcome);
        assert_eq!(color(&ws), None, "Clear removed the attribute");
        assert!(e.body.focused().unwrap().chips.is_empty());
    }

    /// `/attr` browses: its rows become the pickers, on the same trigger.
    #[test]
    fn slash_attr_becomes_the_specific_picker() {
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp"]);
        type_str(&mut e, &mut ws, "/attr");
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(matches!(e.view.overlay, Overlay::Attr(_)));
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(
            matches!(e.view.overlay, Overlay::Tag(_)),
            "row one of /attr is Tags"
        );
    }

    /// A multi-node apply from the menu path: every selected root gets the
    /// tag, in one undo step.
    #[test]
    fn a_menu_tag_apply_covers_the_selection_in_one_step() {
        let (mut ws, mut e) = doc_editor(&["one", "two", "three"]);
        let ids = seed_tags(&mut e, &["mvp"]);
        let nodes: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        assert!(e.open_tag_picker_over(&ws, nodes.clone()));
        let trigger = e.view.overlay.trigger();
        assert!(matches!(
            trigger.as_ref().map(|t| t.anchor),
            Some(Anchor::Insert)
        ));
        ws.enable_undo();
        e.attribute_outcome(&mut ws, trigger, &Outcome::AddTag(nodes.clone(), ids[0]));
        for &n in &nodes {
            assert!(ws.read(n).unwrap().tag_ids().any(|t| t == ids[0]));
        }
        assert!(ws.undo().unwrap());
        for &n in &nodes {
            assert!(
                !ws.read(n).unwrap().tag_ids().any(|t| t == ids[0]),
                "one Ctrl+Z takes the whole apply back"
            );
        }
    }

    /// The create row hands the request up rather than writing a table this
    /// layer cannot see — and still takes the trigger text out.
    #[test]
    fn creating_a_tag_asks_the_host() {
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["mvp"]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "note #roadmap");
        assert!(matches!(e.view.overlay, Overlay::Tag(_)));
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert_eq!(
            e.view.wants_create_tag.take(),
            Some((vec![node], "roadmap".to_owned()))
        );
        assert_eq!(ws.read(node).unwrap().content.as_text(), Some("note "));
    }

    // ------------------------------------------------------- due + priority

    fn enter(e: &mut Editor, ws: &mut WorkspaceDoc) {
        e.key(
            ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
    }

    /// The node's `koru.due` — the registered key only, so a legacy date
    /// *pill* (bare `"due"` role) does not read as the attribute.
    fn due_of(ws: &WorkspaceDoc, node: NodeId) -> Option<AttrValue> {
        ws.read(node)
            .unwrap()
            .content
            .attrs()
            .iter()
            .find(|a| a.role == koru_core::attributes::KEY_DUE)
            .map(|a| a.value.clone())
    }

    /// `/due` opens the date grid **armed for the attribute**, and Enter
    /// stores a calendar date on the node. `/date` — which used to open the
    /// same grid unarmed, for an inline pill — is gone: two ways to put a date
    /// on a block was one too many, and the attribute is the one that survives
    /// ([ADR-210](../../../docs/decisions/README.md)).
    #[test]
    fn slash_due_arms_the_grid_and_enter_sets_the_attribute() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/due");
        enter(&mut e, &mut ws);
        match &e.view.overlay {
            Overlay::Date(p) => assert!(p.due.is_some(), "armed for koru.due"),
            other => panic!("expected the date grid, got {}", overlay_name(other)),
        }
        enter(&mut e, &mut ws);
        assert!(matches!(e.view.overlay, Overlay::None));
        assert!(
            matches!(due_of(&ws, node), Some(AttrValue::Date(_))),
            "a grid pick is a calendar date (D3)"
        );
        assert!(
            !e.body.focused().unwrap().chips.is_empty(),
            "the chip is on"
        );
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some(""),
            "the /due trigger text went with the commit"
        );

        // `/date` names nothing now: the menu stays open with no rows rather
        // than opening a second date panel, and Enter writes nothing.
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/date");
        match &e.view.overlay {
            Overlay::Command(menu) => assert!(menu.hits.is_empty(), "no command is called date"),
            other => panic!("expected the command menu, got {}", overlay_name(other)),
        }
        enter(&mut e, &mut ws);
        assert!(due_of(&ws, node).is_none());
        assert!(
            !ws.read(node)
                .unwrap()
                .content
                .as_text()
                .unwrap_or_default()
                .contains(ANCHOR),
            "no pill anchor landed in the text"
        );
    }

    /// `due tomorrow` typed in the command field commits directly — no
    /// picker — and the whole gesture is one undo step.
    #[test]
    fn a_typed_due_argument_commits_directly_in_one_step() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        let before = chrono::Local::now().date_naive();
        type_str(&mut e, &mut ws, "/due tomorrow");
        assert!(matches!(e.view.overlay, Overlay::Command(_)));
        enter(&mut e, &mut ws);
        let after = chrono::Local::now().date_naive();
        assert!(matches!(e.view.overlay, Overlay::None), "no picker opened");
        let Some(AttrValue::Date(d)) = due_of(&ws, node) else {
            panic!("expected a calendar date, got {:?}", due_of(&ws, node));
        };
        // Bracketed rather than exact, so a run crossing midnight cannot
        // flake: tomorrow-of-before <= d <= tomorrow-of-after.
        assert!(d.date() >= before.succ_opt().unwrap() && d.date() <= after.succ_opt().unwrap());
        assert_eq!(ws.read(node).unwrap().content.as_text(), Some(""));

        assert!(ws.undo().unwrap());
        assert!(due_of(&ws, node).is_none(), "one Ctrl+Z takes it all back");
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("/"),
            "and restores the trigger text"
        );
    }

    /// **A refused command never eats what was typed.**
    ///
    /// `commit_attribute` takes the trigger text out and runs the command in
    /// one undo group, and `edit_group` is not a transaction — it brackets undo
    /// grouping and rolls nothing back. With the deletion first, a command that
    /// refused left `/due tomorrow` deleted and no chip in its place: the user
    /// watched their typing disappear and got no attribute and no message. A
    /// quote is the refusal reachable from here (SPEC §4's kind exclusion); the
    /// one that shipped was a document carrying no workspace id, which refused
    /// the same way on every row in the file.
    ///
    /// Only the `/` is ever in the document — the argument lives in the command
    /// overlay's own field — so what survives is that character, against the
    /// empty row the old order left. The overlay still closes, which means the
    /// typed argument is still lost; putting it back is a separate change.
    #[test]
    fn a_refused_due_leaves_the_typed_trigger_alone() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        ws.set_kind(node, Kind::Quote).unwrap();
        e.body.rebuild(&ws);
        e.place_caret(0);
        let before = crate::tasks::refused_commits();

        type_str(&mut e, &mut ws, "/due tomorrow");
        enter(&mut e, &mut ws);

        assert!(
            due_of(&ws, node).is_none(),
            "a quote cannot carry task state, so the due was right to refuse"
        );
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("/"),
            "and the trigger is still there — this row read `\"\"` before the \
             fix, so the gesture blanked the line and left nothing behind"
        );
        assert_eq!(
            crate::tasks::refused_commits(),
            before + 1,
            "the refusal is a number a diagnostic can see, not silence"
        );
    }

    /// **A timed due is stored zoned, and the zone is never absent.**
    ///
    /// `koru.due` is shared, so a deadline has to name the zone it was meant
    /// in or two collaborators cannot agree which moment it is (ADR-183). The
    /// parser handed the commit a *floating* wall clock — what the person
    /// said — and the commit resolved it through this machine's zone. There is
    /// no code path that stores the instant on its own.
    #[test]
    fn a_timed_due_argument_stores_a_zoned_deadline() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/due in 1h");
        enter(&mut e, &mut ws);
        match due_of(&ws, node) {
            Some(AttrValue::ZonedDateTime(zoned)) => {
                assert!(
                    !zoned.zone().as_str().is_empty(),
                    "a shared deadline names its zone"
                );
                let now_ms = chrono::Utc::now().timestamp_millis();
                let hour = 3_600_000;
                assert!(
                    (zoned.instant_ms() - now_ms - hour).abs() < 120_000,
                    "about an hour from now"
                );
            }
            other => panic!("expected a zoned deadline, got {other:?}"),
        }
    }

    /// A day is a day. `/due tomorrow` names no time, so nothing invents one —
    /// and in particular nothing resolves it to midnight in some zone, which
    /// is the date-only-through-UTC bug the three types exist to prevent.
    #[test]
    fn a_dateless_phrase_never_acquires_a_time_or_a_zone() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/due friday");
        enter(&mut e, &mut ws);
        assert!(
            matches!(due_of(&ws, node), Some(AttrValue::Date(_))),
            "got {:?}",
            due_of(&ws, node)
        );
    }

    /// A phrase the grammar does not know opens the picker instead of
    /// guessing — armed for the attribute, seeded on today.
    #[test]
    fn an_unparsed_due_argument_falls_back_to_the_picker() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/due someday");
        enter(&mut e, &mut ws);
        match &e.view.overlay {
            Overlay::Date(p) => {
                assert!(p.due.is_some(), "still armed for the attribute");
                assert!(!p.offers_clear(), "nothing set yet");
            }
            other => panic!("expected the date grid, got {}", overlay_name(other)),
        }
        assert!(due_of(&ws, node).is_none(), "nothing was committed");
    }

    /// Reopening over a due-carrying node preselects its day and offers
    /// Clear; Delete clears — one step, and the chip stops carrying a date.
    ///
    /// **It does not stop being drawn**, and that is SPEC §4 rather than a
    /// leak: setting a due made this node a task, `clear_due` marks first so
    /// the deadline can come off without the task going with it, and a task's
    /// indicator *is* its due chip. The chip therefore dashes. Until 2026-08-31
    /// the last line here asserted the row went back to no chips at all, which
    /// was true only while a dateless task drew nothing.
    #[test]
    fn the_due_picker_preselects_and_clears() {
        let (mut ws, mut e) = doc_editor(&["task"]);
        let node = e.body.focused().unwrap().id;
        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        ws.set_attribute(node, koru_core::attributes::KEY_DUE, AttrValue::Date(due))
            .unwrap();
        e.body.refresh(&ws, node);
        assert!(!e.body.focused().unwrap().chips.is_empty());

        assert!(e.open_due_picker_over(&ws, vec![node]));
        match &e.view.overlay {
            Overlay::Date(p) => {
                assert_eq!(p.grid.selected, due.date(), "opens on the set day");
                assert!(p.offers_clear());
            }
            other => panic!("expected the date grid, got {}", overlay_name(other)),
        }
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Delete),
            None,
            ModifiersState::empty(),
        );
        assert!(due_of(&ws, node).is_none(), "Clear removed the attribute");
        assert_eq!(
            e.body.focused().unwrap().chips,
            vec![crate::chips::ChipSpec::Due(None)],
            "the date went; the task, and so its chip, stayed"
        );
    }

    /// `/priority` opens the picker; `priority high` and the unambiguous
    /// `pri h` commit directly; the picker preselects and clears.
    #[test]
    fn slash_priority_commits_types_and_clears() {
        use koru_core::attributes::{KEY_PRIORITY, Priority};
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;

        // Typed argument: direct commit, one undo step.
        type_str(&mut e, &mut ws, "/priority high");
        enter(&mut e, &mut ws);
        assert!(matches!(e.view.overlay, Overlay::None));
        assert_eq!(ws.read(node).unwrap().priority(), Some(Priority::High));
        assert!(
            e.body
                .focused()
                .unwrap()
                .chips
                .iter()
                .any(|c| matches!(c, crate::chips::ChipSpec::Priority(Priority::High))),
            "the chip is on"
        );
        assert!(ws.undo().unwrap());
        assert_eq!(ws.read(node).unwrap().priority(), None);
        assert_eq!(ws.read(node).unwrap().content.as_text(), Some("/"));

        // An unambiguous prefix: the four levels have distinct first letters.
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/pri u");
        enter(&mut e, &mut ws);
        assert_eq!(ws.read(node).unwrap().priority(), Some(Priority::Urgent));

        // No argument: the picker, preselected, with Clear at row 0.
        e.body.refresh(&ws, node);
        assert!(e.open_priority_picker_over(&ws, vec![node]));
        let Overlay::Priority(picker) = &e.view.overlay else {
            panic!("expected the priority picker");
        };
        assert_eq!(picker.current, Some(Priority::Urgent));
        assert!(picker.offers_clear());
        assert_eq!(picker.sel, 1, "Urgent sits past the Clear row");
        let trigger = e.view.overlay.trigger();
        let outcome = match &e.view.overlay {
            Overlay::Priority(p) => p.click(theme::ROW_H * 0.5),
            _ => unreachable!(),
        };
        e.apply_overlay_outcome(&mut ws, trigger, outcome);
        assert_eq!(ws.read(node).unwrap().priority(), None, "cleared");
        assert!(
            !e.body
                .focused()
                .unwrap()
                .chips
                .iter()
                .any(|c| matches!(c, crate::chips::ChipSpec::Priority(_))),
            "the chip went with it"
        );
        let _ = KEY_PRIORITY;
    }

    /// A garbage priority argument opens the picker rather than guessing a
    /// level.
    #[test]
    fn an_unparsed_priority_argument_falls_back_to_the_picker() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        type_str(&mut e, &mut ws, "/priority soon");
        enter(&mut e, &mut ws);
        assert!(matches!(e.view.overlay, Overlay::Priority(_)));
        assert_eq!(ws.read(node).unwrap().priority(), None);
    }

    // ---------------------------------------- routing a mixed selection
    //
    // Every attribute gesture lands on `attribute_outcome`, which asks
    // `tasks::route_attribute` **per node** — so a selection holding a task, a
    // plain row and a kind that cannot carry task state does the right thing
    // for each rather than one thing for all three (ADR-273).

    /// A due over a mixed selection: the two capable rows become dated tasks,
    /// the quote is refused and left alone, and the whole apply is still one
    /// undo step.
    #[test]
    fn a_due_over_mixed_targets_routes_each_one() {
        use koru_core::attributes::KEY_DUE;
        let (mut ws, mut e) = doc_editor(&["plain", "already a task", "quoted"]);
        let nodes: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        let (plain, task, quote) = (nodes[0], nodes[1], nodes[2]);
        ws.set_kind(quote, Kind::Quote).unwrap();
        koru_core::tasks::ensure_task(&mut ws, task, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
        e.body.rebuild(&ws);
        ws.enable_undo();

        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::SetAttr(nodes.clone(), KEY_DUE.to_owned(), AttrValue::Date(due)),
        );

        // A plain row a due lands on **becomes** a task, with the deadline as
        // the reason it exists; one that already was keeps the reason it had.
        let dated = koru_core::time::Temporal::Date(due);
        assert_eq!(ws.read(plain).unwrap().due(), Some(dated.clone()));
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(plain).unwrap()).marker,
            Some(koru_core::attributes::TaskReason::Due)
        );
        assert_eq!(ws.read(task).unwrap().due(), Some(dated));
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(task).unwrap()).marker,
            Some(koru_core::attributes::TaskReason::Explicit),
            "the marker records how the task came to exist, and it already did"
        );
        assert!(
            ws.read(quote).unwrap().due().is_none(),
            "a quote cannot carry task state and the door refused it"
        );

        // Two commands and the trigger-free commit, still one Ctrl+Z.
        assert!(ws.undo().unwrap());
        assert!(ws.read(plain).unwrap().due().is_none());
        assert!(ws.read(task).unwrap().due().is_none());
    }

    /// Clearing a due over a mixed selection runs `clear_due`, which **marks
    /// first** — so taking the deadline away never takes the task with it.
    #[test]
    fn clearing_a_due_keeps_the_task() {
        use koru_core::attributes::KEY_DUE;
        let (mut ws, mut e) = doc_editor(&["plain"]);
        let row = e.body.rows[0].id;
        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        let nodes = vec![row];
        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::SetAttr(nodes.clone(), KEY_DUE.to_owned(), AttrValue::Date(due)),
        );
        assert!(koru_core::tasks::is_task_node(&ws, row).unwrap());

        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::ClearAttr(nodes, KEY_DUE.to_owned()),
        );
        assert!(ws.read(row).unwrap().due().is_none(), "the deadline went");
        assert!(
            koru_core::tasks::is_task_node(&ws, row).unwrap(),
            "and the task stayed"
        );
    }

    // --------------------------------------------------------- `/task_list`

    /// `/task_list` opens the list picker over the row with the workspace's
    /// lists in it; choosing one makes the row a task if it is not one and
    /// puts it in the list — the marker, the membership and the typed trigger
    /// one undo step, and one intent owed.
    #[test]
    fn slash_task_list_makes_the_row_a_task_in_the_chosen_list() {
        let (mut ws, mut e) = doc_editor(&["buy balloons"]);
        let node = e.body.rows[0].id;
        let birthday = koru_core::ids::ListId::new();
        e.view.lists.replace_named([
            (
                birthday,
                "Birthday Tasks".to_owned(),
                Some("pink".to_owned()),
            ),
            (
                koru_core::ids::ListId::new(),
                "Bike Project".to_owned(),
                None,
            ),
        ]);
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        ws.enable_undo();
        type_str(&mut e, &mut ws, " /task_list");
        assert!(matches!(&e.view.overlay, Overlay::Command(_)));
        enter(&mut e, &mut ws);
        let Overlay::List(picker) = &e.view.overlay else {
            panic!(
                "the list picker opened, got {}",
                overlay_name(&e.view.overlay)
            )
        };
        assert_eq!(picker.table.len(), 2, "the workspace's lists");
        assert!(
            picker.current.is_none(),
            "not in a list yet, so no Clear row"
        );
        assert_eq!(picker.row_count(), 2);
        enter(&mut e, &mut ws);

        let read = ws.read(node).unwrap();
        assert_eq!(
            koru_core::tasks::TaskFields::of(&read).marker,
            Some(koru_core::attributes::TaskReason::Explicit),
            "a list holds tasks, so the row became one"
        );
        assert_eq!(current_list(&ws, Some(node)), Some(birthday));
        assert_eq!(read.content.as_text(), Some("buy balloons "));
        assert!(matches!(e.view.overlay, Overlay::None));
        assert_eq!(
            e.body.take_task_activity().len(),
            1,
            "one intent: the marker"
        );

        // One Ctrl+Z: no task, no list, and the typed command back.
        assert!(ws.undo().unwrap());
        let read = ws.read(node).unwrap();
        assert!(!koru_core::tasks::is_task(
            &koru_core::tasks::TaskFields::of(&read)
        ));
        assert_eq!(current_list(&ws, Some(node)), None);
        assert_eq!(read.content.as_text(), Some("buy balloons /"));

        // Opened again over a row in a list: that row is checked and Clear leads.
        ws.redo().unwrap();
        e.body.rebuild(&ws);
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        type_str(&mut e, &mut ws, "/task_list");
        enter(&mut e, &mut ws);
        let Overlay::List(picker) = &e.view.overlay else {
            panic!("the list picker")
        };
        assert_eq!(picker.current, Some(birthday));
        assert!(picker.offers_clear());
        assert_eq!(picker.sel, 1, "Birthday Tasks, past Clear");
    }

    /// A name no list has goes up to the host with its colour, and the
    /// trigger comes out now — the host makes the list and joins the row.
    #[test]
    fn slash_task_list_hands_a_new_list_up_with_its_colour() {
        let (mut ws, mut e) = doc_editor(&["order the cake"]);
        let node = e.body.rows[0].id;
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        type_str(&mut e, &mut ws, " /task_list");
        enter(&mut e, &mut ws);
        type_str(&mut e, &mut ws, "Wedding");
        enter(&mut e, &mut ws);
        assert!(
            matches!(&e.view.overlay, Overlay::List(p)
                if matches!(&p.stage, list_picker::Stage::Color { name, .. } if name == "Wedding")),
            "the swatches come next"
        );
        enter(&mut e, &mut ws);
        assert_eq!(
            e.view.wants_join_new_list.take(),
            Some((vec![node], "Wedding".to_owned(), Some("red".to_owned()))),
            "no list wears a colour yet, so red is the suggestion"
        );
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("order the cake "),
            "the trigger came out with the ask"
        );
        assert!(
            !koru_core::tasks::is_task_node(&ws, node).unwrap(),
            "the task is made with the join, by the host, once the list exists"
        );
    }

    // --------------------------------------------------------- `/completed`

    /// On a page whose title is the task, `/completed` finishes the page:
    /// the lifecycle turns, the title row reads done, the trigger comes out,
    /// and the same command again reopens it. On a line that is not a task
    /// the row is offered dimmed with the reason.
    #[test]
    fn slash_completed_finishes_the_page_whose_title_is_the_task() {
        let (mut ws, mut e) = doc_editor_on_title("Cake");
        let title = e.body.rows[0].id;
        koru_core::tasks::ensure_task(&mut ws, title, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(ws.text_len(title).unwrap());
        e.reflow();
        type_str(&mut e, &mut ws, " /completed");
        let Overlay::Command(menu) = &e.view.overlay else {
            panic!("the menu")
        };
        assert_eq!(menu.hits, vec![command_menu::Command::Complete]);
        assert_eq!(
            command_menu::disabled(command_menu::Command::Complete, &menu.allows),
            None
        );
        enter(&mut e, &mut ws);
        let read = ws.read(title).unwrap();
        assert_eq!(
            koru_core::tasks::TaskFields::of(&read).lifecycle,
            koru_core::attributes::Lifecycle::Completed
        );
        assert!(read.meta.done, "the mirror the title's strike reads");
        assert_eq!(read.content.as_text(), Some("Cake "));
        assert!(e.body.rows[0].done, "the row reads done");

        // Again: reopened.
        type_str(&mut e, &mut ws, "/completed");
        enter(&mut e, &mut ws);
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(title).unwrap()).lifecycle,
            koru_core::attributes::Lifecycle::Open
        );

        // A plain paragraph: listed, dimmed, with the reason.
        let (mut ws, mut e) = doc_editor(&["plain words"]);
        let node = e.body.rows[0].id;
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        type_str(&mut e, &mut ws, " /completed");
        let Overlay::Command(menu) = &e.view.overlay else {
            panic!("the menu")
        };
        assert_eq!(
            command_menu::disabled(command_menu::Command::Complete, &menu.allows),
            Some("Not a task")
        );
    }

    // -------------------------------------------------------------- `/task`
    //
    // The command's whole job is `ensure_task(Explicit)` in the undo group that
    // takes the typed trigger out. What these pin is that it is *in place* —
    // kind, position and content unchanged (SPEC §1) — and that the gate has
    // two halves: the menu dims the row, and the model refuses beneath it.

    /// `/task` marks the row and changes nothing else about it.
    #[test]
    fn slash_task_marks_the_row_with_reason_explicit() {
        let (mut ws, mut e) = doc_editor(&["write the report"]);
        let node = e.body.rows[0].id;
        let siblings = ws.children(ws.parent(node).unwrap()).unwrap();
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        type_str(&mut e, &mut ws, " /task");
        assert!(
            matches!(&e.view.overlay, Overlay::Command(_)),
            "the menu did not arm"
        );
        enter(&mut e, &mut ws);

        let read = ws.read(node).unwrap();
        assert_eq!(
            koru_core::tasks::TaskFields::of(&read).marker,
            Some(koru_core::attributes::TaskReason::Explicit)
        );
        // In place: same node, same kind, same position, same text — and the
        // typed command is gone rather than left behind as content.
        assert_eq!(read.kind, Kind::Paragraph);
        // The trigger span is the `/` onwards — the query is typed into the
        // panel's own field, never into the document — so what comes back out
        // is one character, and the space before it is the user's.
        assert_eq!(read.content.as_text(), Some("write the report "));
        assert_eq!(ws.children(ws.parent(node).unwrap()).unwrap(), siblings);
        assert!(matches!(e.view.overlay, Overlay::None), "the menu closed");

        // One gesture, one intent — the marker, with the reason that made it.
        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1);
        assert!(matches!(
            owed[0].intents.first(),
            Some(koru_core::tasks::ActivityIntent::Created {
                reason: koru_core::attributes::TaskReason::Explicit,
                node_kind: Kind::Paragraph,
                ..
            })
        ));

        // **One undo step for both writes.** `commit_attribute` composes the
        // command with the trigger deletion, so Ctrl+Z takes the marker off
        // *and* puts the typing back — not one, then the other.
        assert!(ws.undo().unwrap());
        assert!(!koru_core::tasks::is_task_node(&ws, node).unwrap());
    }

    /// **A page is promoted through its title row** — which is an ordinary row
    /// carrying the document's root node (ADR-210), so `/task` needs no special
    /// case and the menu's `attrs` gate already lets it open there.
    #[test]
    fn slash_task_on_a_title_row_promotes_the_page() {
        let (mut ws, mut e) = doc_editor_on_title("Quarterly review");
        let root = e.body.rows[0].id;
        assert_eq!(ws.read(root).unwrap().kind, Kind::Document);
        type_str(&mut e, &mut ws, " /task");
        let Overlay::Command(menu) = &e.view.overlay else {
            panic!("the command menu does not open on a title");
        };
        assert!(
            menu.allows.task_capable,
            "a document root is task-capable, so the row must not be dim"
        );
        enter(&mut e, &mut ws);

        let read = ws.read(root).unwrap();
        assert_eq!(
            koru_core::tasks::TaskFields::of(&read).marker,
            Some(koru_core::attributes::TaskReason::Explicit)
        );
        assert_eq!(read.kind, Kind::Document, "the page is still a page");
        assert_eq!(read.content.as_text(), Some("Quarterly review "));
    }

    /// **Both halves of the kind gate, on the same row.** The menu dims `/task`
    /// over a quote and prints why; and the model refuses even when the menu is
    /// bypassed, which is the half that has to hold when a second surface
    /// forgets to ask ([ADR-285]).
    ///
    /// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
    #[test]
    fn task_is_dimmed_and_refused_on_a_kind_that_cannot_be_one() {
        let (mut ws, mut e) = doc_editor(&["quoted"]);
        let node = e.body.rows[0].id;
        ws.set_kind(node, Kind::Quote).unwrap();
        e.body.rebuild(&ws);
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();

        type_str(&mut e, &mut ws, " /task");
        let Overlay::Command(menu) = &e.view.overlay else {
            panic!("the command menu did not arm on a quote");
        };
        assert!(
            !menu.allows.task_capable,
            "a quote is outside ADR-272's closed set"
        );
        assert!(
            menu.hits.contains(&command_menu::Command::Task),
            "listed and dim, not filtered out"
        );
        assert_eq!(
            command_menu::disabled(command_menu::Command::Task, &menu.allows),
            Some("Can't be a task")
        );

        // Choosing it does nothing and leaves the menu up, so the reason stays
        // on screen — and the typed text is untouched.
        enter(&mut e, &mut ws);
        assert!(matches!(e.view.overlay, Overlay::Command(_)));
        assert!(!koru_core::tasks::is_task_node(&ws, node).unwrap());

        // **And the model refuses underneath.** The command is called directly,
        // as a surface that never asked the menu would call it.
        assert!(matches!(
            koru_core::tasks::ensure_task(
                &mut ws,
                node,
                koru_core::attributes::TaskReason::Explicit
            ),
            Err(koru_core::schema::StoreError::TaskRejected {
                why: koru_core::tasks::TaskRefusal::KindNotCapable,
                ..
            })
        ));
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("quoted /"),
            "the typed `/` is still there — a refusal takes nothing out, and \
             the menu is still open over it"
        );
    }

    /// **The four attribute doors already work on an image row**, through the
    /// same per-node routing a paragraph gets — so there is no image-shaped
    /// picker, no image-shaped menu, and nothing for `/task`'s surface to add
    /// here beyond the row itself.
    ///
    /// Pinned rather than built, because the thing that could quietly break it
    /// is a future gate that reads "does this row have text" and gets it wrong:
    /// an image has none, and a due, a tag, a colour and a priority are all
    /// node state that never touches the content.
    #[test]
    fn the_attribute_doors_reach_an_image_row() {
        use koru_core::attributes::{KEY_DUE, KEY_PRIORITY};
        use koru_core::node::BlobHash;
        let (mut ws, mut e) = doc_editor(&["beside it"]);
        let para = e.body.rows[0].id;
        let doc = ws.parent(para).unwrap();
        let image = ws
            .insert(
                doc,
                Node::image(BlobHash("h".into()), "image/png", Some(1), Some(1)).unwrap(),
            )
            .unwrap();
        e.body.rebuild(&ws);
        e.body.take_task_activity();

        // Kind::Image is in ADR-272's closed set, so a due lands and makes it a
        // task with the deadline as the reason.
        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::SetAttr(vec![para, image], KEY_DUE.to_owned(), AttrValue::Date(due)),
        );
        assert_eq!(
            ws.read(image).unwrap().due(),
            Some(koru_core::time::Temporal::Date(due))
        );
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(image).unwrap()).marker,
            Some(koru_core::attributes::TaskReason::Due)
        );

        // Priority takes the ordinary door on it, exactly as on the paragraph,
        // and creates nothing on its own.
        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::SetAttr(
                vec![image],
                KEY_PRIORITY.to_owned(),
                AttrValue::Text("high".into()),
            ),
        );
        assert_eq!(
            ws.read(image).unwrap().priority(),
            Some(koru_core::attributes::Priority::High)
        );

        // And the whole closed set is what says a file row would answer the
        // same — a file is the root of its own document, so it is reached from
        // the manifest rather than from a body.
        assert!(koru_core::tasks::task_capable(Kind::File));
        assert!(koru_core::tasks::task_capable(Kind::Image));
    }

    /// Every other non-capable kind SPEC §4 excludes answers the same way, and
    /// it is asked of the model rather than of a menu — this is the door a
    /// bypassed UI still has to hit.
    #[test]
    fn the_excluded_kinds_all_refuse_the_marker() {
        let (mut ws, e) = doc_editor(&["a", "b", "c", "d"]);
        let nodes: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        for (node, kind) in
            nodes
                .iter()
                .zip([Kind::Quote, Kind::Code, Kind::Table, Kind::ListBlock])
        {
            ws.set_kind(*node, kind).unwrap();
            assert!(
                koru_core::tasks::ensure_task(
                    &mut ws,
                    *node,
                    koru_core::attributes::TaskReason::Explicit
                )
                .is_err(),
                "{kind:?} accepted a task marker"
            );
            assert!(!koru_core::tasks::is_task_node(&ws, *node).unwrap());
        }
    }

    /// **Setting priority never creates a task** (SPEC §1). On a row that is
    /// already one it takes the ordinary door and owes a
    /// `task.priority.changed`; on a plain row it takes the same door and owes
    /// nothing at all.
    #[test]
    fn priority_owes_activity_only_where_the_target_is_already_a_task() {
        use koru_core::attributes::KEY_PRIORITY;
        let (mut ws, mut e) = doc_editor(&["plain", "already a task"]);
        let nodes: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        let (plain, task) = (nodes[0], nodes[1]);
        koru_core::tasks::ensure_task(&mut ws, task, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
        e.body.rebuild(&ws);
        e.body.take_task_activity();

        e.attribute_outcome(
            &mut ws,
            None,
            &Outcome::SetAttr(
                nodes,
                KEY_PRIORITY.to_owned(),
                AttrValue::Text("high".into()),
            ),
        );

        assert_eq!(
            ws.read(plain).unwrap().priority(),
            Some(koru_core::attributes::Priority::High)
        );
        assert!(
            !koru_core::tasks::is_task_node(&ws, plain).unwrap(),
            "priority created nothing"
        );
        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1, "one intent, for the row that was a task");
        assert!(matches!(
            owed[0].intents.first(),
            Some(koru_core::tasks::ActivityIntent::PriorityChanged {
                change: koru_core::tasks::AttrChange::Set,
                priority: Some(koru_core::attributes::Priority::High),
                ..
            })
        ));
    }

    /// A menu-path due apply covers the whole selection in one undo step —
    /// the multi-select contract A2 set, inherited by A3/A4.
    #[test]
    fn a_menu_due_apply_covers_the_selection_in_one_step() {
        use koru_core::attributes::KEY_DUE;
        let (mut ws, mut e) = doc_editor(&["one", "two", "three"]);
        let nodes: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        assert!(e.open_due_picker_over(&ws, nodes.clone()));
        let trigger = e.view.overlay.trigger();
        ws.enable_undo();
        let due = koru_core::time::DateOnly::parse("2026-09-04").unwrap();
        e.attribute_outcome(
            &mut ws,
            trigger,
            &Outcome::SetAttr(nodes.clone(), KEY_DUE.to_owned(), AttrValue::Date(due)),
        );
        for &n in &nodes {
            assert_eq!(
                ws.read(n).unwrap().due(),
                Some(koru_core::time::Temporal::Date(due))
            );
        }
        assert!(ws.undo().unwrap());
        for &n in &nodes {
            assert!(
                ws.read(n).unwrap().due().is_none(),
                "one Ctrl+Z takes the whole apply back"
            );
        }
    }

    // --------------------------------------------- the move reopen matrix
    //
    // SPEC §5: *adding or moving* an unfinished structural child under a
    // completed task reopens it, reason `UNFINISHED_CHILD_ADDED`. The add half
    // is proved at the store boundary (`apps/koru-store-server/tests/tasks.rs`);
    // this is the move half, driven through the gesture that performs one — a
    // grip drag, landing on a row inside the completed task so the drop
    // re-parents rather than merely reordering.

    /// A completed task with one child, and a loose sibling to drag into it.
    /// Non-flat, because re-parenting is what makes a drop a move at all.
    fn a_completed_task_and_a_sibling(sibling: Node) -> (WorkspaceDoc, Editor, NodeId, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let parent = ws
            .insert(Parent::Root, Node::task("ship it").unwrap())
            .unwrap();
        ws.insert(Parent::Node(parent), Node::paragraph("a note").unwrap())
            .unwrap();
        let loose = ws.insert(Parent::Root, sibling).unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        (ws, e, parent, loose)
    }

    /// Complete `node` without cascading over anything below it — the residue
    /// state a drop is about to land in. Written through the restore door
    /// because `complete_task` would tick the children this matrix is testing.
    fn stand_completed(ws: &mut WorkspaceDoc, node: NodeId) {
        use koru_core::attributes::{Lifecycle, TaskReason};
        koru_core::tasks::ensure_task(ws, node, TaskReason::Explicit).unwrap();
        koru_core::tasks::faithful_import::restore_task(
            ws,
            node,
            &koru_core::tasks::TaskFields {
                marker: Some(TaskReason::Explicit),
                due: None,
                lifecycle: Lifecycle::Completed,
                done: false,
                completed_at: None,
                pinned_today: false,
            },
        )
        .unwrap();
    }

    fn lifecycle(ws: &WorkspaceDoc, node: NodeId) -> koru_core::attributes::Lifecycle {
        koru_core::tasks::TaskFields::of(&ws.read(node).unwrap()).lifecycle
    }

    /// **Dragging an open task under a completed one reopens it**, and says why
    /// — told as a *move*, so the feed reads `task.child.moved`.
    #[test]
    fn moving_an_unfinished_child_under_a_completed_task_reopens_it() {
        use koru_core::attributes::{Lifecycle, TaskReason};
        use koru_core::tasks::{ActivityIntent, ChildClass, LifecycleReason};
        let (mut ws, mut e, parent, loose) =
            a_completed_task_and_a_sibling(Node::task("draft the summary").unwrap());
        koru_core::tasks::ensure_task(&mut ws, loose, TaskReason::Explicit).unwrap();
        stand_completed(&mut ws, parent);
        e.body.rebuild(&ws);
        e.body.take_task_activity();
        assert_eq!(lifecycle(&ws, parent), Lifecycle::Completed);

        // Row 2 is the loose task; row 1 is the note inside `parent`, so the
        // drop takes the note's parent.
        e.drop_row(&mut ws, 2, 1);

        assert!(
            matches!(ws.parent(loose), Ok(Parent::Node(p)) if p == parent),
            "it landed inside the completed task"
        );
        assert_eq!(
            lifecycle(&ws, parent),
            Lifecycle::Open,
            "which reopened it (SPEC §5)"
        );
        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1, "one gesture, one causation id");
        assert!(
            owed[0].intents.iter().any(|i| matches!(
                i,
                ActivityIntent::Reopened {
                    reason: LifecycleReason::UnfinishedChildAdded,
                    ..
                }
            )),
            "and it says why: {:?}",
            owed[0].intents
        );
        assert!(
            owed[0].intents.iter().any(|i| matches!(
                i,
                ActivityIntent::ChildMoved {
                    class: ChildClass::UnfinishedStructural,
                    reopened: true,
                    ..
                }
            )),
            "told as a move, not an insert: {:?}",
            owed[0].intents
        );
    }

    /// **An already-completed child reopens nothing.** It is structural, and it
    /// is finished — the second of `classify_child`'s four rules.
    #[test]
    fn moving_a_completed_child_under_a_completed_task_reopens_nothing() {
        use koru_core::attributes::Lifecycle;
        let (mut ws, mut e, parent, loose) =
            a_completed_task_and_a_sibling(Node::task("already done").unwrap());
        stand_completed(&mut ws, loose);
        stand_completed(&mut ws, parent);
        e.body.rebuild(&ws);
        e.body.take_task_activity();

        e.drop_row(&mut ws, 2, 1);

        assert!(matches!(ws.parent(loose), Ok(Parent::Node(p)) if p == parent));
        assert_eq!(
            lifecycle(&ws, parent),
            Lifecycle::Completed,
            "it stayed completed"
        );
        assert!(
            e.body.take_task_activity().is_empty(),
            "and owed nothing at all"
        );
    }

    /// **Passive content reopens nothing.** A note is not work; moving one into
    /// a finished task is filing, not reopening.
    #[test]
    fn moving_passive_content_under_a_completed_task_reopens_nothing() {
        use koru_core::attributes::Lifecycle;
        let (mut ws, mut e, parent, loose) =
            a_completed_task_and_a_sibling(Node::paragraph("for the record").unwrap());
        stand_completed(&mut ws, parent);
        e.body.rebuild(&ws);
        e.body.take_task_activity();

        e.drop_row(&mut ws, 2, 1);

        assert!(matches!(ws.parent(loose), Ok(Parent::Node(p)) if p == parent));
        assert_eq!(lifecycle(&ws, parent), Lifecycle::Completed);
        assert!(e.body.take_task_activity().is_empty());
    }

    /// An ordinary reorder — nothing completed anywhere above — costs no
    /// activity and no undo step of its own. This is every drag in a document
    /// that has no tasks in it, and it is the case the hook must stay cheap in.
    #[test]
    fn an_ordinary_reorder_owes_nothing() {
        let (mut ws, mut e, _parent, loose) =
            a_completed_task_and_a_sibling(Node::task("draft the summary").unwrap());
        e.body.rebuild(&ws);
        e.body.take_task_activity();

        e.drop_row(&mut ws, 2, 1);

        assert!(e.body.take_task_activity().is_empty());
        assert!(
            ws.undo().unwrap(),
            "one Ctrl+Z, and it is the move — nothing was grouped in front of it"
        );
        assert!(matches!(ws.parent(loose), Ok(Parent::Root)));
    }

    // ------------------------------------------------- chips are clickable

    /// Where a row's chips are, in the coordinates `chip_boxes` answers in.
    /// The reflow is what puts a layout under them: without one the last line
    /// has no advance and every chip sits at the row's text origin.
    fn chip_rects(e: &mut Editor, row: usize) -> Vec<crate::chips::ChipBox> {
        e.reflow();
        let (body, view) = e.parts();
        crate::chips::chip_boxes(&body.rows[row], &mut view.engine, &view.tags, &view.lists)
    }

    /// A point inside chip `chip` of row `row`, in the viewport coordinates a
    /// press arrives in — `content_x` back on, the scroll back off.
    fn on_chip(e: &mut Editor, row: usize, chip: usize) -> (f32, f32) {
        let r = chip_rects(e, row)[chip].rect;
        (
            e.view.content_x + r.x + r.w * 0.3,
            r.y + r.h * 0.5 - e.view.scroll,
        )
    }

    /// A point on a chip's remove lane. Every kind has one — except a dateless
    /// task's due chip, which has nothing to clear and offers none
    /// (`chips::ChipBox::close`).
    fn on_chip_close(e: &mut Editor, row: usize, chip: usize) -> (f32, f32) {
        let r = chip_rects(e, row)[chip]
            .close
            .expect("every chip has a remove lane");
        (
            e.view.content_x + r.x + r.w * 0.5,
            r.y + r.h * 0.5 - e.view.scroll,
        )
    }

    /// One row carrying every kind of chip, in display order: the tags, then
    /// due, priority and the colour swatch.
    fn row_with_every_chip(
        names: &[&str],
    ) -> (WorkspaceDoc, Editor, NodeId, Vec<koru_core::ids::TagId>) {
        use koru_core::attributes::{KEY_COLOR, KEY_DUE, KEY_PRIORITY};
        let (mut ws, mut e) = doc_editor(&["task"]);
        let ids = seed_tags(&mut e, names);
        let node = e.body.focused().unwrap().id;
        for &id in &ids {
            ws.add_tag(node, id).unwrap();
        }
        ws.set_attribute(
            node,
            KEY_DUE,
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-09-04").unwrap()),
        )
        .unwrap();
        ws.set_attribute(node, KEY_PRIORITY, AttrValue::Text("high".to_owned()))
            .unwrap();
        ws.set_attribute(node, KEY_COLOR, AttrValue::Text("teal".to_owned()))
            .unwrap();
        e.body.refresh(&ws, node);
        e.reflow();
        (ws, e, node, ids)
    }

    /// **Every chip takes the pointer off the text, and nothing beside them
    /// does.** Koru does not use the hand cursor (`docs/features/design/DESIGN.md`), so what a
    /// chip changes is that it stops being I-beam: hover goes through the same
    /// `chips::chip_at` the press does, so a chip that opens a picker can never
    /// also be a stretch of caret-able text, and the gap either side of the run
    /// still is.
    #[test]
    fn hovering_a_chip_of_any_kind_takes_the_pointer_off_the_text() {
        let (_ws, mut e, _node, _ids) = row_with_every_chip(&["mvp"]);
        let boxes = chip_rects(&mut e, 0);
        assert_eq!(boxes.len(), 4, "tag, due, priority, swatch");

        for i in 0..boxes.len() {
            let kind = boxes[i].kind;
            let (x, y) = on_chip(&mut e, 0, i);
            e.mouse_move(x, y);
            assert_eq!(
                e.view.hover,
                Some(Hover::Chip { row: 0, chip: i }),
                "chip {i} ({kind:?})"
            );
            assert_eq!(
                e.view.cursor_icon(),
                koru_ui::cursor::PRESS,
                "chip {i} ({kind:?})"
            );
        }

        // The gap before the first chip is the row's own text.
        let first = boxes[0].rect;
        e.mouse_move(
            e.view.content_x + first.x - 4.0,
            first.y + first.h * 0.5 - e.view.scroll,
        );
        assert!(
            !matches!(e.view.hover, Some(Hover::Chip { .. })),
            "the gap before the chips is not a chip"
        );
        assert_eq!(e.view.cursor_icon(), winit::window::CursorIcon::Text);

        // Past the last one is nothing either.
        let last = boxes[3].rect;
        e.mouse_move(
            e.view.content_x + last.x + last.w + 8.0,
            last.y + last.h * 0.5 - e.view.scroll,
        );
        assert!(!matches!(e.view.hover, Some(Hover::Chip { .. })));
    }

    /// Clicking a **value** chip opens that attribute's picker over that
    /// chip's node — the same panel the `/`-command opens, seeded the same way
    /// — and the panel is anchored to the chip rather than to a caret.
    ///
    /// The due chip is not in this list any more: it is the task marker, not a
    /// value on a row, and its click goes to the task. See
    /// `clicking_the_due_chip_goes_to_the_task_and_edit_still_opens_the_grid`.
    #[test]
    fn clicking_each_value_chip_opens_its_own_picker() {
        let (mut ws, mut e, node, ids) = row_with_every_chip(&["alpha", "beta"]);
        for (name, chip) in [("Tag", 0usize), ("Tag", 1), ("Priority", 3), ("Color", 4)] {
            e.view.overlay = Overlay::None;
            let (x, y) = on_chip(&mut e, 0, chip);
            assert!(e.mouse_down(&mut ws, x, y, false));
            assert_eq!(
                overlay_name(&e.view.overlay),
                name,
                "chip {chip} should open the {name} picker"
            );
            assert_eq!(
                e.view.overlay.trigger().map(|t| t.node),
                Some(node),
                "and over the chip's own node"
            );
            assert_eq!(
                e.view.overlay.trigger().map(|t| t.anchor),
                Some(Anchor::Chip),
                "anchored to the chip, not to a caret"
            );
            assert!(e.view.chip_anchor.is_some(), "and the rect is recorded");
        }

        // The tag chooser opens on the tag that was pressed, so the keyboard
        // is already where the pointer was. Which tag each chip *is* comes
        // from the row rather than from `ids`: chips are in attribute order
        // and the chooser lists by name, and neither is the seeding order.
        assert_eq!(ids.len(), 2);
        for chip in [0usize, 1] {
            let crate::chips::ChipSpec::Tag(tag) = e.body.rows[0].chips[chip] else {
                panic!("chip {chip} should be a tag");
            };
            e.view.overlay = Overlay::None;
            let (x, y) = on_chip(&mut e, 0, chip);
            e.mouse_down(&mut ws, x, y, false);
            let Overlay::Tag(picker) = &e.view.overlay else {
                panic!("expected the tag chooser");
            };
            let selected = picker.table[picker.hits[picker.sel]].id;
            assert_eq!(selected, tag, "chip {chip} selects its own tag's row");
        }
    }

    /// **With the Tasks view hidden, the due chip opens its date picker** and
    /// asks for no Tasks view — there is none to go to (koru-features).
    #[test]
    fn with_tasks_hidden_the_due_chip_opens_its_picker_instead_of_the_task() {
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            let (mut ws, mut e, _node, _) = row_with_every_chip(&["alpha", "beta"]);
            let due_chip = 2usize;
            e.view.overlay = Overlay::None;
            e.view.wants_task_view = None;
            let (x, y) = on_chip(&mut e, 0, due_chip);
            assert!(e.mouse_down(&mut ws, x, y, false));
            assert_eq!(e.view.wants_task_view, None, "no Tasks view is asked for");
            assert_ne!(
                overlay_name(&e.view.overlay),
                "None",
                "the date picker opens instead"
            );
        });
    }

    /// **The due chip's click goes to the task; its menu still opens the grid.**
    ///
    /// The chip is the task marker (SPEC §4), so the most useful thing behind
    /// it is the task itself — the same place `Ctrl+Shift+E` lands. Editing the
    /// date moved one door along, into the chip's menu, and `edit_chip` is that
    /// door: a *Edit due date…* row that stopped opening the date grid would be
    /// the whole change made worthless.
    #[test]
    fn clicking_the_due_chip_goes_to_the_task_and_edit_still_opens_the_grid() {
        let (mut ws, mut e, node, _) = row_with_every_chip(&["alpha", "beta"]);
        let due_chip = 2usize;

        e.view.overlay = Overlay::None;
        e.view.wants_task_view = None;
        let (x, y) = on_chip(&mut e, 0, due_chip);
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(
            e.view.wants_task_view,
            Some(node),
            "the press asks the container for this row's task"
        );
        assert_eq!(
            overlay_name(&e.view.overlay),
            "None",
            "and opens no picker on the way"
        );

        // The menu's *Edit* row, which is this call.
        let hit = e.chip_at_point(0, x - e.view.content_x, y + e.view.scroll);
        let hit = hit.expect("the due chip");
        assert!(Editing::edit_chip(&mut e, &mut ws, 0, hit));
        assert_eq!(overlay_name(&e.view.overlay), "Date");
        assert_eq!(e.view.overlay.trigger().map(|t| t.node), Some(node));
    }

    /// **`/task` puts the due chip on the row, dashed, and pressing it opens
    /// the date picker** — the task's indicator *is* the due chip (SPEC §4),
    /// so the gesture that makes a task and the control that gives it a
    /// deadline are one element. Two halves are pinned here that the chip
    /// module cannot see on its own: the chip is in the row's cached specs the
    /// moment the command commits (the row is refreshed inside the same undo
    /// group), and the press arm routes on the chip's **kind** rather than its
    /// value, so a chip with no date still reaches `open_due_chooser`.
    #[test]
    fn slash_task_shows_the_dashed_due_chip_and_it_opens_the_date_picker() {
        let (mut ws, mut e) = doc_editor(&["write the report"]);
        let node = e.body.rows[0].id;
        e.body.focus_on(node);
        e.place_caret(ws.text_len(node).unwrap());
        e.reflow();
        assert!(e.body.rows[0].chips.is_empty(), "a plain row has no chips");

        type_str(&mut e, &mut ws, " /task");
        enter(&mut e, &mut ws);
        assert_eq!(
            e.body.rows[0].chips,
            vec![crate::chips::ChipSpec::Due(None)],
            "the chip is there without a second refresh"
        );
        let boxes = chip_rects(&mut e, 0);
        assert_eq!(boxes[0].kind, koru_draw::chip::ChipKind::Due);
        assert_eq!(boxes[0].label, crate::chips::DUE_NONE);
        assert!(
            boxes[0].close.is_some(),
            "and a remove lane, because there is still a task to take off"
        );

        // **The label half asks for the task** — the dateless chip reaching the
        // same arm as a dated one is the point, because the press routes on the
        // chip's *kind* rather than on its value. The trailing lane is the
        // remove lane and is `the_dash_chip_offers_its_remove_lane_like_the_rest`'s.
        {
            e.view.overlay = Overlay::None;
            e.view.wants_task_view = None;
            let r = chip_rects(&mut e, 0)[0].rect;
            let (x, y) = (
                e.view.content_x + r.x + r.w * 0.3,
                r.y + r.h * 0.5 - e.view.scroll,
            );
            e.mouse_move(x, y);
            assert_eq!(e.view.cursor_icon(), koru_ui::cursor::PRESS);
            assert!(e.mouse_down(&mut ws, x, y, false));
            assert_eq!(e.view.wants_task_view, Some(node));
            assert_eq!(overlay_name(&e.view.overlay), "None");
            assert!(
                koru_core::tasks::is_task_node(&ws, node).unwrap(),
                "and pressing it never took the task off by itself"
            );
        }

        // The date grid is still one door away, which is the row the chip's
        // menu draws.
        let r = chip_rects(&mut e, 0)[0].rect;
        let hit = e
            .chip_at_point(0, r.x + r.w * 0.3, r.y + r.h * 0.5)
            .expect("the due chip");
        assert!(Editing::edit_chip(&mut e, &mut ws, 0, hit));
        assert_eq!(overlay_name(&e.view.overlay), "Date");
        assert_eq!(e.view.overlay.trigger().map(|t| t.node), Some(node));
        assert_eq!(
            e.view.overlay.trigger().map(|t| t.anchor),
            Some(Anchor::Chip)
        );
    }

    /// **A press on a chip is not a press on the line.** The caret stays where
    /// it was and no selection starts — a chip is chrome hanging off the row,
    /// and moving the insertion point every time somebody edited a due date is
    /// the bug this guards.
    #[test]
    fn clicking_a_chip_leaves_the_caret_alone() {
        let (mut ws, mut e, _node, _ids) = row_with_every_chip(&["mvp"]);
        e.place_caret(2);
        e.reflow();
        let before = e.body.focused().unwrap().caret();

        let (x, y) = on_chip(&mut e, 0, 1);
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(e.body.focused().unwrap().caret(), before, "the caret held");
        assert!(!e.view.dragging, "and no drag was armed");
    }

    /// **A chip on the document's own title row is clickable too** —
    /// attributes belong on a document as much as on a block ([ADR-187]), and
    /// the hit test knows nothing about which row it is on.
    ///
    /// [ADR-187]: ../../../docs/decisions/ADR-201-225.md#adr-210-titleattrs--a-title-row-takes-attributes-but-not-pills-and-date-is-gone
    #[test]
    fn a_chip_on_the_title_row_is_clickable() {
        let (mut ws, mut e) = doc_editor_on_title("Spec");
        let ids = seed_tags(&mut e, &["mvp"]);
        let title = e.body.rows[0].id;
        assert!(e.body.is_title_row(0));
        ws.add_tag(title, ids[0]).unwrap();
        ws.set_attribute(
            title,
            koru_core::attributes::KEY_PRIORITY,
            AttrValue::Text("high".to_owned()),
        )
        .unwrap();
        e.body.refresh(&ws, title);
        e.reflow();
        assert_eq!(e.body.rows[0].chips.len(), 2);

        let (x, y) = on_chip(&mut e, 0, 1);
        e.mouse_move(x, y);
        assert_eq!(e.view.cursor_icon(), koru_ui::cursor::PRESS);
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(overlay_name(&e.view.overlay), "Priority");
        assert_eq!(e.view.overlay.trigger().map(|t| t.node), Some(title));
    }

    /// **The tag chip's remove lane takes that one tag off**, and its
    /// neighbours are untouched. One undo step puts it back, and the label half
    /// of the same chip still opens the picker — two regions of one rectangle,
    /// both out of the one geometry function ([ADR-188]).
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    #[test]
    fn the_tag_chips_close_removes_that_tag_only() {
        let (mut ws, mut e, node, ids) = row_with_every_chip(&["alpha", "beta"]);
        ws.enable_undo();
        // Which tag chip 0 *is* comes from the row: chips are in attribute
        // order, which is id order, and not the order `seed_tags` named them.
        assert_eq!(ids.len(), 2);
        let crate::chips::ChipSpec::Tag(pressed) = e.body.rows[0].chips[0] else {
            panic!("chip 0 should be a tag");
        };
        let survivor = *ids.iter().find(|&&id| id != pressed).expect("the other");

        let (x, y) = on_chip_close(&mut e, 0, 0);
        assert!(e.mouse_down(&mut ws, x, y, false));
        let left: Vec<koru_core::ids::TagId> = ws.read(node).unwrap().tag_ids().collect();
        assert_eq!(
            left,
            vec![survivor],
            "the pressed tag went, the other stayed"
        );
        assert!(
            !e.view.overlay.is_open(),
            "and it removes outright rather than opening anything"
        );

        assert!(ws.undo().unwrap());
        e.body.refresh(&ws, node);
        assert_eq!(
            ws.read(node).unwrap().tag_ids().count(),
            2,
            "one Ctrl+Z puts it back"
        );

        // The label half of the chip is still the picker.
        e.reflow();
        let (x, y) = on_chip(&mut e, 0, 0);
        assert!(e.mouse_down(&mut ws, x, y, false));
        assert_eq!(overlay_name(&e.view.overlay), "Tag");
        assert_eq!(
            ws.read(node).unwrap().tag_ids().count(),
            2,
            "and opening it removed nothing"
        );
    }

    /// **Every chip kind has a remove lane, and each one clears only its own
    /// attribute** ([ADR-188], amended 2026-08-27). It was the tag chip's
    /// alone, which made the chip a control for setting an attribute but not
    /// for taking it off — and which of the four could be removed in place was
    /// discoverable only by hovering each.
    ///
    /// Driven through `mouse_down`, not through the outcome, so the geometry is
    /// under test as well as the commit: a lane the hit test cannot reach is
    /// not a lane.
    ///
    /// [ADR-188]: ../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    #[test]
    fn every_chips_close_clears_only_its_own_attribute() {
        use koru_core::attributes::{KEY_COLOR, KEY_DUE, KEY_PRIORITY};

        // The row carries one tag, a due, a priority and a colour: four chips,
        // and clearing any one has to leave the other three standing.
        let set = |ws: &WorkspaceDoc, node: NodeId| -> (usize, bool, bool, bool) {
            let n = ws.read(node).unwrap();
            let has = |key: &str| n.content.attrs().iter().any(|a| a.role == key);
            (
                n.tag_ids().count(),
                has(KEY_DUE),
                has(KEY_PRIORITY),
                has(KEY_COLOR),
            )
        };

        for (chip, after) in [
            (0usize, (0, true, true, true)),
            (1, (1, false, true, true)),
            (2, (1, true, false, true)),
            (3, (1, true, true, false)),
        ] {
            let (mut ws, mut e, node, _ids) = row_with_every_chip(&["mvp"]);
            ws.enable_undo();
            let boxes = chip_rects(&mut e, 0);
            assert_eq!(boxes.len(), 4, "tag, due, priority, swatch");
            for placed in &boxes {
                assert!(
                    placed.close.is_some(),
                    "{:?} reserves a remove lane",
                    placed.kind
                );
            }
            // The swatch's lane sits **to the right of its dot**, not over it:
            // the dot is the half that opens the picker.
            let swatch = &boxes[3];
            assert!(
                swatch.close.unwrap().x >= swatch.rect.x + koru_draw::chip::SWATCH_D,
                "the swatch's `×` is past its dot"
            );

            assert_eq!(set(&ws, node), (1, true, true, true), "chip {chip}: before");
            // **Before the press.** Clearing a tag takes its chip off the row,
            // so `chips[chip]` afterwards is a different chip.
            let due = matches!(e.body.rows[0].chips[chip], crate::chips::ChipSpec::Due(_));
            e.view.wants_remove_from_tasks = None;
            let (x, y) = on_chip_close(&mut e, 0, chip);
            assert!(e.mouse_down(&mut ws, x, y, false));
            // **The due chip's lane asks instead of clearing.** It takes the
            // *task* off now, not the date, and what goes with a task is not
            // visible from the chip — so it names its row and the host raises
            // the confirmation. The document is untouched until that is
            // answered, which is what `before` still holding says.
            let after = if due { (1, true, true, true) } else { after };
            assert_eq!(set(&ws, node), after, "chip {chip}: only that one went");
            assert_eq!(
                e.view.wants_remove_from_tasks == Some(node),
                due,
                "chip {chip}: only the due chip's lane asks"
            );
            assert!(
                !e.view.overlay.is_open(),
                "chip {chip}: it removes outright rather than opening anything"
            );

            if due {
                // Nothing was written, so there is nothing to undo and the
                // label half was covered above.
                continue;
            }
            // One undo step, and the label half is still the chip's own
            // gesture — the picker for a value chip, which is the whole of
            // what the lane is *not*.
            assert!(ws.undo().unwrap());
            e.body.refresh(&ws, node);
            assert_eq!(set(&ws, node), (1, true, true, true), "chip {chip}: undone");
            e.reflow();
            e.view.wants_task_view = None;
            let (x, y) = on_chip(&mut e, 0, chip);
            assert!(e.mouse_down(&mut ws, x, y, false));
            let due = matches!(e.body.rows[0].chips[chip], crate::chips::ChipSpec::Due(_));
            assert_eq!(
                e.view.overlay.is_open(),
                !due,
                "chip {chip}: the label half is still its own gesture"
            );
            assert_eq!(
                e.view.wants_task_view.is_some(),
                due,
                "chip {chip}: and only the due chip asks for the task"
            );
            assert_eq!(
                set(&ws, node),
                (1, true, true, true),
                "chip {chip}: and the label half removed nothing"
            );
        }
    }

    // ------------------------------------- the tag chooser stays open

    /// **Choosing a tag applies it and leaves the chooser up** (ADR-188,
    /// amended twice): tags are the one attribute a block carries several of,
    /// so a visit is usually more than one tag. The row ticks live, a second
    /// choice lands on the same node, choosing a **ticked** row unticks it,
    /// and Escape is what ends the visit.
    ///
    /// The typed `/tag` is taken out by the **first** commit; the second one
    /// must not go looking for it again, which is what the trigger's retirement
    /// to `Anchor::Insert` is for.
    #[test]
    fn the_tag_chooser_stays_open_across_several_choices() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        let ids = seed_tags(&mut e, &["alpha", "beta", "gamma"]);
        assert_eq!(ids.len(), 3);

        type_str(&mut e, &mut ws, "/tag");
        let enter = |e: &mut Editor, ws: &mut WorkspaceDoc| {
            e.key(
                ws,
                &Key::Named(NamedKey::Enter),
                None,
                ModifiersState::empty(),
            );
        };
        enter(&mut e, &mut ws);
        assert!(matches!(e.view.overlay, Overlay::Tag(_)), "the chooser");

        // Row 0 of the unfiltered list, whichever tag that is by name.
        let first = match &e.view.overlay {
            Overlay::Tag(p) => p.table[p.hits[0]].id,
            _ => unreachable!(),
        };
        enter(&mut e, &mut ws);
        let Overlay::Tag(picker) = &e.view.overlay else {
            panic!("the chooser stays open after a choice");
        };
        assert!(picker.applied.contains(&first), "and the row ticks live");
        assert_eq!(picker.query_text(), "", "with the box emptied for the next");
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some(""),
            "the typed /tag went out with the first commit"
        );

        // A second choice, on the same still-open panel.
        e.key(
            &mut ws,
            &Key::Named(NamedKey::ArrowDown),
            None,
            ModifiersState::empty(),
        );
        let second = match &e.view.overlay {
            Overlay::Tag(p) => p.table[p.hits[p.sel]].id,
            _ => unreachable!(),
        };
        assert_ne!(second, first);
        enter(&mut e, &mut ws);
        let left: Vec<koru_core::ids::TagId> = ws.read(node).unwrap().tag_ids().collect();
        assert!(
            left.contains(&first) && left.contains(&second),
            "both tags landed on the node"
        );
        assert!(
            matches!(&e.view.overlay, Overlay::Tag(p) if p.applied.contains(&second)),
            "and it is still open, with the second row ticked"
        );
        assert!(
            e.view.notice.is_none(),
            "the second commit did not go looking for the deleted /tag"
        );

        // Re-arming put the selection back on row 0, which is `first` and is
        // ticked — so choosing it now **takes that tag off** (ADR-188, amended
        // twice) and the panel stays up for the rest of the visit.
        enter(&mut e, &mut ws);
        let left: Vec<koru_core::ids::TagId> = ws.read(node).unwrap().tag_ids().collect();
        assert_eq!(
            left,
            vec![second],
            "the ticked row came off, the other stays"
        );
        assert!(
            matches!(&e.view.overlay, Overlay::Tag(p)
                if !p.applied.contains(&first) && p.applied.contains(&second)),
            "still open, with that row unticked and the other still ticked"
        );

        // Escape ends the visit.
        e.key(
            &mut ws,
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty(),
        );
        assert!(!e.view.overlay.is_open(), "Escape closes it");
    }

    /// A click **outside** the panel closes it — the ordinary dismissal every
    /// overlay already has, reused rather than a second path invented for this
    /// one.
    #[test]
    fn a_click_outside_closes_the_stay_open_tag_chooser() {
        let (mut ws, mut e) = doc_editor(&[""]);
        seed_tags(&mut e, &["alpha", "beta"]);
        type_str(&mut e, &mut ws, "/tag");
        for _ in 0..2 {
            e.key(
                &mut ws,
                &Key::Named(NamedKey::Enter),
                None,
                ModifiersState::empty(),
            );
        }
        assert!(matches!(e.view.overlay, Overlay::Tag(_)));

        let panel = e.overlay_panel_rect().expect("the panel is placed");
        e.mouse_down(&mut ws, panel.x + panel.w + 40.0, panel.y - 40.0, false);
        assert!(!e.view.overlay.is_open(), "a press outside dismisses it");
    }

    /// **The `#tag` grammar still commits and closes.** Its query is the text
    /// behind the panel and the commit is what takes that text out; leaving it
    /// open would hang a picker over prose being typed into, re-filtering
    /// against a `#home` that is no longer there.
    #[test]
    fn the_hash_tag_path_still_closes_on_commit() {
        let (mut ws, mut e) = doc_editor(&[""]);
        let node = e.body.focused().unwrap().id;
        let ids = seed_tags(&mut e, &["mvp"]);
        // Mid-line: `#` at the start of a row is the heading shorthand's.
        type_str(&mut e, &mut ws, "x #mv");
        let Overlay::Tag(picker) = &e.view.overlay else {
            panic!("the `#` form opens");
        };
        assert!(!picker.has_search(), "and it has no box of its own");

        e.key(
            &mut ws,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty(),
        );
        assert!(!e.view.overlay.is_open(), "it commits and closes");
        assert_eq!(
            ws.read(node).unwrap().tag_ids().collect::<Vec<_>>(),
            ids,
            "with the tag applied"
        );
        assert_eq!(
            ws.read(node).unwrap().content.as_text(),
            Some("x "),
            "and the typed #mv taken back out"
        );
    }

    // ------------------------------------- the unfinished-items indicator

    /// A completed task holding one unchecked checklist item — SPEC §5's
    /// **concurrent residue**, built the way a merge produces it: the parent is
    /// stood completed through the restore door, because `complete_task` would
    /// tick the very child that makes this state interesting.
    fn a_task_with_unfinished_work() -> (WorkspaceDoc, Editor, NodeId, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let parent = ws
            .insert(Parent::Root, Node::task("ship it").unwrap())
            .unwrap();
        let step = ws
            .insert(Parent::Node(parent), Node::paragraph("a step").unwrap())
            .unwrap();
        ws.wrap_in_list_block(step, koru_core::ListStyle::Checkbox)
            .unwrap();
        stand_completed(&mut ws, parent);
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        e.reflow();
        (ws, e, parent, step)
    }

    fn warned(e: &Editor, id: NodeId) -> bool {
        e.body
            .row(id)
            .expect("a row")
            .task
            .is_some_and(|t| t.unfinished_children)
    }

    /// **Exactly the residue state, and nothing else.**
    ///
    /// Four rows out of one document: the completed task holding unfinished
    /// work warns; ticking that work off clears it; a task that is merely *open*
    /// with unfinished work below never warned in the first place; and the
    /// checklist item itself is not a task and carries no flag at all.
    #[test]
    fn the_unfinished_items_flag_marks_only_a_completed_task_still_holding_work() {
        let (mut ws, mut e, parent, step) = a_task_with_unfinished_work();
        assert!(warned(&e, parent), "completed, and a step is not done");
        assert!(
            e.body.row(step).expect("a row").task.is_none(),
            "a checklist item is checkable without being a task"
        );

        // Ticking the step off is an ordinary checkbox, and it is what makes the
        // parent's claim true again.
        e.body.set_done(&mut ws, step, true);
        assert!(!warned(&e, parent), "nothing unfinished is left");

        // And an *open* task with the same work under it says nothing: the
        // indicator is about a completed task's residue, not about having work.
        e.body.set_done(&mut ws, step, false);
        assert!(warned(&e, parent));
        koru_core::tasks::reopen_task(
            &mut ws,
            parent,
            koru_core::tasks::LifecycleReason::ExplicitReopen,
        )
        .unwrap();
        e.body.rebuild(&ws);
        assert!(!warned(&e, parent), "an open task is not a residue");
    }

    /// A remote peer unticking a child arrives as a **map** change, which names
    /// only the node it happened on — so the row above it is refreshed by the
    /// parent-chain pass in `Body::refresh` or not at all. Driven through
    /// `refresh`, which is the path `catch_up` takes for a non-structural
    /// change.
    #[test]
    fn unticking_a_step_re_warns_the_completed_task_above_it() {
        let (mut ws, mut e, parent, step) = a_task_with_unfinished_work();
        e.body.set_done(&mut ws, step, true);
        assert!(!warned(&e, parent));

        ws.set_checkbox(step, false).unwrap();
        e.body.refresh(&ws, step);
        assert!(
            warned(&e, parent),
            "the ancestor's claim is re-asked, not left stale"
        );
    }

    /// **The triangle hit-tests back to where it is drawn**, and stops where it
    /// is drawn too — the negative probes are the half that catches a rect that
    /// grew without its drawing.
    ///
    /// The geometry comes from the one function the renderer calls, converted
    /// the way a press arrives: `content_x` back on, the scroll back off.
    #[test]
    fn the_unfinished_items_triangle_hit_tests_back_to_itself() {
        let (_ws, mut e, parent, _) = a_task_with_unfinished_work();
        let row = e.body.rows.iter().position(|r| r.id == parent).unwrap();
        let rect = {
            let (body, view) = e.parts();
            rowlayout::task_warning_rect(&body.rows[row], &mut view.engine, &view.tags, &view.lists)
                .expect("the residue row is drawn with one")
        };
        let (cx, cy) = (
            e.view.content_x + rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5 - e.view.scroll,
        );
        e.mouse_move(cx, cy);
        assert_eq!(e.view.hover, Some(Hover::TaskWarning(row)));

        // Just off each side, on the same line.
        e.mouse_move(cx - rect.w, cy);
        assert_ne!(e.view.hover, Some(Hover::TaskWarning(row)));
        e.mouse_move(cx + rect.w, cy);
        assert_ne!(e.view.hover, Some(Hover::TaskWarning(row)));

        // And a row with nothing unfinished offers no rect at all, which is what
        // makes the whole thing free on the common row.
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let plain = ws
            .insert(Parent::Root, Node::paragraph("no task here").unwrap())
            .unwrap();
        let mut plain_e = Editor::over(&ws);
        plain_e.set_viewport(900.0, 560.0);
        plain_e.reflow();
        let i = plain_e
            .body
            .rows
            .iter()
            .position(|r| r.id == plain)
            .unwrap();
        let (body, view) = plain_e.parts();
        assert!(
            rowlayout::task_warning_rect(&body.rows[i], &mut view.engine, &view.tags, &view.lists)
                .is_none()
        );
    }

    /// Resting on it says what it means. The glyph alone reads as an error;
    /// the sentence is the only thing that tells anyone a merge did this.
    #[test]
    fn resting_on_the_triangle_raises_its_tooltip() {
        let (_ws, mut e, parent, _) = a_task_with_unfinished_work();
        let row = e.body.rows.iter().position(|r| r.id == parent).unwrap();
        let rect = {
            let (body, view) = e.parts();
            rowlayout::task_warning_rect(&body.rows[row], &mut view.engine, &view.tags, &view.lists)
                .unwrap()
        };
        e.mouse_move(
            e.view.content_x + rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5 - e.view.scroll,
        );
        assert!(
            e.view.task_warning.is_present(),
            "the tooltip clock is running"
        );
        assert_eq!(
            e.view.task_warning.label(),
            Some(TASK_WARNING_TOOLTIP),
            "and it is the sentence, not the glyph again"
        );

        // Off it, the clock lets go.
        e.mouse_move(10.0, rect.y + rect.h * 0.5 - e.view.scroll);
        assert_ne!(e.view.hover, Some(Hover::TaskWarning(row)));
    }

    // ------------------------------------------------- departures (T-0c)
    //
    // `delete_rows` is the structural delete seam — the explicit row delete and
    // the cut of whole rows both come through it — so it is where a task learns
    // a child left it.

    /// **Deleting a structural child tells the task it lost one.** Filed under
    /// the task, carrying the child's id, and nothing else.
    #[test]
    fn deleting_a_row_out_of_a_task_reports_the_departure() {
        let (mut ws, mut e, parent, step) = a_task_with_unfinished_work();
        e.body.task_activity.clear();
        e.body.focus_on(step);

        assert!(e.delete_rows(&mut ws, true), "the row went");

        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1, "one gesture, one chain");
        let [koru_core::tasks::ActivityIntent::ChildRemoved { task, child }] = owed[0].intents[..]
        else {
            panic!("a delete owes exactly the departure: {:?}", owed[0]);
        };
        assert_eq!(child, step);
        assert_eq!(task.node_id(), parent, "filed under the task it left");
    }

    /// **Passive content owes nothing**, and neither does a row under no task —
    /// the two halves of the reading T-0c took from ADR-273's row.
    #[test]
    fn deleting_passive_content_and_plain_rows_owes_nothing() {
        let (mut ws, mut e, parent, _) = a_task_with_unfinished_work();
        let note = ws
            .insert(Parent::Node(parent), Node::paragraph("a note").unwrap())
            .unwrap();
        e.body.rebuild(&ws);
        e.body.task_activity.clear();
        e.body.focus_on(note);
        assert!(e.delete_rows(&mut ws, true));
        assert!(
            e.body.take_task_activity().is_empty(),
            "a note under a task is filing, not work"
        );

        let (mut ws, mut e) = editor_over(&["one", "two"]);
        let row = e.body.rows[1].id;
        e.body.focus_on(row);
        assert!(e.delete_rows(&mut ws, true));
        assert!(
            e.body.take_task_activity().is_empty(),
            "no task above it, so no task lost anything"
        );
    }

    /// **A cut is a departure too.** It comes through the same seam with
    /// `promote = false`, because a cut is a delete followed by a paste
    /// somewhere else — and the paste's own hook reports the arrival.
    #[test]
    fn cutting_a_row_out_of_a_task_reports_the_departure() {
        let (mut ws, mut e, parent, step) = a_task_with_unfinished_work();
        e.body.task_activity.clear();
        e.body.focus_on(step);

        assert!(e.delete_rows(&mut ws, false));

        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1);
        let [koru_core::tasks::ActivityIntent::ChildRemoved { task, child }] = owed[0].intents[..]
        else {
            panic!("a cut owes the departure: {:?}", owed[0]);
        };
        assert_eq!(child, step);
        assert_eq!(task.node_id(), parent);
    }

    /// **A drop out of a task reports the departure**, beside the arrival
    /// `reclassify_moved` already asked. A drop inside one task reports
    /// neither.
    #[test]
    fn dragging_a_row_out_of_a_task_reports_the_departure() {
        let (mut ws, mut e, parent, step) = a_task_with_unfinished_work();
        let after = ws
            .insert(Parent::Root, Node::paragraph("elsewhere").unwrap())
            .unwrap();
        e.body.rebuild(&ws);
        e.reflow();
        e.body.task_activity.clear();

        let from = e.body.index_of(step).expect("the step has a row");
        let to = e.body.index_of(after).expect("and so does the target");
        e.drop_row(&mut ws, from, to);
        assert_eq!(ws.parent(step).unwrap(), Parent::Root, "it left the task");

        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1, "the departure, and no arrival to report");
        let [koru_core::tasks::ActivityIntent::ChildRemoved { task, child }] = owed[0].intents[..]
        else {
            panic!("a drop out owes the departure: {:?}", owed[0]);
        };
        assert_eq!(child, step);
        assert_eq!(task.node_id(), parent);
    }
}
