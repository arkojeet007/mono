// SPDX-License-Identifier: MPL-2.0

//! What the container does about the Trash.
//!
//! Two gestures ask for documents to be binned — the explicit explore-row menu
//! (opened by `…` or right-click) and dropping a selection on Trash — and
//! neither can do it where it is raised: `apply` holds a **shared**
//! `&WorkspaceDoc` and binning needs `&mut`. So each records a `pending_*` and
//! one drain does the work.
//!
//! That is not only a borrow-checker workaround. **The confirmation threshold
//! and the toast both live in [`MainView::drain_trash`]**, so no gesture can
//! reach one without the other — which is the whole safety property of the
//! feature, expressed as "there is one door".
//!
//! ## The other window
//!
//! Trashing closes the tabs *this* window has open on it, because this window
//! asked. It does **not** close anybody else's: `TabSet::retain_live` drops tabs
//! whose node has *gone*, and a trashed node has only moved, so a peer's tab
//! stays open and grows an *In Trash* strip instead. See [`in_trash_notice`].
//!
//! [`in_trash_notice`]: MainView::in_trash_notice

use std::time::Instant;

use koru_core::NodeId;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use koru_ui::button;
use koru_ui::{Confirm, Toast};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState};

use super::MainView;

/// What a confirmation is about. The dialog carries only strings; this is what
/// the container matches on when the answer comes back.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ConfirmSubject {
    Trash(NodeId),
    /// A whole gesture's worth of documents, already counted.
    ///
    /// **The set, not one of them.** A confirm per document asked four times for
    /// one section with four pages under it, and never asked at all for four
    /// pages selected side by side — each of those carries nothing beneath it.
    /// What somebody is agreeing to is the gesture.
    TrashDocuments(Vec<NodeId>),
    DeleteForever(Vec<NodeId>),
    EmptyTrash,
    /// E-3: taking a row off the Tasks list.
    ///
    /// **The odd one in this enum, and deliberately here anyway.** Nothing is
    /// deleted — the marker, the lifecycle and the due date come off and the
    /// paragraph stays — but the confirm machinery is one machinery, and a
    /// second dialog type beside it would be a second set of answers to *what
    /// does Escape mean*.
    RemoveFromTasks(super::tasks::PendingTaskEdit),
    /// V-6: deleting a list from the tasks rail.
    ///
    /// **The second one in this enum that bins nothing.** The list's registry
    /// record is tombstoned and every membership pointing at it is cleared; the
    /// tasks themselves are untouched, which is precisely what the dialog says
    /// and why it is worth asking. It is here rather than in a dialog of its own
    /// for [`RemoveFromTasks`](Self::RemoveFromTasks)'s reason: one confirm
    /// machinery, one answer to what Escape means.
    DeleteList(koru_core::ids::ListId),
    /// **The voice consent step.** It carries nothing: what is being agreed to
    /// is the offer the shell is already holding, and a copy of the terms here
    /// would be a second thing that could be out of date by the time somebody
    /// answers.
    VoiceConsent,
    ClearPersonalDictionary,
    /// A project in the Agents tree. Not a document at all: a yes sends the
    /// typed delete verb to the session host, which is the project registry's
    /// one authority (ADR-264) — nothing in this window's workspace moves.
    DeleteAgentProject(koru_session_protocol::ProjectId),
    /// A schedule retirement: hidden replicated authority, never a manifest
    /// move and never a Trash row. The id is stable across list refreshes.
    RetireSchedules(Vec<koru_delegate_core::ScheduleId>),
    /// **Runs in the Agents navigator.** A yes puts them in the workspace's
    /// bin — reversible, and nothing on disk changes until the bin is
    /// emptied. The dialog has three answers when the runs left results
    /// behind, and the third one is the alternative:
    /// `Confirm::Outcome::Confirm` keeps them, `Alternative` deletes them
    /// with the runs. Nothing here is a manifest edit; the worker answers
    /// first and `drain_run_deletions` finishes it.
    ///
    /// **The set, not one of them**, for [`TrashDocuments`](Self::TrashDocuments)'
    /// reason: what somebody is agreeing to is the gesture, and the question
    /// about what happens to the results is asked once for all of them. Runs
    /// that cannot go were filtered out before the question was asked, and
    /// the dialog says how many those were.
    TrashRuns(Vec<koru_delegate_core::RunId>),
    /// A session in the Agents navigator. A yes sends the backend's terminate
    /// verb, ending the process (Q-E7 P8) — re-checked against the attached
    /// session, since the person may have switched tabs while the dialog stood
    /// open. Distinct from closing a tab (detach) and a process ending itself.
    TerminateSession(koru_session_protocol::SessionId),
    /// **A chat in the Chats navigator** (AGENTUX I5). A yes hands the delete
    /// to the Chats view, which sends it and keeps the row, with a sentence
    /// under it, if the service refuses — a chat whose agent could not be
    /// stopped is not gone (UC13).
    DeleteChat(koru_ai_ui::ChatId),
}

/// More nested documents than this and the deletion is confirmed first.
///
/// **Documents, not nodes.** Counting every descendant would count paragraphs,
/// and then every page with four lines in it would stop to ask.
pub(super) const CONFIRM_ABOVE: usize = 3;

/// The *In Trash* band: two lines of copy and a row of answers, so it is taller
/// than a one-line strip.
pub(super) const NOTICE_H: f32 = 64.0;
const NOTICE_PAD: f32 = 16.0;
const NOTICE_GAP: f32 = 10.0;
const SAVE_COPY: &str = "Save a copy";

