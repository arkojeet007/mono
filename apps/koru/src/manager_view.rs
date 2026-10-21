// SPDX-License-Identifier: MPL-2.0

//! The two kinds of window the manager can host, as one type.
//!
//! [`MainView`] is the container — tabs and side columns. A popped-out document
//! is a [`BuiltInDocument`] on its own, which is a *different* view type. The
//! shell's [`Host`] trait has a single `V`, and every pane holds one, so the two
//! are unified here as [`ManagerView`].
//!
//! That is the whole reason this module exists. Nothing else needs to know both
//! kinds at once: the shell routes by [`View`], and each arm delegates to the
//! view it holds. The container owns the tab strip and the pop-out gesture; the
//! document window owns its close-choice dialog.
//!
//! ## Why the close dialog lives here and not in `koru-document`
//!
//! "Restore as a tab" only means something when there is a container to restore
//! *into*. The standalone `koru-document` binary has no container, so it has no
//! such dialog. The dialog is a property of a document window the *container*
//! popped out, which is exactly what [`DocumentPane`] wraps.

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use koru_document::BuiltInDocument;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, icons, scene, theme};
use koru_editor::Editing;
use koru_window::WindowRequest;
use koru_workspace::Workspace;
use koru_workspace_host::{PopoutClose, Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use crate::mainview::MainView;

/// One window: the tab container, or a single document popped out on its own.
///
/// `Host::V` is a single type, and every pane the shell opens holds one — so
/// the container and a popped-out document share this enum rather than the shell
/// learning a second view type. Each arm delegates [`View`] straight through.
///
// Both arms are large — a `MainView` carries a whole tab set, and a
// `DocumentPane` carries an editor — so the enum is sized to the bigger of
// them. Boxing would save bytes on a `HashMap` that holds a handful of panes,
// at the cost of an indirection on every key, mouse and draw dispatch. Not
// worth it, for the same reason `TabView` is not boxed.
#[allow(clippy::large_enum_variant)]
pub enum ManagerView {
    Container(MainView),
    Document(DocumentPane),
}

/// A document that *is* its window, popped out of a container's tab strip.
///
/// It wraps a [`BuiltInDocument`] that owns its window chrome, and adds the one
/// thing a popped-out document has that a standalone one does not: a close-choice
/// dialog, because closing it can either leave the document closed or restore it
/// as a tab in the container it came from.
pub struct DocumentPane {
    doc: BuiltInDocument,
    /// The document node, remembered so "restore as a tab" can name it without
    /// re-asking the editor for its root.
    node: NodeId,
    /// The close-choice overlay, open while the user decides what closing does.
    close: Option<CloseDialog>,
    /// A pending request to restore the document as a container tab. The shell
    /// drains it and routes it to the container, then closes this window.
    wants_restore_tab: bool,
    /// *Support replied · Open* over the document — the same widget the
    /// document's search surface uses, so the notice reaches the active
    /// window whatever kind it is (D18).
    support_toast: Option<koru_ui::Toast>,
    /// The toast's *Open*, forwarded through `take_pane_request`.
    pending_request: Option<koru_workspace_host::PaneRequest>,
    /// True once a close is going ahead, so the dialog does not re-open in
    /// response to the close request it itself produced.
    closing: bool,
    /// The remembered close choice in force this run. `Ask` means the dialog is
    /// shown; the shell passes the persisted value in when the pane is built.
    popout_close: PopoutClose,
    /// A window-level request the dialog (or a vetoed close) asked the shell to
    /// perform. Separate from the document's own request so the dialog can emit
    /// a close after resolving without going through the document.
    outgoing: Option<WindowRequest>,
    /// A new remembered choice the user made this run, if any. The shell writes
    /// it to `Settings`.
    pending_setting: Option<PopoutClose>,
    /// The task this window's caret was last in — the caret half of the
    /// content-burst boundary (ADR-273). A popped-out document is a window and
    /// therefore has a caret of its own; the container's copy is on `MainView`.
    task_caret: crate::tasks::TaskCaret,
}

impl DocumentPane {
    /// Where a toast sits over this window: the whole viewport, the toast's
    /// own placement rule putting it at the bottom.
    fn toast_area(&self) -> koru_draw::geometry::Rect {
        let (w, h) = self.doc.editor.view.viewport;
        koru_draw::geometry::Rect::new(0.0, 0.0, w, h)
    }

    /// The toast's *Open*, drained by the shell like the container's requests.
    fn take_pane_request(&mut self) -> Option<koru_workspace_host::PaneRequest> {
        self.pending_request.take()
    }

    /// Open `doc` in its own window, sharing the live workspace. `popout_close`
    /// is the remembered close choice (the shell reads it from `Settings`).
    pub fn open(ws: &WorkspaceDoc, doc: NodeId, popout_close: PopoutClose) -> Self {
        Self {
            doc: BuiltInDocument::open(ws, doc).owns_the_window(ws),
            node: doc,
            close: None,
            support_toast: None,
            pending_request: None,
            wants_restore_tab: false,
            closing: false,
            popout_close,
            outgoing: None,
            pending_setting: None,
            task_caret: crate::tasks::TaskCaret::default(),
        }
    }

    /// The viewport, read from the editor so the dialog can centre in it.
    fn viewport(&self) -> (f32, f32) {
        self.doc.editor.view.viewport
    }

    /// Apply a close choice taken either from the dialog or a remembered setting.
    ///
    /// `remember` writes the choice back via [`take_popout_close_setting`]; the
    /// shell persists it. Either way the window is asked to close, and "restore"
    /// flags a tab restore for the shell to drain.
    fn choose(&mut self, choice: CloseChoice, remember: bool) {
        let setting = match choice {
            CloseChoice::Restore => PopoutClose::RestoreTab,
            CloseChoice::Close => PopoutClose::Close,
        };
        if remember {
            self.popout_close = setting;
            self.pending_setting = Some(setting);
        }
        if matches!(choice, CloseChoice::Restore) {
            self.wants_restore_tab = true;
        }
        self.closing = true;
        self.close = None;
        self.outgoing = Some(WindowRequest::Close);
    }
}

impl View for DocumentPane {
    /// **Straight through to the document, which draws its own caption.**
    ///
    /// A popped-out window shares the container's workspace, so it is owed the
    /// same answer about the store. Without this line the pane swallowed it and
    /// the one window somebody had put a document into to concentrate on was
    /// the one window that never said the store was away.
    fn set_store_trouble(&mut self, trouble: Option<koru_workspace::StoreTrouble>) -> bool {
        View::set_store_trouble(&mut self.doc, trouble)
    }

    /// The document was **replaced**, not edited. Every `ChangeTracker` under
    /// this view is a subscription on the old one and is dead, so this rebuilds
    /// rather than patching — `adopt` cannot help here and never will.
    fn reseat(&mut self, ws: &WorkspaceDoc) {
        // The node this window was showing lived in the closed document, so
        // there is no id left to reopen at — `first` lands on whatever the new
        // workspace's library offers, the same as a cold start.
        self.doc = BuiltInDocument::first(ws);
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // The dialog is modal: Esc cancels (closes the document), Enter confirms
        // the default (restore tab), and anything else is swallowed so typing
        // does not reach the editor behind it.
        if self.close.is_some() {
            use winit::keyboard::NamedKey;
            match key {
                Key::Named(NamedKey::Escape) => self.choose(CloseChoice::Close, false),
                Key::Named(NamedKey::Enter) => {
                    let remember = self.close.as_ref().is_some_and(|d| d.remember);
                    self.choose(CloseChoice::Restore, remember);
                }
                _ => {}
            }
            return true;
        }
        BuiltInDocument::key(&mut self.doc, ws, key, text, mods)
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        // The dialog takes the click first; resolving a choice ends the
        // interaction, so the hit and the "remember" flag are read out before
        // `choose` borrows `self` again.
        let viewport = self.viewport();
        if let Some(hit) = self.close.as_ref().and_then(|d| d.hit_at(viewport, x, y)) {
            let remember = self.close.as_ref().is_some_and(|d| d.remember);
            match hit {
                CloseHit::Remember => {
                    if let Some(d) = self.close.as_mut() {
                        d.remember = !d.remember;
                    }
                }
                CloseHit::Restore => self.choose(CloseChoice::Restore, remember),
                CloseHit::Close => self.choose(CloseChoice::Close, remember),
            }
            return true;
        }
        if self.close.is_some() {
            // A click outside the dialog dismisses it and closes the document,
            // the way cancelling a native close confirm does.
            self.choose(CloseChoice::Close, false);
            return true;
        }
        if self.support_toast.is_some() {
            let over = self.toast_area();
            let engine = &mut self.doc.editor.view.engine;
            if let Some(toast) = self.support_toast.as_mut() {
                match toast.mouse_down(engine, over, over.y + over.h, x, y) {
                    koru_ui::toast::Outcome::Action => {
                        self.support_toast = None;
                        self.pending_request = Some(koru_workspace_host::PaneRequest::OpenSupport);
                        return true;
                    }
                    koru_ui::toast::Outcome::Dismiss => {
                        self.support_toast = None;
                        return true;
                    }
                    koru_ui::toast::Outcome::Secondary | koru_ui::toast::Outcome::Miss => {}
                }
            }
        }
        BuiltInDocument::mouse_down(&mut self.doc, ws, x, y, ctrl)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.close.is_some() {
            let viewport = self.viewport();
            let new = self.close.as_ref().and_then(|d| d.hit_at(viewport, x, y));
            let mut changed = false;
            if let Some(d) = self.close.as_mut() {
                changed = new != d.hover;
                d.hover = new;
            }
            return changed;
        }
        BuiltInDocument::mouse_move(&mut self.doc, x, y)
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.close.is_some() {
            return false;
        }
        Editing::mouse_up(&mut self.doc, ws)
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if self.close.is_some() {
            return true;
        }
        BuiltInDocument::mouse_double_click(&mut self.doc, ws, x, y)
    }

    fn cursor_icon(&self) -> CursorIcon {
        // The dialog owns the pointer while it is open, so the document's
        // I-beam cannot show through it. It does not change shape over the
        // buttons or the checkbox — Koru does not use the hand cursor
        // (`docs/features/design/DESIGN.md`) — and the arms are the record of what is pressable.
        if let Some(dialog) = &self.close {
            return match dialog.hover {
                Some(CloseHit::Restore) | Some(CloseHit::Close) | Some(CloseHit::Remember) => {
                    koru_ui::cursor::PRESS
                }
                None => koru_ui::cursor::IDLE,
            };
        }
        View::cursor_icon(&self.doc)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if self.close.is_some() {
            return;
        }
        Editing::scroll_by(&mut self.doc, delta);
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        Editing::set_viewport(&mut self.doc, w, h);
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.doc.editor.view.mods = mods;
    }

    fn set_maximized(&mut self, maximized: bool) {
        View::set_maximized(&mut self.doc, maximized);
    }

    fn set_pinned(&mut self, pinned: bool) {
        View::set_pinned(&mut self.doc, pinned);
    }

    fn adopt(&mut self, ws: &WorkspaceDoc) {
        Editing::adopt(&mut self.doc, ws);
    }

    fn invalidate(&mut self) {
        View::invalidate(&mut self.doc);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        View::draw(
            &mut self.doc,
            scene,
            workspace,
            images,
            root,
            caret_on,
            pointer,
        );
        if self.close.is_some() {
            // Read the viewport out (a copy) and borrow the engine directly, so
            // neither holds `self` across the other.
            let viewport = self.doc.editor.view.viewport;
            let engine = &mut self.doc.editor.view.engine;
            if let Some(dialog) = self.close.as_ref() {
                dialog.draw(scene, root, engine, viewport);
            }
        }
        if self.support_toast.is_some() {
            let now = std::time::Instant::now();
            let over = self.toast_area();
            let engine = &mut self.doc.editor.view.engine;
            if let Some(toast) = self.support_toast.as_ref()
                && let Some(alpha) = toast.alpha(now)
            {
                toast.draw(scene, root, engine, over, over.y + over.h, alpha, now);
            }
        }
    }

    /// *Support replied · Open* over the document (D18: the active window,
    /// whichever kind it is).
    fn show_support_reply(&mut self) -> bool {
        let lang = crate::format::lang();
        self.support_toast = Some(koru_ui::Toast::new(
            koru_i18n::t(lang, koru_i18n::Key::SupportReplied).to_string(),
            Some(koru_i18n::t(lang, koru_i18n::Key::SupportOpen)),
            std::time::Instant::now(),
        ));
        true
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        let toast = self
            .support_toast
            .as_ref()
            .map(|_| std::time::Instant::now() + std::time::Duration::from_millis(250));
        match (toast, View::maintenance_deadline(&self.doc)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        let expired = self
            .support_toast
            .as_ref()
            .is_some_and(|t| t.alpha(now).is_none());
        if expired {
            self.support_toast = None;
        }
        View::maintain(&mut self.doc, ws, now) || expired
    }

    fn document_root(&self) -> Option<NodeId> {
        Some(self.node)
    }

    fn take_wants_open(&mut self) -> Option<NodeId> {
        View::take_wants_open(&mut self.doc)
    }

    fn take_wants_sharing_toggle(&mut self) -> bool {
        View::take_wants_sharing_toggle(&mut self.doc)
    }

    fn take_wants_image(&mut self) -> Option<NodeId> {
        View::take_wants_image(&mut self.doc)
    }

    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        View::take_wants_attachment(&mut self.doc)
    }

    fn take_wants_dictation(&mut self) -> bool {
        View::take_wants_dictation(&mut self.doc)
    }

    fn insert_attachment(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        files: &[(koru_core::DocumentId, String)],
    ) {
        View::insert_attachment(&mut self.doc, ws, node, files)
    }

    fn attachment_cancelled(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        View::attachment_cancelled(&mut self.doc, ws, node)
    }

    fn take_wants_file_action(
        &mut self,
    ) -> Option<(koru_core::DocumentId, koru_workspace_host::FileAction)> {
        View::take_wants_file_action(&mut self.doc)
    }

    fn take_wants_save_diagram_as(
        &mut self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::SaveDiagram> {
        View::take_wants_save_diagram_as(&mut self.doc, ws)
    }

    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        View::take_wants_create_tag(&mut self.doc)
    }

    fn take_wants_join_new_list(&mut self) -> Option<(Vec<NodeId>, String, Option<String>)> {
        View::take_wants_join_new_list(&mut self.doc)
    }

    fn take_wants_create_list(&mut self) -> Option<(String, Option<String>)> {
        View::take_wants_create_list(&mut self.doc)
    }

    fn take_wants_list_edits(&mut self) -> Vec<koru_workspace_host::tasks::ListEdit> {
        View::take_wants_list_edits(&mut self.doc)
    }

    fn take_wants_task_commands(&mut self) -> Vec<koru_workspace_host::tasks::TaskCommand> {
        View::take_wants_task_commands(&mut self.doc)
    }

    fn take_wants_task_view(&mut self) -> Option<NodeId> {
        View::take_wants_task_view(&mut self.doc)
    }

    fn task_command_failed(
        &mut self,
        command: &koru_workspace_host::tasks::TaskCommand,
        error: &str,
    ) -> bool {
        View::task_command_failed(&mut self.doc, command, error)
    }

    fn task_command_landed(&mut self, landed: &koru_workspace_host::tasks::Landed) -> bool {
        View::task_command_landed(&mut self.doc, landed)
    }

    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        View::take_wants_workspace_word(&mut self.doc)
    }

    fn set_tag_table(&mut self, tags: &[koru_core::system::TagEntry]) -> bool {
        View::set_tag_table(&mut self.doc, tags)
    }

    fn set_list_table(&mut self, lists: &[koru_core::system::ListEntry]) -> bool {
        View::set_list_table(&mut self.doc, lists)
    }

    fn set_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        View::set_workspace(&mut self.doc, workspace)
    }

    fn take_wants_copy_image(&mut self) -> Option<String> {
        View::take_wants_copy_image(&mut self.doc)
    }

    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        View::take_wants_copy_html(&mut self.doc)
    }

    fn apply_pending_image_cut(&mut self, ws: &mut WorkspaceDoc) -> bool {
        View::apply_pending_image_cut(&mut self.doc, ws)
    }

    fn insert_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        View::insert_image(&mut self.doc, ws, node, hash, mime, width, height);
    }

    fn append_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        View::append_image(&mut self.doc, ws, hash, mime, width, height);
    }

    fn take_wants_paste_image(&mut self) -> Option<koru_workspace_host::PasteTarget> {
        View::take_wants_paste_image(&mut self.doc)
    }

    fn take_wants_paste_rich(&mut self) -> Option<(koru_workspace_host::PasteTarget, String)> {
        View::take_wants_paste_rich(&mut self.doc)
    }

    fn paste_images(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        images: &[koru_workspace_host::PastedImage],
    ) {
        View::paste_images(&mut self.doc, ws, target, images);
    }

    fn paste_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        View::paste_text(&mut self.doc, ws, text);
    }

    fn paste_rich(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        blocks: &[koru_workspace_host::RichBlock],
    ) {
        View::paste_rich(&mut self.doc, ws, target, blocks);
    }

    fn animating(&self) -> bool {
        View::animating(&self.doc)
    }

    fn take_wants_restore_tab(&mut self) -> Option<NodeId> {
        self.wants_restore_tab.then_some(self.node)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        if let Some(req) = self.outgoing.take() {
            return Some(req);
        }
        View::take_window_request(&mut self.doc)
    }

    /// Veto an immediate close when the dialog should be shown: open it, and the
    /// shell leaves the window up. A remembered choice (or an already-started
    /// close) lets the close through.
    fn request_close(&mut self) -> bool {
        if self.closing {
            return false;
        }
        match self.popout_close {
            PopoutClose::Ask => {
                self.close = Some(CloseDialog::new());
                true
            }
            PopoutClose::Close => {
                self.choose(CloseChoice::Close, false);
                false
            }
            PopoutClose::RestoreTab => {
                self.choose(CloseChoice::Restore, false);
                false
            }
        }
    }

    fn take_popout_close_setting(&mut self) -> Option<PopoutClose> {
        self.pending_setting.take()
    }

    fn set_popout_close(&mut self, choice: PopoutClose) {
        self.popout_close = choice;
    }

    fn restore(&mut self, _ws: &Workspace, _settings: &Settings) {}
    fn capture(&self, _settings: &mut Settings) {}
}