const NOTICE_TITLE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::SM, theme::FontWeight::SemiBold);
const NOTICE_BODY_STYLE: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::XS, theme::FontWeight::Medium);

/// **What it says, and why it says it.**
///
/// The first sentence is the fact. The second is the one that matters: the
/// document left the Library, but nothing you typed has gone anywhere, and there
/// are two ways forward rather than one. A notice that only announced the
/// deletion would leave somebody wondering whether their afternoon survived it.
const NOTICE_BODY: &str = "It left the Library while you were editing. Your unsynced \
     changes are still here \u{2014} restore it to keep editing, or save your version as \
     a separate document.";

impl MainView {
    /// Publish the one answer to "is there anything in the Trash?"
    ///
    /// Documents and runs arrive from different indexes, but they share one
    /// bin and one Empty Trash action. Keeping the sum here prevents a caller
    /// from accidentally treating either half as the whole bin.
    pub(super) fn sync_trash_count(&mut self) -> bool {
        let documents = self.trashed_documents.as_ref().map_or(0, Vec::len);
        let runs = self.run_index.trashed().len();
        self.explore.set_trash_count(documents + runs)
    }

    // ------------------------------------------------------------- binning

    /// Move `pending_trash` to the bin, asking first when it takes more than
    /// three documents with it.
    ///
    /// **The only caller of `ws.trash`.** Called at the tail of `key`,
    /// `mouse_down` and `mouse_up`, beside the other drains.
    pub(super) fn drain_trash(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some((node, asked)) = self.pending_trash.take() else {
            return false;
        };
        // **Documents never arrive here.** They are binned as a *set* by
        // `trash_documents_now`, one ask and one toast for the whole gesture —
        // because an Undo that took back only the last of nine is not an undo.
        // This queue is the node bin's, and works one at a time because a node's
        // confirm is about that node.
        if ws.read(node).is_err() || ws.in_trash(node) {
            return false;
        }
        let name = koru_workspace::name_of(ws, node);
        let nested = ws.nested_documents(node);
        if nested > CONFIRM_ABOVE && !asked {
            self.confirm = Some((
                Confirm::new(
                    format!("Move {} items to Trash?", nested + 1),
                    format!(
                        "\u{201C}{name}\u{201D} contains {nested} nested items. They will be \
                         moved together and can be restored from Trash."
                    ),
                    "Move to Trash",
                ),
                ConfirmSubject::Trash(node),
            ));
            return true;
        }
        // Its own undo step: closing the group first stops it folding into
        // whatever was typed a moment ago.
        ws.checkpoint();
        if ws
            .trash(node, koru_workspace::open::now_ms(), self.actor.as_deref())
            .is_err()
        {
            return false;
        }
        self.trashed_here.insert(node);
        // `retain_live` cannot see a *move*, so the tabs this window has open on
        // it are closed here. Undo does not reopen them: it restores the
        // document, not the window layout.
        self.close_tabs_showing(ws, node);
        self.explore_stale = true;
        self.toast = Some((
            Toast::new(trashed_message(&name, nested), Some("Undo"), Instant::now()),
            super::ToastPayload::Restore(vec![node]),
        ));
        true
    }

    pub(super) fn drain_restore(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(nodes) = self.pending_restore.take() else {
            return false;
        };
        // **A run goes to the delegate worker first.** Its restore is two
        // halves and only the second is a manifest edit; sending it down the
        // document path would find no such document and fail in silence.
        let (runs, nodes) = self.split_runs(nodes);
        let restored_runs = self.restore_runs(runs);
        // The same call the toast's Undo makes, so "Undo" and "Restore" cannot
        // drift — and a forward operation rather than `undo`, which still
        // declines on a node another peer deleted.
        // **Documents go to the shell, which owns the manifest.** The same split
        // `queue_trash` makes and for the same reason: a Trash row's `NodeId` is
        // a `DocumentId`'s bytes, and `ws.restore` looks for a node in the open
        // document.
        let (documents, nodes) = self.split_documents(nodes);
        let mut acted = restored_runs;
        if !documents.is_empty() {
            self.ask_pane(koru_workspace_host::PaneRequest::RestoreDocuments(
                documents,
            ));
            acted = true;
        }
        for node in nodes {
            if ws.restore(node, self.actor.as_deref()).is_ok() {
                self.trashed_here.remove(&node);
                acted = true;
            }
        }
        if acted {
            self.explore_stale = true;
            self.toast = None;
        }
        acted
    }

    /// Fork a trashed document into a live one.
    ///
    /// **The other way out of the notice.** Restoring argues with the peer who
    /// binned it; this does not — you keep your version, they keep their
    /// decision, and the two stop being the same document. The copy is a new
    /// node with a new id, so nothing about it is still in the bin.
    pub(super) fn drain_save_copy(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(node) = self.pending_save_copy.take() else {
            return false;
        };
        // **A document goes to the shell**, the same split `queue_trash` and
        // `drain_restore` make: the copy is a document of the *workspace*,
        // registered in the manifest, and a view holds none. The name is decided
        // here because it is a piece of this interface's language.
        let (documents, nodes) = self.split_documents(vec![node]);
        if let Some(&source) = documents.first() {
            let title = format!("{} (copy)", self.name_of_document(source));
            self.ask_pane(koru_workspace_host::PaneRequest::CopyDocument {
                source,
                title: title.clone(),
            });
            self.explore_stale = true;
            self.toast = Some((
                Toast::new(
                    format!("Saved a copy as \u{201C}{title}\u{201D}"),
                    None,
                    Instant::now(),
                ),
                super::ToastPayload::None,
            ));
            return true;
        }
        let Some(node) = nodes.first().copied() else {
            return false;
        };
        let name = koru_workspace::name_of(ws, node);
        let title = format!("{name} (copy)");
        ws.checkpoint();
        let Ok(copy) = ws.duplicate(node, Parent::Root, &title) else {
            return false;
        };
        // Nothing to suppress: the notice speaks for whatever the active tab is
        // showing, and that is about to be the copy — which is not in the bin.
        self.explore_stale = true;
        self.open_node(ws, copy);
        self.toast = Some((
            Toast::new(
                format!("Saved a copy as \u{201C}{title}\u{201D}"),
                None,
                Instant::now(),
            ),
            super::ToastPayload::None,
        ));
        true
    }

    pub(super) fn drain_delete_forever(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some((nodes, asked)) = self.pending_delete_forever.take() else {
            return false;
        };
        if nodes.is_empty() {
            return false;
        }
        if !asked {
            let count = nodes.len();
            let (title, body) = if count == 1 {
                // **A run is named by the two words a person knows it by.**
                // `name_of` reads the open document, which holds no run: a
                // run's row here is a manifest entry whose title is the
                // short, content-free `run-9f2c11ab` nothing reads.
                let (runs, _) = self.split_runs(vec![nodes[0]]);
                let name = match runs.first() {
                    Some(run) => koru_delegate_core::RunName::of(*run).to_string(),
                    None => koru_workspace::name_of(ws, nodes[0]),
                };
                (
                    "Delete permanently?".to_owned(),
                    format!(
                        "\u{201C}{name}\u{201D} will be removed for good. This cannot be undone."
                    ),
                )
            } else {
                (
                    format!("Delete {count} items permanently?"),
                    format!(
                        "{count} selected items will be removed for good. This cannot be undone."
                    ),
                )
            };
            self.confirm = Some((
                Confirm::new(title, body, "Delete permanently"),
                ConfirmSubject::DeleteForever(nodes),
            ));
            return true;
        }
        // **A run's permanent delete is the worker's first.** Everything
        // device-local it left behind goes there — the ledger's rows, its
        // authority record and its working folder when Koru made it — and
        // the Run document only follows once that has answered.
        let (runs, nodes) = self.split_runs(nodes);
        let mut acted = self.purge_runs(runs);
        for node in nodes {
            if ws.delete_permanently(node).is_ok() {
                self.trashed_here.remove(&node);
                acted = true;
            }
        }
        if acted {
            self.explore_stale = true;
        }
        acted
    }