/// **Hand one `View` method to whichever window this is.**
///
/// [`ManagerView`] implements [`View`] by hand, one `match` per method, and
/// the shell calls every method on *this* enum — so a method the container
/// implements and this file never names falls through to the trait's default,
/// which is a real implementation and a plausible answer. Nothing fails to
/// compile and nothing is logged; the feature is simply never called.
///
/// It has bitten six times. The sixth was *Change…* in the delegate dialog:
/// the picker opened, a folder was chosen, and
/// [`View::delegate_folder_chosen`] answered `false` here without ever
/// reaching the container — so the row kept the old folder, nothing was
/// stored, and a refused folder said nothing either. Twenty methods were in
/// that state at once, including the crumb a finished run leaves in the
/// document it came from.
///
/// Every method written through this macro is one **both** arms answer for
/// themselves. [`DocumentPane`] implements [`View`] too, so a popped-out
/// window with no opinion inherits exactly the default it would have inherited
/// by being forgotten — what changes is that the container is no longer
/// forgotten alongside it, and adding one is a line rather than six.
macro_rules! forward_to_the_window {
    ($( fn $name:ident(&mut self $(, $arg:ident: $ty:ty)* $(,)?) $(-> $ret:ty)?; )+) => {
        $(
            fn $name(&mut self $(, $arg: $ty)*) $(-> $ret)? {
                match self {
                    Self::Container(view) => view.$name($($arg),*),
                    Self::Document(view) => view.$name($($arg),*),
                }
            }
        )+
    };
}

impl ManagerView {
    /// **The activity drain, and the one place it happens** ([ADR-286]).
    ///
    /// Every event the shell delivers to a window arrives at this enum: it is a
    /// pane's whole view, so a task gesture — a checkbox, a `/due`, a chip's
    /// `×`, a menu row — has finished by the time one of the arms below
    /// returns. That is the moment to drain: the command has run, the document
    /// is no longer borrowed, and the intents the view left behind can go to
    /// the host wrapper that owns their attribution and their scheduling.
    ///
    /// It hangs off the five arms that are handed a `WorkspaceDoc` — the ones
    /// that can have changed something. A pointer move, a scroll or a viewport
    /// change cannot produce a command, so they are not asked.
    ///
    /// Written as a pass-through over the answer rather than a bare call so a
    /// dispatch arm reads as one expression and cannot return early past it.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    fn dispatched(&mut self, ws: &WorkspaceDoc, handled: bool) -> bool {
        let mut revealed = false;
        let owed = match self {
            Self::Container(v) => {
                let owed = v.take_task_activity();
                // With the written document in hand: the mark carries the
                // write's version, so the surfaces can wait for the read
                // model to include it rather than draw the first answer back.
                v.note_task_activity(ws, &owed);
                // **And the ask to be shown on the Tasks page, here, after
                // every dispatch** — its doc said it was drained on this
                // sweep, but it was drained only when a document arrival
                // happened to run the explore gestures, so *View in Tasks*
                // pressed in a document already open did nothing (finding 32).
                revealed = v.drain_task_view_request(ws);
                owed
            }
            Self::Document(v) => Editing::body_mut(&mut v.doc).take_task_activity(),
        };
        // **The invalidation moved inside the drain**, and this is what it used
        // to be: `task_counts_invalidate` here, on the container arm, so the
        // rail's numbers did not wait for a coarse tick after the checkbox you
        // just pressed. It was right about the moment and wrong about the
        // reach — the page in front of you is a third reading of the same
        // facts and was left on that tick, which is how a queue came to say
        // *Nothing waiting* beside a rail that had already counted the row.
        //
        // `MainView::take_task_activity` now marks all three, so the signal has
        // one home rather than two that agree until one of them is edited.
        crate::tasks::attempt(owed);
        self.content_bursts(ws);
        handled || revealed
    }

    /// **The content-burst half of the same drain** ([ADR-286]).
    ///
    /// It hangs here rather than beside the activity sink because the two owe
    /// different things: an intent is a command's answer and goes out as it
    /// stands, while a text edit is one keystroke of a burst nobody has decided
    /// the end of yet. What the editors noted is handed to the window's
    /// [`TaskCaret`], which is the only thing that decides a boundary, and what
    /// it answers is emitted.
    ///
    /// **The caret is read lazily.** `caret_task` is a document read, and the
    /// overwhelming majority of events edit no task and leave no burst open —
    /// so the closure is not called at all unless one of those two is true.
    ///
    /// [`TaskCaret`]: crate::tasks::TaskCaret
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    fn content_bursts(&mut self, ws: &WorkspaceDoc) {
        let ended = match self {
            Self::Container(v) => v.drain_content_bursts(ws),
            Self::Document(v) => {
                let edits = Editing::body_mut(&mut v.doc).take_task_text_edits();
                let DocumentPane {
                    doc, task_caret, ..
                } = v;
                task_caret.dispatched(edits, || Editing::body(doc).caret_task(ws))
            }
        };
        crate::tasks::content_changed(&ended);
    }
}

impl View for ManagerView {
    fn set_window_focused(&mut self, focused: bool) {
        match self {
            Self::Container(view) => view.set_window_focused(focused),
            Self::Document(_) => {}
        }
    }
    // The host holds this wrapper, not MainView. Forward the container's
    // prepared projection and action route; popout semantics remain unsupported.
    fn seat_accessibility(
        &mut self,
        window: koru_workspace_host::accessibility::WindowSlot,
        view: koru_workspace_host::accessibility::ViewSlot,
    ) {
        if let Self::Container(container) = self {
            View::seat_accessibility(container, window, view);
        }
    }

    fn semantics(
        &self,
        view: koru_workspace_host::accessibility::ViewSlot,
        scope: koru_workspace_host::accessibility::Scope,
    ) -> Vec<koru_workspace_host::accessibility::SemanticNode> {
        match self {
            Self::Container(container) => View::semantics(container, view, scope),
            Self::Document(_) => Vec::new(),
        }
    }

    fn accessibility_retired(&self) -> Vec<koru_workspace_host::accessibility::Occurrence> {
        match self {
            Self::Container(container) => View::accessibility_retired(container),
            Self::Document(_) => Vec::new(),
        }
    }

    fn accessibility_focus(
        &self,
        view: koru_workspace_host::accessibility::ViewSlot,
    ) -> Option<koru_workspace_host::accessibility::Occurrence> {
        match self {
            Self::Container(container) => View::accessibility_focus(container, view),
            Self::Document(_) => None,
        }
    }

    fn window_command_bounds(&self) -> koru_workspace_host::accessibility::CaptionGeometry {
        match self {
            Self::Container(container) => View::window_command_bounds(container),
            Self::Document(_) => Default::default(),
        }
    }

    fn semantics_revision(&self) -> u64 {
        match self {
            Self::Container(container) => View::semantics_revision(container),
            Self::Document(_) => 0,
        }
    }

    fn accessibility_action(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: koru_workspace_host::accessibility::Occurrence,
        action: accesskit::Action,
        data: Option<&accesskit::ActionData>,
    ) -> bool {
        match self {
            // The trait route also drains the ordinary post-gesture requests.
            Self::Container(container) => {
                View::accessibility_action(container, ws, target, action, data)
            }
            Self::Document(_) => false,
        }
    }

    fn open_all_reminders(&mut self) -> bool {
        match self {
            Self::Container(c) => c.open_all_reminders(),
            // Reminder chrome belongs to the container caption, not a
            // document that was popped into its own window.
            Self::Document(_) => false,
        }
    }

    fn is_global_search(&self) -> bool {
        match self {
            Self::Container(view) => view.is_global_search(),
            Self::Document(_) => false,
        }
    }

    fn is_feedback_window(&self) -> bool {
        match self {
            Self::Container(view) => view.is_feedback_window(),
            Self::Document(_) => false,
        }
    }

    // ---- the support chat and its icon (ADR-276)

    fn is_support_window(&self) -> bool {
        match self {
            Self::Container(view) => view.is_support_window(),
            Self::Document(_) => false,
        }
    }

    fn is_auxiliary(&self) -> bool {
        match self {
            Self::Container(view) => view.is_auxiliary(),
            Self::Document(_) => false,
        }
    }

    fn floating_window(&self) -> bool {
        match self {
            Self::Container(view) => view.floating_window(),
            Self::Document(_) => false,
        }
    }

    fn fixed_width(&self) -> bool {
        match self {
            Self::Container(view) => view.fixed_width(),
            Self::Document(_) => false,
        }
    }