    pub(super) fn drain_empty_trash(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let Some(asked) = self.pending_empty_trash.take() else {
            return false;
        };
        // The workspace's bin when there is one, the document's otherwise —
        // plus the runs, which are in the same bin and on their own road.
        let documents = self.trashed_documents.as_ref().map_or(0, Vec::len);
        let runs = self.trashed_runs();
        let n = if self.trashed_documents.is_some() {
            documents
        } else {
            ws.trash_len()
        } + runs.len();
        if n == 0 {
            return false;
        }
        if !asked {
            let mut body = format!(
                "{n} {} will be removed for good. This cannot be undone.",
                if n == 1 { "item" } else { "items" }
            );
            // **What the empty is about to do to folders, before it does
            // it.** A run's working folder is removed only when Koru made
            // it; one that worked inside the person's own project leaves
            // that folder exactly as it is, and an empty that quietly did
            // less than it offered would teach them that it did more.
            if !runs.is_empty() {
                let lang = crate::format::lang();
                body.push(' ');
                body.push_str(&koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::TrashEmptyRunsNote),
                    &[&koru_i18n::number::decimal(lang, runs.len() as u64)],
                ));
            }
            self.confirm = Some((
                Confirm::new("Empty the Trash?".to_owned(), body, "Empty Trash"),
                ConfirmSubject::EmptyTrash,
            ));
            return true;
        }
        let emptied_runs = self.purge_runs(runs);
        if self.trashed_documents.is_some() {
            if documents > 0 {
                self.ask_pane(koru_workspace_host::PaneRequest::EmptyDocumentTrash);
            }
            self.explore_stale = true;
            return true;
        }
        if emptied_runs && ws.trash_len() == 0 {
            self.explore_stale = true;
            return true;
        }
        let ok = ws.empty_trash().is_ok();
        self.trashed_here.clear();
        self.explore_stale = true;
        ok
    }

    /// Turn the dialog's answer into an action, and drop the dialog.
    pub(super) fn settle_confirm_outcome(&mut self, out: koru_ui::confirm::Outcome) -> bool {
        match out {
            koru_ui::confirm::Outcome::Keep => false,
            koru_ui::confirm::Outcome::Cancel => self.settle_confirm(false),
            koru_ui::confirm::Outcome::Confirm => self.settle_confirm(true),
            // **The third answer**, and only one question has one: deleting
            // a run that produced results asks what happens to them, and
            // this is the half that says *with the run*. Every other
            // subject clears the dialog and does nothing, which is what a
            // dialog that offered no third answer would have done anyway.
            koru_ui::confirm::Outcome::Alternative => self.settle_confirm_alternative(),
        }
    }

    /// The alternative answer. See [`ConfirmSubject::TrashRun`].
    fn settle_confirm_alternative(&mut self) -> bool {
        match self.confirm.take() {
            Some((_, ConfirmSubject::TrashRuns(runs))) => {
                self.send_run_trash(runs, koru_delegate_core::ArtifactDisposition::Delete)
            }
            other => {
                self.confirm = other;
                false
            }
        }
    }

    /// Hover over the toast. `true` while the pointer is on the chip, so the
    /// editor underneath does not keep an I-beam and paint its own hover.
    pub(super) fn toast_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let content = self.regions.content;
        let floor = self.toast_floor();
        let Some((toast, _)) = self.toast.as_mut() else {
            return false;
        };
        toast.mouse_move(&mut self.engine, content, floor, x, y);
        toast.over_chip()
    }

    /// A press on the toast. `true` if it was ours.
    pub(super) fn toast_mouse_down(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let content = self.regions.content;
        let floor = self.toast_floor();
        let Some((toast, payload)) = self.toast.as_mut() else {
            return false;
        };
        let out = toast.mouse_down(&mut self.engine, content, floor, x, y);
        let inside = toast.contains(&mut self.engine, content, floor, x, y);
        let payload = payload.clone();
        match out {
            koru_ui::toast::Outcome::Action => {
                // Move-to undo takes priority when that toast is the one up.
                if self.move_undo.is_some() {
                    let _ = self.undo_move();
                    let _ = ws;
                    return true;
                }
                match payload {
                    // **Undo on a run** is the worker's restore plus the
                    // document coming back out; the toast names the run
                    // rather than a node for exactly that reason.
                    super::ToastPayload::RestoreRuns(runs) => {
                        self.restore_runs(runs);
                    }
                    super::ToastPayload::Restore(nodes) => {
                        // **`restore`, not general history undo.** The toast
                        // names these exact nodes even if another local edit
                        // became the top undo item while it was visible — and
                        // Loro's undo, which usually survives a concurrent
                        // remote change now, still declines outright on a node
                        // another peer deleted. A button whose whole purpose is
                        // reversibility must not depend on either. A forward
                        // operation always works.
                        self.pending_restore = (!nodes.is_empty()).then_some(nodes);
                    }
                    super::ToastPayload::Reveal(path) => {
                        self.ask_pane(koru_workspace_host::PaneRequest::RevealPath(path));
                    }
                    super::ToastPayload::Open(document) => {
                        self.reveal_imported_document(document);
                    }
                    super::ToastPayload::ShowFile(file) => self.reveal_imported_file(file),
                    super::ToastPayload::OpenSupport => {
                        self.ask_pane(koru_workspace_host::PaneRequest::OpenSupport);
                    }
                    super::ToastPayload::OpenProfiles => {
                        self.open_profiles_popover();
                    }
                    super::ToastPayload::ImportProgress => {
                        self.ask_pane(koru_workspace_host::PaneRequest::CancelImport);
                        let _ = ws;
                        return true;
                    }
                    // **Stop, not dismiss.** The toast stays up saying
                    // *Canceling* and the shell closes it three seconds later,
                    // so this returns without clearing it — DESIGN's contract,
                    // and the same one the import above follows.
                    super::ToastPayload::VoiceDownload => {
                        self.ask_pane(koru_workspace_host::PaneRequest::CancelVoiceDownload);
                        let _ = ws;
                        return true;
                    }
                    // Through the trait, so a search-only window still
                    // refuses to grow a Feedback tab it has no room for.
                    super::ToastPayload::ReopenFeedback => {
                        koru_workspace_host::View::open_feedback_tab(self);
                    }
                    super::ToastPayload::TaskAdded {
                        document,
                        node,
                        target,
                    } => {
                        self.view_added_task(document, node, target);
                    }
                    super::ToastPayload::None => {}
                }
                self.toast = None;
                let _ = ws;
                true
            }
            // The second action — *Undo* on a capture's toast, the only chip
            // that has one today.
            koru_ui::toast::Outcome::Secondary => {
                if let super::ToastPayload::TaskAdded { document, node, .. } = payload {
                    self.undo_added_task(document, node);
                }
                self.toast = None;
                let _ = ws;
                true
            }
            koru_ui::toast::Outcome::Dismiss => {
                self.toast = None;
                self.move_undo = None;
                true
            }
            // A press on the chip but off its controls is still ours: it sits
            // over the document, and falling through would move the caret.
            koru_ui::toast::Outcome::Miss => inside,
        }
    }

    pub(super) fn reveal_imported_document(&mut self, document: koru_core::DocumentId) {
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        self.explore.show_documents();
        let node = koru_core::NodeId::from_uuid(document.as_uuid());
        self.explore_stale |= self.explore.reveal(node);
        self.explore.set_selected(Some(node));
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        });
    }

    pub(super) fn reveal_imported_file(&mut self, file: koru_core::DocumentId) {
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        self.explore.show_files();
        let node = koru_core::NodeId::from_uuid(file.as_uuid());
        self.explore.reveal(node);
        self.explore.set_selected(Some(node));
    }

    /// **Ctrl+Z takes a delete back, while the toast offering it is up.**
    ///
    /// The toast says *Undo*; the keyboard should agree, and this is the one
    /// place the two can be made to. It calls `restore` rather than `ws.undo()`
    /// for the reason [ADR-70] gives: the toast names this exact deletion,
    /// while general history undo means whichever local step is newest — and
    /// Loro's undo still declines outright on a node another peer deleted.
    ///
    /// There is a second reason the editor cannot do this: **trashing closes the
    /// tab**, so by the time Ctrl+Z arrives there is usually no editor left that
    /// has ever heard of the document.
    ///
    /// **Bounded by the toast's life**, deliberately. That is the window in
    /// which Ctrl+Z means "take that back"; after it, it belongs to whatever
    /// you are typing in, and stealing it would be worse than not offering it.
    ///
    /// [ADR-70]: ../../../../docs/decisions/README.md
    pub(super) fn undo_key_takes_back_a_trash(&mut self, key: &Key, mods: ModifiersState) -> bool {
        if !koru_keys::primary_only(mods) {
            return false;
        }
        if koru_editor::input::keymap::ctrl_letter(key, None) != Some('z') {
            return false;
        }
        // Move-to toast carries no trash nodes; reverse via move_undo.
        if self.move_undo.is_some() && self.toast.is_some() {
            return self.undo_move();
        }
        // Only while a toast is offering Restore, and only when it has something
        // to take back — the hand-off toast ("… was restored") and export Show
        // carry no restore list.
        // **Everything the toast was about.** Taking back only the first of
        // them would be worse than offering nothing.
        let nodes = match self.toast.as_ref().map(|(_, p)| p) {
            Some(super::ToastPayload::Restore(nodes)) if !nodes.is_empty() => nodes.clone(),
            _ => return false,
        };
        self.pending_restore = Some(nodes);
        self.toast = None;
        true
    }

    /// Where the toast's floor is — lifted above the update banner when one is
    /// showing, so the two do not stack on top of each other.
    pub(super) fn toast_floor(&self) -> f32 {
        let c = self.regions.content;
        let mut floor = c.y + c.h;
        // Two other things can own the bottom of the window. The chip stacks
        // above whichever is there rather than on top of it.
        if self.notice_showing {
            floor = floor.min(self.notice_rect().y);
        }
        if let Some(top) = self.update_banner_top() {
            floor = floor.min(top);
        }
        floor
    }

    /// Drop the toast once it has expired. Called from the frame's advance
    /// block; dropping it is what lets `animating` reach `false`.
    pub(super) fn tick_toast(&mut self, now: Instant) -> bool {
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|(t, _)| t.alpha(now).is_none());
        if expired {
            self.toast = None;
        }
        expired
    }

    /// Whether the toast wants another frame. **Only during its fade** — see
    /// `koru_ui::toast`.
    pub(super) fn toast_animating(&self, now: Instant) -> bool {
        self.toast.as_ref().is_some_and(|(t, _)| t.animating(now))
    }

    pub(super) fn draw_toast(&mut self, scene: &mut Scene, root: Affine, now: Instant) {
        let content = self.regions.content;
        let floor = self.toast_floor();
        let Some((toast, _)) = self.toast.as_ref() else {
            return;
        };
        let Some(alpha) = toast.alpha(now) else {
            return;
        };
        // Split out so the borrow of `self.toast` ends before the engine is
        // borrowed mutably.
        let toast = std::mem::take(&mut self.toast);
        if let Some((t, payload)) = toast {
            t.draw(scene, root, &mut self.engine, content, floor, alpha, now);
            self.toast = Some((t, payload));
        }
    }

    pub(super) fn draw_confirm(&mut self, scene: &mut Scene, root: Affine) {
        let dialog = std::mem::take(&mut self.confirm);
        if let Some((d, subject)) = dialog {
            d.draw(scene, root, &mut self.engine, self.viewport);
            self.confirm = Some((d, subject));
        }
    }

    /// The answer to a confirmation. Re-arms the matching `pending_*` and lets
    /// the drain run again, so there is still exactly one place each verb
    /// happens.
    pub(super) fn settle_confirm(&mut self, confirmed: bool) -> bool {
        let Some((_, subject)) = self.confirm.take() else {
            return false;
        };
        // **The voice step is answered either way**, which is what makes it
        // different from every other subject here: the shell is holding a
        // catalog entry until somebody says yes or no, and a Cancel that only
        // closed the dialog would leave it holding one forever — and the next
        // press would be ignored as *already offering*.
        if subject == ConfirmSubject::VoiceConsent {
            self.ask_pane(koru_workspace_host::PaneRequest::VoiceConsent(confirmed));
            return true;
        }
        if !confirmed {
            // Cancelling clears the request as well as the dialog — a pending
            // deletion that survived a Cancel would go off on the next drain.
            // **And the rest of the dragged set with it**: somebody who says no
            // to one of them is answering the gesture, not that one page.
            self.pending_trash = None;
            self.trash_queue.clear();
            self.pending_delete_forever = None;
            self.pending_empty_trash = None;
            // A run that was not confirmed is a run nothing was told about:
            // the subject carried the whole request, so dropping the dialog
            // drops it.
            return true;
        }
        // Re-armed **as already asked**, so the drain performs rather than
        // putting the same question up again.
        match subject {
            ConfirmSubject::Trash(id) => self.pending_trash = Some((id, true)),
            ConfirmSubject::DeleteForever(nodes) => {
                self.pending_delete_forever = Some((nodes, true))
            }
            ConfirmSubject::EmptyTrash => self.pending_empty_trash = Some(true),
            // Queued only on *yes*, which is the whole of what makes the
            // confirm the one door: cancelling leaves nothing behind to go off
            // on the next turn.
            // Queued on *yes* and nowhere else — through the one door, so the
            // row leaves the page and the numbers now, and the store confirms
            // it a moment later.
            ConfirmSubject::RemoveFromTasks(pending) => {
                self.queue_task_command(pending.command, true)
            }
            // Queued on *yes* and nowhere else, for that same reason — and
            // through `apply_tasks_rail`, so a list's deletion travels the one
            // vocabulary every other list gesture does.
            ConfirmSubject::DeleteList(id) => {
                self.apply_tasks_rail(crate::panes::explore::rail::TasksRailRequest::DeleteList(
                    id,
                ));
            }
            // The whole gesture was agreed to, so every one of them queues —
            // and none asks again, because the question was about the set.
            ConfirmSubject::TrashDocuments(nodes) => {
                self.trash_now(nodes);
            }
            // Keep the results, which is the safe half of the question and
            // the answer a run with none gets without being asked.
            ConfirmSubject::TrashRuns(runs) => {
                self.send_run_trash(runs, koru_delegate_core::ArtifactDisposition::Keep);
            }
            // Straight to the wire: delete is bookkeeping on the session
            // host's registry, and the tree redraws off the `ProjectsChanged`
            // that answers. A host-side refusal (sessions started running
            // since the menu dimmed) comes back through the backend's failure
            // path rather than being swallowed.
            ConfirmSubject::DeleteAgentProject(id) => {
                if let Some(sender) = self.session_verbs() {
                    sender.delete_project(id);
                }
            }
            ConfirmSubject::RetireSchedules(schedules) => {
                for schedule in schedules {
                    if let Some(subject) = self.schedule_index.subject(schedule) {
                        self.schedule_index.retire(
                            koru_delegate_api::wire::RetireScheduleRequest {
                                schedule,
                                workspace: subject.workspace,
                            },
                        );
                    }
                }
            }
            // Terminate acts on the controller the backend holds, so it is
            // only sent while this window is still attached to the session the
            // confirm was about — a tab switch since then means the lease has
            // moved, and terminating whatever the backend now controls would
            // kill the wrong session.
            ConfirmSubject::TerminateSession(session) => {
                let attached_here = self.session_attached.is_some()
                    && self.session_attached == self.session_tab_for(session);
                if attached_here && let Some(sender) = self.session_verbs() {
                    sender.terminate();
                }
            }
            // Answered above, before the cancel path — it is the one subject
            // whose *no* is as much of an answer as its *yes*.
            ConfirmSubject::VoiceConsent => {}
            ConfirmSubject::ClearPersonalDictionary => {
                koru_workspace_host::spelling::edit(
                    koru_workspace_host::spelling::SpellEdit::ClearPersonal,
                );
            }
            ConfirmSubject::DeleteChat(chat) => self.explore.chats_mut().delete(chat),
        }
        true
    }

    /// Close every tab in **this** window showing `node` or anything under it.
    ///
    /// `TabSet::retain_live` cannot do this: it drops tabs whose node has gone,
    /// and a trashed node has only moved. Deliberately local — a peer's window
    /// keeps its tab and grows the *In Trash* strip instead.
    pub(super) fn close_tabs_showing(&mut self, ws: &WorkspaceDoc, node: NodeId) {
        // Back to front, so closing one does not renumber the next.
        let doomed: Vec<usize> = (0..self.tabs().len())
            .rev()
            .filter(|&i| {
                self.tabs()
                    .get(i)
                    .and_then(|t| t.view.root())
                    .is_some_and(|r| r == node || ws.in_trash(r))
            })
            .collect();
        for i in doomed {
            self.close(i);
        }
    }

    /// Every trash drain, in the order they have to run.
    ///
    /// One call at each of the three tails rather than four, so a new gesture
    /// cannot be wired to three of them and quietly miss the fourth. The Trash
    /// tab's own request is drained first, since it is what raises three of the
    /// four `pending_*`.
    pub(super) fn drain_trash_all(&mut self, ws: &mut WorkspaceDoc) -> bool {
        // The Tasks tab's request rides this call rather than growing a fourth
        // tail of its own, for the reason this function exists: a new gesture
        // must not be wired to two of the three places a drain has to run.
        let mut acted = self.drain_tasks_request();
        // The Reminders page's intent rides it too — the same surface, the
        // same tails, the same reason.
        acted |= self.drain_reminders_request();
        // …and a canvas's Run, Stop and open-a-page, for the same reason.
        acted |= self.drain_canvas_asks();
        acted |= self.drain_trash_request();
        acted |= self.drain_trash(ws);
        // Work through a dragged set, stopping the moment one of them puts a
        // question up — the rest wait behind it rather than going off while
        // somebody is reading it.
        while self.pending_trash.is_none() && self.confirm.is_none() {
            let Some(next) = self.trash_queue.pop_front() else {
                break;
            };
            self.pending_trash = Some((next, false));
            acted |= self.drain_trash(ws);
        }
        acted |= self.drain_restore(ws);
        acted |= self.drain_save_copy(ws);
        acted |= self.drain_delete_forever(ws);
        acted |= self.drain_empty_trash(ws);
        if acted {
            // The listing the Trash tab is showing is now a lie.
            if let super::tab::TabView::Trash(t) = &mut self.tabs_mut().active_mut().view {
                t.mark_stale();
            }
        }
        acted
    }

    // -------------------------------------------------------- the hand-off

    /// Notice when somebody else takes back something *this* window binned, and
    /// say so.
    ///
    /// Checked from `refresh_explore`, which already runs once per structural
    /// change; the set is only what this session deleted, so this is a handful
    /// of lookups on a walk that was happening anyway.
    pub(super) fn notice_restores(&mut self, ws: &WorkspaceDoc) {
        let taken_back: Vec<NodeId> = self
            .trashed_here
            .iter()
            .copied()
            .filter(|id| ws.read(*id).is_ok() && !ws.in_trash(*id))
            .collect();
        for id in taken_back {
            self.trashed_here.remove(&id);
            let name = koru_workspace::name_of(ws, id);
            // **No name unless the document carries one.** Nothing here infers
            // who acted; the client that restored it wrote its own label, and if
            // there is none the sentence simply has no "by" clause. Inventing
            // one would be worse than saying nothing.
            let message = match ws.restored_by(id) {
                Some(who) => format!("\u{201C}{name}\u{201D} was restored by {who}"),
                None => format!("\u{201C}{name}\u{201D} was restored"),
            };
            self.toast = Some((
                Toast::new(message, None, Instant::now()),
                super::ToastPayload::None,
            ));
        }
    }

    // ------------------------------------------------------ the In Trash notice

    /// What the active tab should say about being in the bin, if anything.
    ///
    /// Derived, never stored: `in_trash` plus the label the acting client wrote.
    pub(super) fn in_trash_notice(&self, ws: &WorkspaceDoc) -> Option<Notice> {
        // **From the manifest when there is one.** A document's binning is a
        // manifest change now, so `ws.in_trash` — which looks in the open
        // document's own hidden trash root — answers `false` forever and the
        // window sat there editing a page that had left the Library.
        if let Some(trashed) = &self.trashed_documents {
            // The notice belongs to the active tab, not to the last Store
            // document announced for this window. Welcome and a fresh draft
            // have no document; the shell may still have the previous one open
            // while either is visible, and using that window-level value leaked
            // its banner onto unrelated tabs.
            let id = self.tabs().active_tab().document?;
            let entry = trashed.iter().find(|e| e.id == id)?;
            let node = NodeId::from_uuid(id.as_uuid());
            // **Not in the window that did it.** The person who binned it does
            // not need telling; the notice is for the peer who was reading it
            // (ADR-70).
            if self.trashed_here.contains(&node) {
                return None;
            }
            return Some(Notice {
                node,
                title: match entry.trashed_by.as_deref() {
                    Some(who) if Some(who) != self.actor.as_deref() => {
                        format!("{who} moved this document to Trash")
                    }
                    // Either no label, or our own — and our own reaching here
                    // means another window of the same person's, which "you"
                    // would misdescribe.
                    _ => "This document was moved to Trash".into(),
                },
            });
        }
        let root = self.tabs().active_tab().view.root()?;
        if !ws.in_trash(root) {
            return None;
        }
        Some(Notice {
            node: root,
            title: match ws.trashed_by(root) {
                Some(who) => format!("{who} moved this document to Trash"),
                // No label — an unnamed profile, or an older build. Say what
                // happened without naming the wrong person.
                None => "This document was moved to Trash".into(),
            },
        })
    }

    /// What a document of this workspace is called, from the lists the shell
    /// pushed — the library's and the bin's, since the one being copied is in
    /// the bin by definition.
    ///
    /// `Untitled` for a document neither list names, which is the same answer
    /// `name_of` gives a node with no text: a copy has to be called something,
    /// and refusing to make one because its name could not be read would be a
    /// worse failure than a plain name.
    pub(super) fn name_of_document(&self, id: koru_core::DocumentId) -> String {
        [&self.entries, &self.trashed_documents]
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| e.id == id)
            .map(|e| e.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Untitled".into())
    }

    /// The band across the **bottom** of the content region.
    ///
    /// At the foot rather than the head because the top of a document is where
    /// its title is, and this is not about the title — it is about the state the
    /// page is in, which is what the bottom of a window is for. It also stops
    /// the band shoving the first line of somebody's writing down the screen
    /// every time a peer bins something.
    ///
    /// Not the tab label — that is elided already and invisible on a background
    /// tab — and not the title row, which *is* the document and would then have
    /// chrome every structural operation had to reason about.
    pub(super) fn notice_rect(&self) -> Rect {
        let c = self.focused_view_rect();
        Rect::new(c.x, c.y + c.h - NOTICE_H, c.w, NOTICE_H)
    }

    /// Restore, Save a copy, and the dismiss box — laid out **from the right**,
    /// so the answer that puts things back owns the end of the row however wide
    /// the others turn out to be in this language.
    pub(super) fn notice_targets(&mut self, ws: &WorkspaceDoc) -> Vec<(NoticeHit, Rect)> {
        if self.in_trash_notice(ws).is_none() {
            return Vec::new();
        }
        let r = self.notice_rect();
        // Both answers are offered again. For a while this was Restore alone,
        // because forking went through `WorkspaceDoc::duplicate` — a copy of a
        // subtree *inside one document*, right while a document was a node in
        // the library tree and wrong once it became a document of its own. The
        // copy now goes to the shell, which registers it in the manifest.
        let mut right = r.x + r.w - NOTICE_PAD;
        let mut targets = Vec::new();
        let copy_w = button::button_width(&mut self.engine, SAVE_COPY);
        let copy = Rect::new(
            right - copy_w,
            r.y + (r.h - button::H) * 0.5,
            copy_w,
            button::H,
        );
        right = copy.x - NOTICE_GAP;
        targets.push((NoticeHit::SaveCopy, copy));
        let restore_w = button::button_width(&mut self.engine, "Restore");
        let restore = Rect::new(
            right - restore_w,
            r.y + (r.h - button::H) * 0.5,
            restore_w,
            button::H,
        );
        targets.insert(0, (NoticeHit::Restore, restore));
        targets
    }

    /// A press on the band. `true` if it was ours.
    pub(super) fn notice_mouse_down(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let Some(notice) = self.in_trash_notice(ws) else {
            return false;
        };
        for (hit, r) in self.notice_targets(ws) {
            if !r.contains(x, y) {
                continue;
            }
            match hit {
                NoticeHit::Restore => self.pending_restore = Some(vec![notice.node]),
                NoticeHit::SaveCopy => self.pending_save_copy = Some(notice.node),
            }
            return true;
        }
        // The rest of the band is inert but still ours: it sits over the
        // document, and a press falling through would move the caret.
        self.notice_rect().contains(x, y)
    }

    pub(super) fn draw_in_trash_notice(
        &mut self,
        scene: &mut Scene,
        root_t: Affine,
        ws: &WorkspaceDoc,
    ) {
        let Some(notice) = self.in_trash_notice(ws) else {
            self.notice_showing = false;
            self.notice_buttons.clear();
            return;
        };
        let p = theme::p();
        let r = self.notice_rect();
        scene::fill(scene, root_t, p.notice_bg, &scene::rect(r.x, r.y, r.w, r.h));
        // The rule sits on the band's **top** edge now that it is at the foot of
        // the page: it separates the notice from the document above it.
        scene::fill(
            scene,
            root_t,
            p.notice_fg.multiply_alpha(0.25),
            &scene::rect(r.x, r.y, r.w, 1.0),
        );

        let icon = Rect::new(r.x + NOTICE_PAD, r.y, 20.0, r.h);
        let glyph = self
            .engine
            .icon_layout(koru_draw::icons::TRASH, theme::CAPTION_ICON);
        scene::draw_layout(
            scene,
            root_t,
            &glyph,
            icon.x + (icon.w - glyph.width()) * 0.5,
            icon.y + (icon.h - glyph.height()) * 0.5,
            Some(p.notice_fg),
        );

        // The text stops where the buttons start, so a long name wraps rather
        // than running underneath them.
        let tx = icon.x + icon.w + 10.0;
        let targets = self.notice_targets(ws);
        let right = targets
            .first()
            .map(|(_, b)| b.x - NOTICE_GAP)
            .unwrap_or(r.x + r.w - NOTICE_PAD);
        let tw = (right - tx).max(80.0);

        let title = self
            .engine
            .simple_layout_wrapped(&notice.title, NOTICE_TITLE, tw);
        let body = self
            .engine
            .simple_layout_wrapped(NOTICE_BODY, NOTICE_BODY_STYLE, tw);
        let block = title.height() + 2.0 + body.height();
        let ty = r.y + (r.h - block) * 0.5;
        scene::draw_layout(scene, root_t, &title, tx, ty, Some(p.text));
        scene::draw_layout(
            scene,
            root_t,
            &body,
            tx,
            ty + title.height() + 2.0,
            Some(p.text_dim),
        );

        self.notice_showing = true;
        self.notice_buttons = targets.iter().map(|(_, b)| *b).collect();
        for (hit, b) in targets {
            let hot = b.contains(self.pointer.0, self.pointer.1);
            match hit {
                NoticeHit::Restore => button::draw_button(
                    scene,
                    root_t,
                    &mut self.engine,
                    b,
                    "Restore",
                    button::ButtonRole::Secondary,
                    hot,
                    false,
                ),
                // A quieter offer than Restore: putting it back is the ordinary
                // answer, and keeping your own fork is the other one — so it is
                // a text button rather than a second box competing with it.
                NoticeHit::SaveCopy => {
                    let l = self.engine.simple_layout(SAVE_COPY, button::LABEL);
                    scene::draw_layout_first_line(
                        scene,
                        root_t,
                        &l,
                        b.x + (b.w - l.width()) * 0.5,
                        b.y + (b.h - scene::first_line_height(&l)) * 0.5,
                        Some(if hot { p.text } else { p.text_dim }),
                    );
                }
            }
        }
    }
}