    fn preferred_window_size(&self) -> Option<((f32, f32), (f32, f32))> {
        match self {
            Self::Container(view) => view.preferred_window_size(),
            Self::Document(_) => None,
        }
    }

    fn attach_surface_host(&mut self, host: koru_workspace_host::native_surface::SurfaceHost) {
        if let Self::Container(view) = self {
            view.attach_surface_host(host);
        }
    }

    fn native_surface(
        &mut self,
    ) -> Option<(
        koru_workspace_host::native_surface::SharedSurface,
        koru_workspace_host::native_surface::Placement,
    )> {
        match self {
            Self::Container(view) => view.native_surface(),
            Self::Document(_) => None,
        }
    }

    fn native_surface_failed(&mut self, error: koru_workspace_host::native_surface::SurfaceError) {
        if let Self::Container(view) = self {
            view.native_surface_failed(error);
        }
    }

    fn take_background_intent(
        &mut self,
    ) -> Option<koru_workspace_host::native_surface::BackgroundIntent> {
        match self {
            Self::Container(view) => view.take_background_intent(),
            Self::Document(_) => None,
        }
    }

    fn background_deadline(&self) -> Option<std::time::Instant> {
        match self {
            Self::Container(view) => view.background_deadline(),
            Self::Document(_) => None,
        }
    }

    fn background_maintain(&mut self, now: std::time::Instant, app_hidden: bool) -> bool {
        match self {
            Self::Container(view) => view.background_maintain(now, app_hidden),
            Self::Document(_) => false,
        }
    }

    fn visible_after_restore(&self) -> bool {
        match self {
            Self::Container(view) => view.visible_after_restore(),
            Self::Document(_) => true,
        }
    }

    fn window_shown(&mut self) {
        if let Self::Container(view) = self {
            view.window_shown();
        }
    }

    fn show_support_reply(&mut self) -> bool {
        match self {
            Self::Container(view) => view.show_support_reply(),
            Self::Document(pane) => pane.show_support_reply(),
        }
    }

    fn popup_chosen(&mut self, id: &str) -> bool {
        match self {
            Self::Container(view) => view.popup_chosen(id),
            Self::Document(_) => false,
        }
    }

    fn open_whats_new(&mut self, version: &str) -> bool {
        match self {
            Self::Container(view) => view.open_whats_new(version),
            Self::Document(_) => false,
        }
    }

    fn open_feedback_tab(&mut self) -> bool {
        match self {
            Self::Container(view) => view.open_send_feedback_tab(),
            Self::Document(_) => false,
        }
    }

    fn close_feedback_tab(&mut self) -> bool {
        match self {
            Self::Container(view) => view.close_feedback_tab(),
            Self::Document(_) => false,
        }
    }

    fn open_search_target(
        &mut self,
        ws: &WorkspaceDoc,
        document: Option<NodeId>,
        node: Option<NodeId>,
        trash_root: Option<NodeId>,
        match_text: Option<&str>,
    ) -> bool {
        match self {
            Self::Container(view) => {
                view.open_search_target(ws, document, node, trash_root, match_text)
            }
            Self::Document(_) => false,
        }
    }

    fn open_file_search_target(&mut self, file: NodeId) -> bool {
        match self {
            Self::Container(view) => view.open_file_search_target(file),
            Self::Document(_) => false,
        }
    }

    fn reseat(&mut self, ws: &WorkspaceDoc) {
        match self {
            Self::Container(c) => c.reseat(ws),
            Self::Document(d) => d.reseat(ws),
        }
    }

    fn show_active_document(
        &mut self,
        ws: &koru_core::schema::WorkspaceDoc,
        active: Option<koru_core::DocumentId>,
    ) -> bool {
        match self {
            Self::Container(c) => c.show_active_document(ws, active),
            // A popped-out window is already showing its one document.
            Self::Document(_) => false,
        }
    }

    fn set_store_trouble(&mut self, trouble: Option<koru_workspace::StoreTrouble>) -> bool {
        match self {
            Self::Container(c) => c.set_store_trouble(trouble),
            // A popped-out document window has no caption of its own to say it
            // in. The container it came from is still saying it.
            Self::Document(_) => false,
        }
    }

    fn set_durability(&mut self, durability: &koru_workspace::durability::Durability) {
        match self {
            Self::Container(c) => c.set_durability(durability),
            // **Not forwarded, and that is a gap rather than a decision about
            // popped-out windows.** `koru-document` publishes no semantics of
            // its own yet, so a view there has nowhere to put this; forwarding
            // it would look like coverage and deliver nothing. Recorded in
            // `docs/features/accessibility/README.md`.
            Self::Document(_) => {}
        }
    }

    fn set_trashed(&mut self, trashed: Vec<koru_core::explore::ExploreEntry>) -> bool {
        match self {
            Self::Container(c) => c.set_trashed(trashed),
            Self::Document(_) => false,
        }
    }

    fn set_entries(
        &mut self,
        entries: Vec<koru_core::explore::ExploreEntry>,
        active: Option<koru_core::DocumentId>,
    ) -> bool {
        match self {
            Self::Container(c) => c.set_entries(entries, active),
            // A popped-out document window has no explore pane to fill.
            Self::Document(_) => false,
        }
    }

    fn complete_new_page_link(
        &mut self,
        ws: &mut WorkspaceDoc,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        document: koru_core::DocumentId,
        title: String,
    ) -> bool {
        match self {
            Self::Container(c) => c.complete_new_page_link(ws, profile, workspace, document, title),
            Self::Document(_) => false,
        }
    }

    fn complete_selection_move(
        &mut self,
        ws: &mut WorkspaceDoc,
        document: koru_core::DocumentId,
        node: Option<koru_core::NodeId>,
        title: String,
    ) -> bool {
        match self {
            Self::Container(c) => c.complete_selection_move(ws, document, node, title),
            // A popped-out document disables the move row; nothing to finish.
            Self::Document(_) => false,
        }
    }

    fn cancel_selection_move(&mut self) -> bool {
        match self {
            Self::Container(c) => c.cancel_selection_move(),
            Self::Document(_) => false,
        }
    }

    fn set_pane_context(
        &mut self,
        profiles: Vec<koru_core::ProfileSummary>,
        current_profile: koru_core::ProfileId,
        workspaces: Vec<koru_core::WorkspaceEntry>,
        current: Option<koru_core::WorkspaceId>,
    ) -> bool {
        match self {
            Self::Container(c) => {
                c.set_pane_context(profiles, current_profile, workspaces, current)
            }
            // A popped-out document window has no pane to put one in.
            Self::Document(_) => false,
        }
    }

    // Account and Profile lifecycle events arrive at this wrapper from the host.
    // Keep the entire round trip here: state, admission, results, and local ACKs.
    fn set_account_state(&mut self, state: koru_agent_wire::AccountState) -> bool {
        match self {
            Self::Container(c) => c.set_account_state(state),
            Self::Document(_) => false,
        }
    }

    fn account_result(
        &mut self,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        match self {
            Self::Container(c) => c.account_result(generation, result),
            Self::Document(_) => false,
        }
    }

    fn account_scoped_result(
        &mut self,
        owner_scope: u64,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        match self {
            Self::Container(c) => c.account_scoped_result(owner_scope, generation, result),
            Self::Document(_) => false,
        }
    }

    fn account_command_admitted(
        &mut self,
        admission: koru_workspace_host::account_ui::CommandAdmission,
    ) -> bool {
        match self {
            Self::Container(c) => c.account_command_admitted(admission),
            Self::Document(_) => false,
        }
    }

    fn account_command_failed(
        &mut self,
        failure: koru_workspace_host::account_ui::CommandFailure,
    ) -> bool {
        match self {
            Self::Container(c) => c.account_command_failed(failure),
            Self::Document(_) => false,
        }
    }

    fn set_profile_workspace_counts(
        &mut self,
        counts: std::collections::HashMap<koru_core::ProfileId, usize>,
    ) -> bool {
        match self {
            Self::Container(c) => c.set_profile_workspace_counts(counts),
            Self::Document(_) => false,
        }
    }

    fn set_profile_workspaces(
        &mut self,
        workspaces: std::collections::HashMap<koru_core::ProfileId, Vec<koru_core::WorkspaceEntry>>,
    ) -> bool {
        match self {
            Self::Container(c) => c.set_profile_workspaces(workspaces),
            Self::Document(_) => false,
        }
    }

    fn set_retained_profiles(&mut self, profiles: Vec<koru_core::ProfileSummary>) -> bool {
        match self {
            Self::Container(c) => c.set_retained_profiles(profiles),
            Self::Document(_) => false,
        }
    }

    fn set_hidden_profile_count(&mut self, count: usize) -> bool {
        match self {
            Self::Container(c) => c.set_hidden_profile_count(count),
            Self::Document(_) => false,
        }
    }

    fn local_profile_account_bound(&mut self, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.local_profile_account_bound(profile),
            Self::Document(_) => false,
        }
    }

    fn local_profile_account_bind_failed(
        &mut self,
        profile: koru_core::ProfileId,
        code: &str,
    ) -> bool {
        match self {
            Self::Container(c) => c.local_profile_account_bind_failed(profile, code),
            Self::Document(_) => false,
        }
    }

    fn connected_profile_creation_failed(&mut self, message: &str) -> bool {
        match self {
            Self::Container(c) => c.connected_profile_creation_failed(message),
            Self::Document(_) => false,
        }
    }

    fn connected_profile_connection_paused(
        &mut self,
        owner_scope: u64,
        profile: koru_core::ProfileId,
        retry: koru_agent_wire::AccountCommand,
    ) -> bool {
        match self {
            Self::Container(c) => {
                c.connected_profile_connection_paused(owner_scope, profile, retry)
            }
            Self::Document(_) => false,
        }
    }

    fn connected_profile_ready(&mut self, owner_scope: u64, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.connected_profile_ready(owner_scope, profile),
            Self::Document(_) => false,
        }
    }

    fn local_profile_restored(&mut self, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.local_profile_restored(profile),
            Self::Document(_) => false,
        }
    }

    fn local_profile_restore_failed(&mut self, message: &str) -> bool {
        match self {
            Self::Container(c) => c.local_profile_restore_failed(message),
            Self::Document(_) => false,
        }
    }

    fn local_profile_removed(&mut self, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.local_profile_removed(profile),
            Self::Document(_) => false,
        }
    }

    fn local_profile_remove_failed(&mut self, profile: koru_core::ProfileId, code: &str) -> bool {
        match self {
            Self::Container(c) => c.local_profile_remove_failed(profile, code),
            Self::Document(_) => false,
        }
    }

    fn deleted_profile_continued_offline(&mut self, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.deleted_profile_continued_offline(profile),
            Self::Document(_) => false,
        }
    }

    fn deleted_profile_continue_offline_failed(&mut self, profile: koru_core::ProfileId) -> bool {
        match self {
            Self::Container(c) => c.deleted_profile_continue_offline_failed(profile),
            Self::Document(_) => false,
        }
    }

    fn show_first_minutes(&mut self, owner: &str, profile: &str, workspace: &str) -> bool {
        match self {
            Self::Container(c) => c.show_first_minutes(owner, profile, workspace),
            Self::Document(_) => false,
        }
    }

    fn set_reminder_startup(&mut self, facts: koru_window::ReminderStartupFacts) -> bool {
        match self {
            Self::Container(c) => c.set_reminder_startup(facts),
            Self::Document(_) => false,
        }
    }

    fn reminder_startup_result(
        &mut self,
        result: koru_workspace_host::ReminderStartupResult,
    ) -> bool {
        match self {
            Self::Container(c) => c.reminder_startup_result(result),
            Self::Document(_) => false,
        }
    }

    /// Native notification Snooze arrives at the shell's `ManagerView`, not
    /// directly at `MainView`. Falling through to `View`'s default `false`
    /// silently discarded the validated gesture after navigation succeeded.
    fn reminder_snooze(&mut self, intent: koru_workspace_host::ReminderSnoozeIntent) -> bool {
        match self {
            Self::Container(c) => c.reminder_snooze(intent),
            // A popped-out document has no reminder bell or All Reminders
            // surface in which to own the duration picker.
            Self::Document(_) => false,
        }
    }

    /// **Both of these have to be forwarded, and both were missed.**
    ///
    /// The shell pushes to a *pane*, and a pane's view is this enum — so a
    /// default `false` here is not "this window does not want it", it is the
    /// message being dropped on the floor one level above the thing that wanted
    /// it. The manager showed a dash in every column for exactly this reason,
    /// and the error dialog could not be raised at all.
    fn set_workspace_stats(&mut self, stats: Vec<koru_workspace_host::WorkspaceStat>) -> bool {
        match self {
            Self::Container(c) => c.set_workspace_stats(stats),
            Self::Document(_) => false,
        }
    }

    /// Move to… listings, Ctrl+M, and transfer failure — same floor-drop trap
    /// as workspace stats: the shell talks to this enum, not to `MainView`.
    fn set_move_to_workspace_entries(
        &mut self,
        workspace: koru_core::WorkspaceId,
        entries: Vec<koru_core::explore::ExploreEntry>,
    ) -> bool {
        match self {
            Self::Container(c) => c.set_move_to_workspace_entries(workspace, entries),
            Self::Document(_) => false,
        }
    }

    fn open_move_to(&mut self) -> bool {
        match self {
            Self::Container(c) => c.open_move_to(),
            Self::Document(_) => false,
        }
    }

    fn transfer_failed(&mut self) -> bool {
        match self {
            Self::Container(c) => c.transfer_failed(),
            Self::Document(_) => false,
        }
    }

    fn report_error(&mut self, error: koru_workspace_host::ErrorReport) -> bool {
        match self {
            Self::Container(c) => c.report_error(error),
            // A popped-out document window draws no modal of its own, so it
            // declines and the shell falls back to a native dialog rather than
            // to silence.
            Self::Document(_) => false,
        }
    }

    fn show_notice(&mut self, message: String) -> bool {
        match self {
            Self::Container(c) => c.show_notice(message),
            Self::Document(_) => false,
        }
    }

    fn show_notice_reveal(&mut self, message: String, path: std::path::PathBuf) -> bool {
        match self {
            Self::Container(c) => c.show_notice_reveal(message, path),
            Self::Document(_) => false,
        }
    }

    fn show_import_progress(
        &mut self,
        message: String,
        progress: koru_workspace_host::ImportNoticeProgress,
    ) -> bool {
        match self {
            Self::Container(c) => c.show_import_progress(message, progress),
            Self::Document(_) => false,
        }
    }

    fn update_import_progress(
        &mut self,
        message: String,
        progress: koru_workspace_host::ImportNoticeProgress,
    ) -> bool {
        match self {
            Self::Container(c) => c.update_import_progress(message, progress),
            Self::Document(_) => false,
        }
    }

    fn cancel_import_progress(&mut self, message: String) -> bool {
        match self {
            Self::Container(c) => c.cancel_import_progress(message),
            Self::Document(_) => false,
        }
    }

    fn clear_import_progress(&mut self) -> bool {
        match self {
            Self::Container(c) => c.clear_import_progress(),
            Self::Document(_) => false,
        }
    }

    fn show_import_notice(
        &mut self,
        message: String,
        target: koru_workspace_host::ImportNoticeTarget,
    ) -> bool {
        match self {
            Self::Container(c) => c.show_import_notice(message, target),
            Self::Document(_) => false,
        }
    }

    fn show_import_preview(&mut self, preview: koru_workspace_host::ImportPreview) -> bool {
        match self {
            Self::Container(c) => c.show_import_preview(preview),
            // Import belongs to the workspace manager; a popped-out document
            // has no surface for this modal.
            Self::Document(_) => false,
        }
    }

    fn take_pane_request(&mut self) -> Option<koru_workspace_host::PaneRequest> {
        match self {
            Self::Container(c) => c.take_pane_request(),
            Self::Document(pane) => pane.take_pane_request(),
        }
    }

    fn create_document_failed(&mut self) {
        if let Self::Container(c) = self {
            koru_workspace_host::View::create_document_failed(c);
        }
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let handled = match self {
            Self::Container(v) => v.key(ws, key, text, mods),
            Self::Document(v) => v.key(ws, key, text, mods),
        };
        self.dispatched(ws, handled)
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        let handled = match self {
            Self::Container(v) => v.mouse_down(ws, x, y, ctrl),
            Self::Document(v) => v.mouse_down(ws, x, y, ctrl),
        };
        self.dispatched(ws, handled)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        match self {
            Self::Container(v) => v.mouse_move(x, y),
            Self::Document(v) => v.mouse_move(x, y),
        }
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let handled = match self {
            Self::Container(v) => v.mouse_up(ws),
            Self::Document(v) => v.mouse_up(ws),
        };
        self.dispatched(ws, handled)
    }

    fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        let handled = match self {
            Self::Container(v) => v.context_menu(ws, x, y),
            Self::Document(v) => v.context_menu(ws, x, y),
        };
        self.dispatched(ws, handled)
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        let handled = match self {
            Self::Container(v) => v.mouse_double_click(ws, x, y),
            Self::Document(v) => v.mouse_double_click(ws, x, y),
        };
        self.dispatched(ws, handled)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        match self {
            Self::Container(v) => v.scroll_by(delta),
            Self::Document(v) => v.scroll_by(delta),
        }
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        match self {
            Self::Container(v) => v.set_viewport(w, h),
            Self::Document(v) => v.set_viewport(w, h),
        }
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        match self {
            Self::Container(v) => v.set_mods(mods),
            Self::Document(v) => v.set_mods(mods),
        }
    }

    fn set_maximized(&mut self, maximized: bool) {
        match self {
            Self::Container(v) => v.set_maximized(maximized),
            Self::Document(v) => v.set_maximized(maximized),
        }
    }

    fn set_pinned(&mut self, pinned: bool) {
        match self {
            Self::Container(v) => v.set_pinned(pinned),
            Self::Document(v) => v.set_pinned(pinned),
        }
    }

    fn cursor_icon(&self) -> CursorIcon {
        match self {
            Self::Container(v) => v.cursor_icon(),
            Self::Document(v) => v.cursor_icon(),
        }
    }

    fn adopt(&mut self, ws: &WorkspaceDoc) {
        match self {
            Self::Container(v) => v.adopt(ws),
            Self::Document(v) => v.adopt(ws),
        }
    }

    fn invalidate(&mut self) {
        match self {
            Self::Container(v) => v.invalidate(),
            Self::Document(v) => v.invalidate(),
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        match self {
            Self::Container(v) => v.draw(scene, workspace, images, root, caret_on, pointer),
            Self::Document(v) => v.draw(scene, workspace, images, root, caret_on, pointer),
        }
    }

    fn document_root(&self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.document_root(),
            Self::Document(v) => v.document_root(),
        }
    }

    fn tab_presence(&self, document: koru_core::DocumentId) -> koru_workspace_host::TabPresence {
        match self {
            Self::Container(v) => v.tab_presence(document),
            Self::Document(v) => v.tab_presence(document),
        }
    }

    fn unpin_document(&mut self, document: koru_core::DocumentId) -> bool {
        match self {
            Self::Container(v) => v.unpin_document(document),
            Self::Document(_) => false,
        }
    }

    fn pinned_search_rows(
        &self,
    ) -> Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)> {
        match self {
            Self::Container(v) => v.pinned_search_rows(),
            Self::Document(_) => Vec::new(),
        }
    }

    fn take_wants_open(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_open(),
            Self::Document(v) => v.take_wants_open(),
        }
    }

    fn take_wants_stay_focused(&mut self) -> bool {
        match self {
            Self::Container(v) => v.take_wants_stay_focused(),
            Self::Document(_) => false,
        }
    }

    fn open_background_tab(
        &mut self,
        ws: &WorkspaceDoc,
        document: koru_core::DocumentId,
        home: Option<koru_core::WorkspaceId>,
    ) -> bool {
        match self {
            Self::Container(v) => v.open_background_tab(ws, document, home),
            Self::Document(_) => false,
        }
    }

    fn take_wants_popout(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_popout(),
            Self::Document(_) => None,
        }
    }

    fn pane_drag_document(&self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.pane_drag_document(),
            Self::Document(_) => None,
        }
    }

    fn foreign_tab_hover(&mut self, ws: &WorkspaceDoc, doc: NodeId, x: f32, y: f32) -> bool {
        match self {
            Self::Container(v) => v.foreign_tab_hover(ws, doc, x, y),
            Self::Document(_) => false,
        }
    }

    fn foreign_tab_leave(&mut self) {
        match self {
            Self::Container(v) => {
                v.foreign_tab_leave();
            }
            Self::Document(_) => {}
        }
    }

    fn foreign_tab_drop(&mut self, ws: &WorkspaceDoc, doc: NodeId, x: f32, y: f32) -> bool {
        match self {
            Self::Container(v) => v.foreign_tab_drop(ws, doc, x, y),
            Self::Document(_) => false,
        }
    }

    fn complete_foreign_drop(&mut self) {
        match self {
            Self::Container(v) => v.complete_foreign_drop(),
            Self::Document(_) => {}
        }
    }

    fn set_foreign_targeted(&mut self, targeted: bool) -> bool {
        match self {
            Self::Container(v) => v.set_foreign_targeted(targeted),
            Self::Document(_) => false,
        }
    }

    fn take_wants_session_popout(&mut self) -> Option<koru_session_protocol::SessionId> {
        match self {
            Self::Container(v) => v.take_wants_session_popout(),
            Self::Document(_) => None,
        }
    }

    fn shows_session(&self, session: koru_session_protocol::SessionId) -> bool {
        match self {
            Self::Container(v) => v.shows_session(session),
            Self::Document(_) => false,
        }
    }

    fn open_new_session_chooser(&mut self) -> bool {
        match self {
            Self::Container(v) => v.open_new_session_chooser(),
            Self::Document(_) => false,
        }
    }

    fn open_session_tab(&mut self, session: koru_session_protocol::SessionId) -> bool {
        match self {
            Self::Container(v) => v.open_session_tab(session),
            Self::Document(_) => false,
        }
    }

    fn take_wants_restore_tab(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_restore_tab(),
            Self::Document(v) => v.take_wants_restore_tab(),
        }
    }

    fn restore_tab(&mut self, ws: &WorkspaceDoc, doc: NodeId) {
        if let Self::Container(v) = self {
            v.restore_tab(ws, doc);
        }
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        match self {
            Self::Container(v) => v.take_window_request(),
            Self::Document(v) => v.take_window_request(),
        }
    }

    fn request_close(&mut self) -> bool {
        match self {
            Self::Container(v) => v.request_close(),
            Self::Document(v) => v.request_close(),
        }
    }

    fn is_container(&self) -> bool {
        match self {
            Self::Container(_) => true,
            Self::Document(_) => false,
        }
    }

    fn take_wants_sharing_toggle(&mut self) -> bool {
        match self {
            Self::Container(v) => v.take_wants_sharing_toggle(),
            Self::Document(v) => v.take_wants_sharing_toggle(),
        }
    }

    fn take_wants_image(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_image(),
            Self::Document(v) => v.take_wants_image(),
        }
    }

    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_attachment(),
            Self::Document(v) => v.take_wants_attachment(),
        }
    }

    fn take_wants_dictation(&mut self) -> bool {
        match self {
            Self::Container(v) => v.take_wants_dictation(),
            Self::Document(v) => v.take_wants_dictation(),
        }
    }

    // **Six voice methods used to stop here.** `ManagerView` forwards the
    // `View` trait by hand, so a method added to `MainView` and not listed
    // again in this file silently falls through to the trait default and is
    // never called - the hazard AGENTS.md records as having bitten four
    // times. It bit a fifth: every one of these defaulted, so dictation never
    // reached a surface and `show_voice_consent` answered `false` for a view
    // that has a perfectly good consent step. The visible symptom was a toast
    // saying another question was already on screen when none was.
    fn show_voice_consent(
        &mut self,
        offer: koru_workspace_host::voice_install::VoiceOffer,
    ) -> bool {
        match self {
            Self::Container(v) => v.show_voice_consent(offer),
            Self::Document(v) => v.show_voice_consent(offer),
        }
    }

    fn dictate(&mut self, ws: &mut WorkspaceDoc, action: &koru_dictation::DictationAction) -> bool {
        match self {
            Self::Container(v) => v.dictate(ws, action),
            Self::Document(v) => v.dictate(ws, action),
        }
    }

    fn dictation_status(&mut self, status: Option<koru_dictation::Status>) -> bool {
        match self {
            Self::Container(v) => v.dictation_status(status),
            Self::Document(v) => v.dictation_status(status),
        }
    }

    fn dictation_hit(&self, x: f32, y: f32) -> bool {
        match self {
            Self::Container(v) => v.dictation_hit(x, y),
            Self::Document(v) => v.dictation_hit(x, y),
        }
    }

    fn dictation_refused(&mut self, refusal: koru_dictation::Refusal) -> bool {
        match self {
            Self::Container(v) => v.dictation_refused(refusal),
            Self::Document(v) => v.dictation_refused(refusal),
        }
    }

    fn voice_models_changed(&mut self) -> bool {
        match self {
            Self::Container(v) => v.voice_models_changed(),
            Self::Document(v) => v.voice_models_changed(),
        }
    }

    // The download surface, and the same trap: with these defaulted the
    // consent step could be answered and then nothing would report progress,
    // because the trait falls back to a plain notice or to nothing at all.
    fn show_voice_download(&mut self, message: String, fraction: Option<f32>) -> bool {
        match self {
            Self::Container(v) => v.show_voice_download(message, fraction),
            Self::Document(v) => v.show_voice_download(message, fraction),
        }
    }

    fn update_voice_download(&mut self, message: String, fraction: Option<f32>) -> bool {
        match self {
            Self::Container(v) => v.update_voice_download(message, fraction),
            Self::Document(v) => v.update_voice_download(message, fraction),
        }
    }

    fn cancel_voice_download(&mut self, message: String) -> bool {
        match self {
            Self::Container(v) => v.cancel_voice_download(message),
            Self::Document(v) => v.cancel_voice_download(message),
        }
    }

    fn clear_voice_download(&mut self) -> bool {
        match self {
            Self::Container(v) => v.clear_voice_download(),
            Self::Document(v) => v.clear_voice_download(),
        }
    }

    fn insert_attachment(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        files: &[(koru_core::DocumentId, String)],
    ) {
        match self {
            Self::Container(v) => v.insert_attachment(ws, node, files),
            Self::Document(v) => v.insert_attachment(ws, node, files),
        }
    }

    fn attachment_cancelled(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        match self {
            Self::Container(v) => v.attachment_cancelled(ws, node),
            Self::Document(v) => v.attachment_cancelled(ws, node),
        }
    }

    fn take_wants_file_action(
        &mut self,
    ) -> Option<(koru_core::DocumentId, koru_workspace_host::FileAction)> {
        match self {
            Self::Container(v) => v.take_wants_file_action(),
            Self::Document(v) => v.take_wants_file_action(),
        }
    }

    fn take_wants_save_diagram_as(
        &mut self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::SaveDiagram> {
        match self {
            Self::Container(v) => v.take_wants_save_diagram_as(ws),
            Self::Document(v) => v.take_wants_save_diagram_as(ws),
        }
    }

    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        match self {
            Self::Container(v) => v.take_wants_workspace_word(),
            Self::Document(v) => v.take_wants_workspace_word(),
        }
    }

    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        match self {
            Self::Container(v) => v.take_wants_create_tag(),
            Self::Document(v) => v.take_wants_create_tag(),
        }
    }

    fn take_wants_join_new_list(&mut self) -> Option<(Vec<NodeId>, String, Option<String>)> {
        match self {
            Self::Container(v) => v.take_wants_join_new_list(),
            Self::Document(v) => v.take_wants_join_new_list(),
        }
    }

    fn take_wants_create_list(&mut self) -> Option<(String, Option<String>)> {
        match self {
            Self::Container(v) => v.take_wants_create_list(),
            Self::Document(v) => v.take_wants_create_list(),
        }
    }

    fn take_wants_list_edits(&mut self) -> Vec<koru_workspace_host::tasks::ListEdit> {
        match self {
            Self::Container(v) => v.take_wants_list_edits(),
            Self::Document(v) => v.take_wants_list_edits(),
        }
    }

    /// **Every task the Tasks page asked for, on its way to the one hand that
    /// can write it.**
    ///
    /// This arm did not exist, and its absence is the whole of acceptance
    /// findings 1, 3 and 4. `View::take_wants_task_commands` has a default that
    /// answers `Vec::new()` — right for the twenty views that never queue one
    /// — and this enum is what the shell holds, so the sweep asked the wrapper,
    /// the wrapper answered "nothing", and every tick, every removal and every
    /// capture sat in `MainView::task_commands` for ever. No error, no toast,
    /// nothing in the log: the queue was never read, so nothing ever failed.
    ///
    /// The three drains above it were forwarded, which is why making a list
    /// worked and ticking a task did not.
    fn take_wants_task_commands(&mut self) -> Vec<koru_workspace_host::tasks::TaskCommand> {
        match self {
            Self::Container(v) => v.take_wants_task_commands(),
            Self::Document(v) => v.take_wants_task_commands(),
        }
    }

    /// **Forwarded, and written the same turn the arm was added.** The last
    /// drain to reach this enum was `take_wants_task_commands`, whose missing
    /// arm was acceptance findings 1, 3 and 4: a default that answers `None` is
    /// right for twenty views and silent for the one that meant it. A
    /// popped-out document raises this too, and its answer is drained by the
    /// container it came from.
    fn take_wants_task_view(&mut self) -> Option<NodeId> {
        match self {
            Self::Container(v) => v.take_wants_task_view(),
            Self::Document(v) => v.take_wants_task_view(),
        }
    }

    /// And the way back, for the same reason it exists at all: a refused
    /// `Create` has taken somebody's typing with it, and the view can only put
    /// the words back if it is told. Forwarded here or the capture row stays
    /// empty after a failure nobody was shown.
    fn task_command_failed(
        &mut self,
        command: &koru_workspace_host::tasks::TaskCommand,
        error: &str,
    ) -> bool {
        match self {
            Self::Container(v) => v.task_command_failed(command, error),
            Self::Document(v) => v.task_command_failed(command, error),
        }
    }

    /// The receipt, forwarded for the reason the refusal is: a default that
    /// answers nothing is right for every view but the one that holds the
    /// Tasks surfaces, and that one is behind this enum.
    fn task_command_landed(&mut self, landed: &koru_workspace_host::tasks::Landed) -> bool {
        match self {
            Self::Container(v) => v.task_command_landed(landed),
            Self::Document(v) => v.task_command_landed(landed),
        }
    }

    fn set_tag_table(&mut self, tags: &[koru_core::system::TagEntry]) -> bool {
        match self {
            Self::Container(v) => v.set_tag_table(tags),
            Self::Document(v) => v.set_tag_table(tags),
        }
    }

    fn set_list_table(&mut self, lists: &[koru_core::system::ListEntry]) -> bool {
        match self {
            Self::Container(v) => v.set_list_table(lists),
            Self::Document(v) => v.set_list_table(lists),
        }
    }

    fn set_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        match self {
            Self::Container(v) => v.set_workspace(workspace),
            Self::Document(v) => v.set_workspace(workspace),
        }
    }

    fn take_wants_copy_image(&mut self) -> Option<String> {
        match self {
            Self::Container(v) => v.take_wants_copy_image(),
            Self::Document(v) => v.take_wants_copy_image(),
        }
    }

    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        match self {
            Self::Container(v) => v.take_wants_copy_html(),
            Self::Document(v) => v.take_wants_copy_html(),
        }
    }

    fn apply_pending_image_cut(&mut self, ws: &mut WorkspaceDoc) -> bool {
        match self {
            Self::Container(v) => v.apply_pending_image_cut(ws),
            Self::Document(v) => v.apply_pending_image_cut(ws),
        }
    }

    fn insert_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        match self {
            Self::Container(v) => v.insert_image(ws, node, hash, mime, width, height),
            Self::Document(v) => v.insert_image(ws, node, hash, mime, width, height),
        }
    }

    fn append_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        match self {
            Self::Container(v) => v.append_image(ws, hash, mime, width, height),
            Self::Document(v) => v.append_image(ws, hash, mime, width, height),
        }
    }

    fn take_wants_paste_image(&mut self) -> Option<koru_workspace_host::PasteTarget> {
        match self {
            Self::Container(v) => v.take_wants_paste_image(),
            Self::Document(v) => v.take_wants_paste_image(),
        }
    }

    fn take_wants_paste_rich(&mut self) -> Option<(koru_workspace_host::PasteTarget, String)> {
        match self {
            Self::Container(v) => v.take_wants_paste_rich(),
            Self::Document(v) => v.take_wants_paste_rich(),
        }
    }

    fn paste_images(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        images: &[koru_workspace_host::PastedImage],
    ) {
        match self {
            Self::Container(v) => v.paste_images(ws, target, images),
            Self::Document(v) => v.paste_images(ws, target, images),
        }
    }

    fn paste_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        match self {
            Self::Container(v) => v.paste_text(ws, text),
            Self::Document(v) => v.paste_text(ws, text),
        }
    }

    fn paste_rich(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        blocks: &[koru_workspace_host::RichBlock],
    ) {
        match self {
            Self::Container(v) => v.paste_rich(ws, target, blocks),
            Self::Document(v) => v.paste_rich(ws, target, blocks),
        }
    }

    fn animating(&self) -> bool {
        match self {
            Self::Container(v) => v.animating(),
            Self::Document(v) => v.animating(),
        }
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        match self {
            Self::Container(v) => v.maintenance_deadline(),
            Self::Document(v) => v.maintenance_deadline(),
        }
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        match self {
            Self::Container(v) => v.maintain(ws, now),
            Self::Document(v) => v.maintain(ws, now),
        }
    }

    fn cycle_tab(&mut self, delta: i32) -> bool {
        match self {
            Self::Container(v) => v.cycle_tab(delta),
            Self::Document(v) => v.cycle_tab(delta),
        }
    }

    /// **Forgetting this arm is how Alt+Arrow and the mouse's thumb buttons did
    /// nothing at all**: the shell calls every `View` method on *this* enum, so
    /// a method it does not forward silently falls back to the trait's no-op.
    /// Every arm above exists for the same reason.
    fn navigate_history(&mut self, ws: &WorkspaceDoc, delta: i32) -> bool {
        match self {
            Self::Container(v) => v.navigate_history(ws, delta),
            // A popped-out document is one document in one window; there is
            // nowhere it has been.
            Self::Document(_) => false,
        }
    }

    fn restore(&mut self, ws: &Workspace, settings: &Settings) {
        match self {
            Self::Container(v) => v.restore(ws, settings),
            Self::Document(v) => v.restore(ws, settings),
        }
    }

    fn capture(&self, settings: &mut Settings) {
        match self {
            Self::Container(v) => v.capture(settings),
            Self::Document(v) => v.capture(settings),
        }
    }

    fn take_popout_close_setting(&mut self) -> Option<PopoutClose> {
        match self {
            Self::Container(v) => v.take_popout_close_setting(),
            Self::Document(v) => v.take_popout_close_setting(),
        }
    }

    fn set_popout_close(&mut self, choice: PopoutClose) {
        match self {
            Self::Container(v) => v.set_popout_close(choice),
            Self::Document(v) => v.set_popout_close(choice),
        }
    }

    // ---- what only a container answers, forwarded once each
    //
    // Thirteen of these were hand-written on dev in the same window this
    // branch macro'd them: the account family below is dev's, arm by arm,
    // and duplicating it here is a compile error rather than a silent
    // disagreement. What is left is what only this branch forwards.
    //
    // See `forward_to_the_window!`. These reached the trait's default until
    // ADR-367 — the account panel, the profile list's own answers, the first
    // minutes of a new profile, the crumb a finished run leaves behind, and
    // the folder a delegated run is given. SESSIONRECORD added the path a
    // recording is saved to, the same trap one window over.
    forward_to_the_window! {
        fn adopt_preferences(&mut self, settings: &Settings);
        fn adopt_startup_preferences(&mut self, settings: &Settings);
        fn delegate_folder_chosen(
            &mut self,
            chosen: koru_workspace_host::DelegateFolder,
        ) -> bool;
        fn on_workspace_switched(&mut self, ws: &Workspace, settings: &Settings);
        fn recording_path_chosen(&mut self, chosen: std::path::PathBuf) -> bool;
        fn ai_chat_folder_chosen(&mut self, chosen: std::path::PathBuf) -> bool;
        fn recording_event(&mut self, event: koru_workspace_host::RecordingEvent) -> bool;
        fn take_recording_telemetry(&mut self) -> Option<koru_recording::SessionTelemetry>;
        fn set_file_fetching(&mut self, file: koru_core::DocumentId, fetching: bool);
        fn set_remote_files(&mut self, remote: Vec<koru_core::DocumentId>) -> bool;
        fn show_notice_open_another_profile(&mut self, message: String) -> bool;
        fn take_wants_run_pills(&mut self) -> Vec<koru_workspace_host::runpill::RunPill>;
        fn take_wants_schedule_pills(&mut self) -> Vec<koru_workspace_host::schedulepill::SchedulePill>;
        // A canvas tab claims the zoom gestures from the display zoom (CANVAS
        // P1.4); a popped-out page has none of its own, and declines by default.
        fn zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) -> bool;
        fn zoom_key(&mut self, intent: koru_workspace_host::ZoomIntent) -> bool;
        // A canvas writes a held arrow key's nudge when the key comes up; a
        // popped-out page ends nothing on a release, and declines by default.
        fn key_up(&mut self, ws: &mut WorkspaceDoc, key: &Key) -> bool;
    }
}