/// What the *In Trash* band says, and about which document.
pub(super) struct Notice {
    pub node: NodeId,
    pub title: String,
}

/// **There is no dismiss.** The band is not a notification you can wave away —
/// it is the state the document is in, and it goes when the state does. A × here
/// would let somebody hide the fact that the page they are typing into is in the
/// bin, which is precisely the thing they need to know.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum NoticeHit {
    Restore,
    SaveCopy,
}

/// The two shapes the trash toast takes, in one function so they cannot drift.
///
/// **The name is elided, not the sentence** — eliding the whole string is how
/// "to Trash" ends up being what gets cut.
/// What the toast says about a whole gesture.
///
/// One document reads as itself; several read as a count, because quoting one of
/// nine names and saying nothing about the rest is how somebody presses Undo
/// believing it undoes only what it named.
pub(super) fn trashed_batch_message(name: &str, roots: usize, moving: usize) -> String {
    if roots <= 1 {
        return trashed_message(name, moving.saturating_sub(1));
    }
    format!("Moved {moving} items to Trash")
}

fn trashed_message(name: &str, nested: usize) -> String {
    let shown = elide(name, NAME_MAX);
    match nested {
        0 => format!("Moved \u{201C}{shown}\u{201D} to Trash"),
        1 => format!("Moved \u{201C}{shown}\u{201D} and 1 nested item to Trash"),
        n => format!("Moved \u{201C}{shown}\u{201D} and {n} nested items to Trash"),
    }
}

/// Longest name a toast quotes, in characters. Measured in graphemes rather than
/// pixels because this is composed once, without a font in hand.
const NAME_MAX: usize = 42;

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", kept.trim_end())
}

#[cfg(test)]
mod tests;