// ============================================================== the close dialog

/// What a close-choice button does.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum CloseChoice {
    /// Close the window and restore the document as a tab in the container.
    Restore,
    /// Close the window and leave the document closed everywhere.
    Close,
}

/// Which interactive part of the dialog a point is over.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum CloseHit {
    Restore,
    Close,
    Remember,
}

/// The modal that asks what closing a popped-out document window should do.
///
/// Geometry only — like the tab menu, it knows nothing about documents. The
/// pane decides what a choice means; this just lays out, hit-tests and draws.
struct CloseDialog {
    hover: Option<CloseHit>,
    remember: bool,
}

impl CloseDialog {
    fn new() -> Self {
        Self {
            hover: None,
            remember: false,
        }
    }
}

// Metrics, kept in step with the other floating overlays.
const DLG_W: f32 = 320.0;
const DLG_PAD: f32 = 18.0;
const DLG_RADIUS: f32 = 8.0;
const DLG_BTN_H: f32 = 32.0;
const DLG_BTN_GAP: f32 = 10.0;
const DLG_CHECK: f32 = 14.0;

impl CloseDialog {
    /// The panel's height, summed from its rows so it cannot drift from drawing.
    fn height() -> f32 {
        // title + subtitle + buttons + remember row, each with its own band.
        DLG_PAD * 2.0 + theme::PILL_FONT_SIZE + 6.0 + 13.0 + 10.0 + DLG_BTN_H + 12.0 + DLG_CHECK
    }

    /// Where the panel sits, centred in the viewport.
    fn panel(&self, viewport: (f32, f32)) -> Rect {
        let h = Self::height();
        let x = (viewport.0 - DLG_W).max(8.0) * 0.5;
        let y = (viewport.1 - h).max(8.0) * 0.5;
        Rect::new(x, y, DLG_W, h)
    }

    fn buttons(&self, viewport: (f32, f32)) -> (Rect, Rect) {
        let p = self.panel(viewport);
        let y = p.y + DLG_PAD + theme::PILL_FONT_SIZE + 6.0 + 13.0 + 10.0;
        let w = (p.w - DLG_PAD * 2.0 - DLG_BTN_GAP) * 0.5;
        let left = Rect::new(p.x + DLG_PAD, y, w, DLG_BTN_H);
        let right = Rect::new(left.x + w + DLG_BTN_GAP, y, w, DLG_BTN_H);
        (left, right)
    }

    fn remember_rect(&self, viewport: (f32, f32)) -> Rect {
        let p = self.panel(viewport);
        let y = self.buttons(viewport).0.y + DLG_BTN_H + 12.0;
        Rect::new(p.x + DLG_PAD, y, DLG_CHECK, DLG_CHECK)
    }

    /// Which part sits under `(x, y)`, or `None` outside the panel.
    fn hit_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<CloseHit> {
        let p = self.panel(viewport);
        if !p.contains(x, y) {
            return None;
        }
        let (restore, close) = self.buttons(viewport);
        if restore.contains(x, y) {
            return Some(CloseHit::Restore);
        }
        if close.contains(x, y) {
            return Some(CloseHit::Close);
        }
        // The checkbox hit includes its label, the whole width to the panel edge.
        let r = self.remember_rect(viewport);
        if y >= r.y && y <= r.y + r.h {
            return Some(CloseHit::Remember);
        }
        None
    }

    fn draw(&self, scene: &mut Scene, root: Affine, engine: &mut TextEngine, viewport: (f32, f32)) {
        let p = self.panel(viewport);
        let pal = theme::p();

        // A scrim over the whole window, so the editor behind reads as inert.
        scene::fill(
            scene,
            root,
            pal.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );

        // Shadow, body, border — the recipe every floating surface shares.
        scene::floating_panel(
            scene,
            root,
            p.x,
            p.y,
            p.w,
            p.h,
            &scene::Panel::popover(DLG_RADIUS),
        );

        let title = engine.simple_layout("Close this window?", theme::role::row());
        scene::draw_layout_first_line(
            scene,
            root,
            &title,
            p.x + DLG_PAD,
            p.y + DLG_PAD,
            Some(pal.text),
        );

        let sub = engine.simple_layout(
            "Restore the document as a tab, or close it.",
            theme::role::row(),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &sub,
            p.x + DLG_PAD,
            p.y + DLG_PAD + theme::PILL_FONT_SIZE + 6.0,
            Some(pal.text_dim),
        );

        let (restore, close) = self.buttons(viewport);
        self.draw_button(
            scene,
            root,
            engine,
            restore,
            "Restore tab",
            self.hover == Some(CloseHit::Restore),
            true,
        );
        self.draw_button(
            scene,
            root,
            engine,
            close,
            "Close",
            self.hover == Some(CloseHit::Close),
            false,
        );

        // The "remember" checkbox and its label.
        let r = self.remember_rect(viewport);
        let box_colour = if self.remember {
            pal.accent
        } else {
            pal.panel_border
        };
        scene::stroke(
            scene,
            root,
            box_colour,
            theme::BORDER_W,
            &scene::rrect(r.x, r.y, r.w, r.h, 3.0),
        );
        if self.remember {
            let tick = engine.icon_layout(icons::CHECK, theme::CAPTION_ICON);
            scene::draw_layout(
                scene,
                root,
                &tick,
                r.x + (r.w - tick.width()) * 0.5,
                r.y + (r.h - tick.height()) * 0.5,
                Some(pal.accent),
            );
        }
        let label = engine.simple_layout("Remember my choice", theme::role::row());
        scene::draw_layout_first_line(
            scene,
            root,
            &label,
            r.x + r.w + 8.0,
            r.y + (r.h - label.height()) * 0.5,
            Some(if self.hover == Some(CloseHit::Remember) {
                pal.text
            } else {
                pal.text_dim
            }),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        label: &str,
        hot: bool,
        primary: bool,
    ) {
        let pal = theme::p();
        let (fill_colour, text_colour) = if primary {
            (pal.accent, pal.on_accent)
        } else if hot {
            (pal.hover_tint, pal.text)
        } else {
            (pal.row_hint, pal.text)
        };
        scene::fill(
            scene,
            root,
            fill_colour,
            &scene::rrect(r.x, r.y, r.w, r.h, 6.0),
        );
        let layout = engine.simple_layout(label, theme::role::row());
        let line_h = layout
            .lines()
            .next()
            .map_or(0.0, |l| l.metrics().line_height);
        scene::draw_layout_first_line(
            scene,
            root,
            &layout,
            r.x + (r.w - layout.width()) * 0.5,
            r.y + (r.h - line_h) * 0.5,
            Some(text_colour),
        );
    }
}

#[cfg(test)]
mod tests;
