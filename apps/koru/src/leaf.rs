// SPDX-License-Identifier: MPL-2.0

//! What a tab bottoms out at: the view that edits a list of paragraphs.
//!
//! Named `Leaf` because it is the end of the containment chain — window holds
//! container holds tab holds *this*, and this holds rows. Everything above it
//! can be several things at once; a leaf is exactly one view.
//!
//! ## Why this is not the same enum as `TabView`
//!
//! A tab may also show a welcome screen, which has no rows, no `Body` and no
//! `ViewState`. Putting that arm here would force [`Editing::parts`] to be
//! `unreachable!()` for it. Two levels of enum instead — `TabView` above,
//! implementing only [`View`]; `Leaf` here, implementing both — turn that into
//! a compile error rather than a panic, and confine the trait overlap below to
//! this one file.
//!
//! ## Why the inherent methods
//!
//! `Leaf` implements two traits that name some of the same methods, and that
//! is not an accident: [`Editing`] supplies the *shared* behaviour every list of
//! paragraphs has, while [`View`] is what the shell asks of a window and reaches
//! the view's own override. `Editing::key` declines the structural actions;
//! `View::key` performs them.
//!
//! Writing `leaf.key(..)` would pick between them by whichever trait happened
//! to be imported at the call site — and picking wrong is silent: every
//! structural test fails and nothing says why. So the real dispatch is spelled
//! once, as inherent methods, which win name resolution over both traits. Both
//! trait impls delegate here.

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use koru_document::BuiltInDocument;
use koru_editor::{Body, Editing, OpenWhere, ViewState};
use koru_workspace::Workspace;
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;

/// One view: a document.
///
/// Still an enum with one variant rather than a struct. There were two — the
/// task outline was the other, and it is gone with the realm it showed
/// ([ADR-90](../../../docs/decisions/README.md)). **A canvas is not a second
/// arm here**: it is not a list of paragraphs, so it is its own
/// `TabView::Canvas` arm one level up, beside the Trash and the Tasks page, for
/// the reason `mainview/tab.rs` gives — an arm here would have to answer
/// `Editing::parts` with nothing. A kanban would follow the canvas, not this.
/// Collapsing it to a struct now would mean touching every `match` here twice.
pub enum Leaf {
    Document(BuiltInDocument),
}

impl Leaf {
    pub fn document(ws: &WorkspaceDoc, doc: NodeId) -> Self {
        Self::Document(BuiltInDocument::open(ws, doc))
    }

    /// The explore rows' Attributes submenu, landing on the now-active
    /// document: open the named attribute picker over its root node.
    pub fn open_document_attr_picker(
        &mut self,
        ws: &WorkspaceDoc,
        which: koru_editor::AttrPicker,
    ) -> bool {
        match self {
            Self::Document(d) => d.open_document_attr_picker(ws, which),
        }
    }

    /// Whether the caret is sitting in a document's title row.
    ///
    /// The container asks so it can hold the tab strip and the explore tree
    /// still while a name is being typed — see `MainView::naming`. A view with
    /// no title row always answers `false`.
    pub fn editing_title(&self) -> bool {
        match self {
            Self::Document(d) => d.view().active && d.body().focus_is_title(),
        }
    }

    /// The trailing Context action beside the document title, expressed in the
    /// leaf's viewport. Sits inside the title's block-selection chip so Narrow
    /// stays on the sheet and the icon cannot drift past the highlight.
    pub fn title_action_rect(&self) -> Option<Rect> {
        let Self::Document(d) = self;
        let view = &d.editor.view;
        let row = d.editor.body.rows.first()?;
        if !d.editor.body.is_title_row(0) {
            return None;
        }
        let w = view.view_mode.effective_w(view.viewport.0);
        let chip = koru_editor::rowlayout::row_selection_rect(row, w, row.y - view.scroll);
        let slot = koru_editor::rowlayout::title_action_slot(chip);
        let slot = Rect::new(view.content_x + slot.x, slot.y, slot.w, slot.h);
        // **Not while the find panel is over it.** Both occupy the top right of
        // the content while the title row is visible, so an open find bar could
        // otherwise paint across the page-furniture icon. The container
        // hit-tests this rect before handing the press to the leaf, so the
        // drawing, cursor, tooltip and press must all share this gate.
        //
        // Gated here rather than at the four call sites because
        // `title_action_rect` is the one source all of them share — the
        // drawing, the cursor, the tooltip and the press.
        if self
            .search_panel()
            .is_some_and(|panel| panel.intersects(slot))
        {
            return None;
        }
        Some(slot)
    }

    /// The Koru AI action immediately left of the context action. Both are
    /// anchored to row 0 and therefore scroll with the title instead of being
    /// clamped into the viewport beside unrelated paragraphs.
    pub fn title_ai_action_rect(&self) -> Option<Rect> {
        // No ✦ button in a build that hides Koru AI (koru-features): no rect
        // means no draw, no hit, no tooltip and no accessibility node.
        if !koru_features::ai() {
            return None;
        }
        let context = self.title_action_rect()?;
        Some(Rect::new(
            context.x - context.w - 4.0,
            context.y,
            context.w,
            context.h,
        ))
    }

    /// The find panel's rect, when this leaf has one open.
    pub fn search_panel(&self) -> Option<Rect> {
        let Self::Document(d) = self;
        d.search_panel()
    }

    /// Put the caret in the title row. A draft opens with it there, so that the
    /// first thing typed is the document's name.
    ///
    /// **Only for a leaf that has not been typed into.** It sets the focused
    /// *row* and nothing else, which is fine for a body whose carets are all at
    /// 0 anyway and wrong for one being carried across a swap — see
    /// [`caret_spot`](Self::caret_spot), and the `rstFi` bug it is named after.
    pub fn focus_title(&mut self) {
        let Self::Document(d) = self;
        d.editor.body.focus = 0;
    }

    /// Whether images can be stored right now.
    ///
    /// Pushed down from the container each tick rather than asked for, because a
    /// leaf holds no workspace and no store. It is a *condition*, unlike
    /// `ViewState::images_enabled` beside it, which is a capability of the view
    /// and never changes while it is open.
    pub fn set_images_offline(&mut self, offline: bool) {
        let Self::Document(d) = self;
        d.editor.view_mut().images_offline = offline;
    }

    /// Make this a page that can be read and not written.
    ///
    /// **Set once, when the view is built, and never unset** — see
    /// [`ViewState::read_only`](koru_editor::ViewState::read_only) for what
    /// honouring it costs and where. One caller: the *What's new* tab.
    pub fn set_read_only(&mut self, on: bool) {
        let Self::Document(d) = self;
        d.editor.view_mut().read_only = on;
    }

    /// Drop the title row: this page's name is not in the document.
    ///
    /// A document opened from the library puts its name in row 0, editable in
    /// place. A page built out of Markdown that was never a library document
    /// has no name in it — `document_from_text` deliberately adds no title
    /// line and moves no heading (ADR-363) — so the row would be an empty
    /// title with a *Title* placeholder over prose that starts with the
    /// author's own first heading. The name belongs to whatever chrome knows
    /// it; a standalone `koru-document` window turns the row off for the same
    /// reason.
    ///
    /// **Set once, when the view is built**, like
    /// [`set_read_only`](Self::set_read_only): the rows are rebuilt here, and
    /// a caret carried across afterwards would be a row out.
    pub fn hide_title_row(&mut self, ws: &WorkspaceDoc) {
        let Self::Document(d) = self;
        d.editor.body.title_row = false;
        d.editor.body.rebuild(ws);
        d.editor.body.focus = d.editor.body.first_body_row();
    }

    /// Where the caret is: which row, and how many bytes into it.
    ///
    /// **The offset is the half that gets forgotten.** `focus` alone names a
    /// row, and a row that has just been rebuilt starts its caret at 0 — so
    /// carrying only the row across a swap silently rewinds the caret to the
    /// start of the line. Typing `First` into a document being created came out
    /// as `rstFi` for exactly that reason: two characters reached the draft, the
    /// swap put them in the real document and the caret at 0, and `rst` was
    /// typed in front of them.
    pub fn caret_spot(&self) -> Option<(usize, usize)> {
        let Self::Document(d) = self;
        let body = d.body();
        body.focused().map(|row| (body.focus, row.caret()))
    }

    /// Put the caret back on a row and at an offset in it, after a rebuild.
    ///
    /// Deferred through `place_caret`, not assigned: the leaf has not been laid
    /// out yet at the moment this is called, and a byte index means nothing to a
    /// row with no layout. `apply_pending` puts it in place once there is one —
    /// the same route `Body::sanitize_title_row` takes.
    pub fn place_caret_at(&mut self, row: usize, at: usize) {
        let Self::Document(d) = self;
        let body = &mut d.editor.body;
        if row >= body.rows.len() {
            return;
        }
        body.focus = row;
        if let Some(p) = body.focused_mut() {
            p.place_caret(at);
        }
    }

    // ------------------------------------------------------------ dispatch
    //
    // Each arm names the view's own method explicitly rather than relying on
    // resolution, so adding a third view is a compile error here rather than a
    // silent fall-through to the shared behaviour.

    pub fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        match self {
            Self::Document(d) => BuiltInDocument::key(d, ws, key, text, mods),
        }
    }

    pub fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        match self {
            Self::Document(d) => BuiltInDocument::mouse_down(d, ws, x, y, ctrl),
        }
    }

    /// Where a typed character would land inside this tab.
    ///
    /// `tab` is passed in rather than looked up because a leaf does not know
    /// which tab holds it — the container does, and this is the only fact it
    /// has to lend.
    // Its first caller is Plan 2.4's dictation controller.
    #[allow(dead_code)]
    /// Route a dictation into this leaf. See
    /// [`BuiltInDocument::dictate`](koru_document::BuiltInDocument::dictate),
    /// which owns the decision — a document has three text surfaces of its own
    /// and picks between them the way its key path does.
    ///
    /// An inherent method for the reason every dispatch in this file is one:
    /// [`View::dictate`] and `Editing`'s `*_dictation` methods are two
    /// vocabularies, deliberately spelled differently so that neither can be
    /// reached by accident through the wrong trait import.
    pub fn dictate(
        &mut self,
        ws: &mut WorkspaceDoc,
        action: &koru_dictation::DictationAction,
    ) -> bool {
        match self {
            Self::Document(d) => d.dictate(ws, action),
        }
    }

    pub fn editable_target(
        &self,
        tab: koru_workspace_host::routing::TabId,
    ) -> koru_workspace_host::routing::EditableTarget {
        match self {
            Self::Document(d) => BuiltInDocument::editable_target(d, tab),
        }
    }

    /// Where a dictation into this leaf is happening, **in the leaf's own
    /// space**. The container adds its content origin; see
    /// `MainView::dictation_anchor`.
    pub fn dictation_anchor(&self) -> Option<Rect> {
        match self {
            Self::Document(d) => d.dictation_anchor(),
        }
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        match self {
            Self::Document(d) => BuiltInDocument::mouse_move(d, x, y),
        }
    }

    // The rest are spelled out anyway, because the ambiguity is what this block
    // exists to remove.

    pub fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        match self {
            // A document owns surfaces above the shared editor, including local
            // search. Its View override must see the release so a drag begun in
            // the query field cannot remain armed after the mouse button is up.
            Self::Document(d) => View::mouse_up(d, ws),
        }
    }

    pub fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        match self {
            // The document claims the bubble and its menus; the shared path would
            // word-select the line under a rapid second click on Bold.
            Self::Document(d) => BuiltInDocument::mouse_double_click(d, ws, x, y),
        }
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        Editing::scroll_by(self, delta)
    }

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        Editing::set_viewport(self, w, h)
    }

    pub fn adopt(&mut self, ws: &WorkspaceDoc) {
        Editing::adopt(self, ws)
    }

    /// Settle a leaf whose rows changed underneath it, **without draining the
    /// tracker** — the container has already drained every tab through
    /// `TabView::adopt_cheap`, so only the settling half is owed here.
    ///
    /// Deliberately not `body_mut().catch_up(ws)`, which re-reads the rows and
    /// nothing else: that left an open picker hanging from a node a peer had
    /// deleted, while the same document in its own window closed it. Both hosts
    /// go through [`Editing::catch_up`] now — Q-B4, `ADR-343-overlaycatchup`.
    pub fn catch_up(&mut self, ws: &WorkspaceDoc) {
        Editing::catch_up(self, ws)
    }

    /// Whether the body's structure changed since the last call — a row was
    /// added, deleted or moved. The container drains this to refresh the
    /// explore column, which is a projection of the library.
    pub fn take_structure_dirty(&mut self) -> bool {
        Editing::body_mut(self).take_structure_dirty()
    }

    /// The open request *with* its disposition, for the container.
    ///
    /// An inherent method rather than a second trait one, and deliberately named
    /// differently from `View::take_wants_open`: `View` is what the *shell* asks
    /// of a window, the shell cannot see `koru-editor`, and so it cannot name
    /// an `OpenWhere`. Only one of the two ever drains the field —
    /// `MainView::take_wants_open` returns `None`, so the shell never reaches a
    /// container's tabs through the trait.
    pub fn take_wants_open_where(&mut self) -> Option<(NodeId, OpenWhere)> {
        match self {
            Self::Document(d) => d.take_wants_open_where(),
        }
    }

    pub fn set_entity_context(
        &mut self,
        current: koru_core::WorkspaceId,
        workspaces: Vec<koru_editor::components::entity_picker::WorkspaceChoice>,
    ) -> bool {
        Editing::set_entity_context(self, current, workspaces)
    }

    pub fn take_entity_search_request(
        &mut self,
    ) -> Option<koru_editor::components::entity_picker::EntitySearchRequest> {
        Editing::take_entity_search_request(self)
    }

    pub fn take_reminder_request(&mut self) -> Option<koru_editor::reminders::Request> {
        Editing::take_reminder_request(self)
    }

    pub fn take_delegate_request(&mut self) -> Option<koru_editor::DelegateRequest> {
        Editing::take_delegate_request(self)
    }

    pub fn reminder_submitted(
        &mut self,
        ws: &mut WorkspaceDoc,
        identity: koru_editor::reminders::RequestIdentity,
    ) -> bool {
        Editing::reminder_submitted(self, ws, identity)
    }

    pub fn cancel_reminder(
        &mut self,
        ws: &mut WorkspaceDoc,
        identity: koru_editor::reminders::RequestIdentity,
    ) -> bool {
        Editing::cancel_reminder(self, ws, identity)
    }

    pub fn set_reminder_chips(
        &mut self,
        reminders: std::collections::HashMap<NodeId, Vec<koru_editor::reminders::InlineChip>>,
    ) -> bool {
        Editing::set_reminder_chips(self, reminders)
    }

    pub fn take_reminder_management_request(&mut self) -> Option<(NodeId, String)> {
        Editing::take_reminder_management_request(self)
    }

    /// The node the caret is in — which item a keyboard command is about.
    ///
    /// `Body::focus` is the row the caret is drawn in, so this is the same
    /// answer the focus ring gives and the same one the document's
    /// accessibility projection publishes for the focused paragraph. `None`
    /// when the editor does not hold the keyboard at all, which is what stops a
    /// chord acting on a row nobody is standing in.
    pub fn caret_node(&self) -> Option<NodeId> {
        let Self::Document(document) = self;
        let editor = &document.editor;
        if !editor.view.active {
            return None;
        }
        editor.body.rows.get(editor.body.focus).map(|row| row.id)
    }

    pub fn take_reminder_context_request(
        &mut self,
    ) -> Option<koru_editor::reminders::ContextRequest> {
        Editing::take_reminder_context_request(self)
    }

    pub fn reminder_anchor(
        &mut self,
        node: NodeId,
        reminder_id: Option<&str>,
    ) -> Option<koru_draw::geometry::Rect> {
        Editing::reminder_anchor(self, node, reminder_id)
    }

    pub fn set_entity_search_results(
        &mut self,
        hits: Vec<koru_editor::components::entity_picker::DocumentChoice>,
        unavailable: bool,
    ) -> bool {
        Editing::set_entity_search_results(self, hits, unavailable)
    }

    pub fn deep_link_documents(&mut self) -> Vec<koru_core::DocumentId> {
        Editing::deep_link_documents(self)
    }

    /// Every delegated run this leaf is showing a pill for.
    pub fn run_pill_documents(&mut self) -> Vec<koru_core::DocumentId> {
        Editing::run_pill_documents(self)
    }

    pub fn take_heading_request(&mut self) -> Option<koru_core::DocumentId> {
        Editing::take_heading_request(self)
    }

    pub fn set_headings(
        &mut self,
        document: koru_core::DocumentId,
        headings: Vec<koru_editor::components::entity_picker::HeadingChoice>,
        projected: bool,
    ) -> bool {
        Editing::set_headings(self, document, headings, projected)
    }

    pub fn set_entity_current_document(
        &mut self,
        current: Option<koru_editor::components::entity_picker::DocumentChoice>,
    ) -> bool {
        Editing::set_entity_current_document(self, current)
    }

    pub fn set_entity_titles(
        &mut self,
        titles: &std::collections::HashMap<String, String>,
        trashed: &std::collections::HashSet<String>,
        live: &std::collections::HashSet<String>,
    ) -> bool {
        Editing::set_entity_titles(self, titles, trashed, live)
    }

    pub fn set_schedule_pills(
        &mut self,
        schedules: &std::collections::HashMap<String, (String, bool)>,
    ) -> bool {
        Editing::set_schedule_pills(self, schedules)
    }

    pub fn take_wants_new_page(&mut self) -> Option<String> {
        Editing::take_wants_new_page(self)
    }

    pub fn entity_wants_recents(&self) -> bool {
        Editing::entity_wants_recents(self)
    }

    pub fn set_entity_recents(
        &mut self,
        hits: Vec<koru_editor::components::entity_picker::DocumentChoice>,
    ) -> bool {
        Editing::set_entity_recents(self, hits)
    }

    pub fn take_wants_move_selection(
        &mut self,
    ) -> Option<(
        koru_editor::view_state::MoveSelectionDestination,
        koru_editor::view_state::MoveSelectionPayload,
    )> {
        Editing::take_wants_move_selection(self)
    }

    pub fn complete_selection_move(
        &mut self,
        ws: &mut WorkspaceDoc,
        title: String,
        target: koru_editor::trigger::DocumentTarget,
    ) -> bool {
        Editing::complete_selection_move(self, ws, title, target)
    }

    pub fn cancel_selection_move(&mut self) -> bool {
        Editing::cancel_selection_move(self)
    }

    pub fn complete_new_page_link(
        &mut self,
        ws: &mut WorkspaceDoc,
        title: String,
        target: koru_editor::trigger::DocumentTarget,
    ) -> bool {
        Editing::complete_new_page_link(self, ws, title, target)
    }

    pub fn take_wants_entity_open(
        &mut self,
    ) -> Option<(koru_editor::trigger::DocumentTarget, OpenWhere)> {
        Editing::take_wants_entity_open(self)
    }

    /// The delegated run a click or an Enter on a run pill asked to open.
    pub fn take_wants_run_open(&mut self) -> Option<(koru_core::DocumentId, OpenWhere)> {
        Editing::take_wants_run_open(self)
    }

    /// The delegated run a **right-click** on a run pill asked for a menu
    /// about. What the run produced is the container's to know.
    pub fn take_wants_run_menu(&mut self) -> Option<koru_core::DocumentId> {
        Editing::take_wants_run_menu(self)
    }

    pub fn take_wants_schedule_open(&mut self) -> Option<(String, OpenWhere)> {
        Editing::take_wants_schedule_open(self)
    }

    pub fn take_wants_schedule_menu(&mut self) -> Option<String> {
        Editing::take_wants_schedule_menu(self)
    }

    pub fn schedule_pill_rect(&self, schedule: &str) -> Option<Rect> {
        Editing::schedule_pill_rect(self, schedule)
    }

    pub fn take_wants_toast(&mut self) -> Option<String> {
        Editing::take_wants_toast(self)
    }

    pub fn take_wants_stay_focused(&mut self) -> bool {
        Editing::take_wants_stay_focused(self)
    }

    /// Focus a search match and keep surrounding paragraph context visible.
    pub fn reveal_centered(&mut self, id: NodeId) -> bool {
        Editing::reveal_centered(self, id)
    }

    /// Focus a search match, put the caret before it, and briefly flash the
    /// authored range without creating a collaborative highlight mark.
    pub fn reveal_search_match(&mut self, id: NodeId, match_text: &str) -> bool {
        Editing::reveal_search_match(self, id, match_text)
    }
}

/// The rows, whichever view is showing them. Nothing here shares a name with
/// `View`, so none of it can be reached by mistake.
impl Editing for Leaf {
    fn parts(&mut self) -> (&mut Body, &mut ViewState) {
        match self {
            Self::Document(d) => d.parts(),
        }
    }
    fn body(&self) -> &Body {
        match self {
            Self::Document(d) => d.body(),
        }
    }
    fn body_mut(&mut self) -> &mut Body {
        match self {
            Self::Document(d) => d.body_mut(),
        }
    }
    fn view(&self) -> &ViewState {
        match self {
            Self::Document(d) => d.view(),
        }
    }
    fn view_mut(&mut self) -> &mut ViewState {
        match self {
            Self::Document(d) => d.view_mut(),
        }
    }
}

impl View for Leaf {
    fn dictate(&mut self, ws: &mut WorkspaceDoc, action: &koru_dictation::DictationAction) -> bool {
        Leaf::dictate(self, ws, action)
    }

    /// The document was **replaced**, not edited. Every `ChangeTracker` under
    /// this view is a subscription on the old one and is dead, so this rebuilds
    /// rather than patching — `adopt` cannot help here and never will.
    fn reseat(&mut self, ws: &WorkspaceDoc) {
        *self = match self {
            Self::Document(_) => Self::Document(BuiltInDocument::first(ws)),
        };
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        Leaf::key(self, ws, key, text, mods)
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        Leaf::mouse_down(self, ws, x, y, ctrl)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        Leaf::mouse_move(self, x, y)
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        Leaf::mouse_up(self, ws)
    }

    fn take_wants_task_view(&mut self) -> Option<NodeId> {
        match self {
            Self::Document(d) => View::take_wants_task_view(d),
        }
    }

    fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        match self {
            Self::Document(d) => View::context_menu(d, ws, x, y),
        }
    }

    fn cursor_icon(&self) -> CursorIcon {
        match self {
            Self::Document(d) => View::cursor_icon(d),
        }
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        Leaf::mouse_double_click(self, ws, x, y)
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        Leaf::scroll_by(self, delta)
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        Leaf::set_viewport(self, w, h)
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        match self {
            Self::Document(d) => View::set_mods(d, mods),
        }
    }

    fn adopt(&mut self, ws: &WorkspaceDoc) {
        Leaf::adopt(self, ws)
    }

    fn animating(&self) -> bool {
        match self {
            Self::Document(d) => View::animating(d),
        }
    }

    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        match self {
            Self::Document(d) => View::maintenance_deadline(d),
        }
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: std::time::Instant) -> bool {
        match self {
            Self::Document(d) => View::maintain(d, ws, now),
        }
    }

    fn invalidate(&mut self) {
        match self {
            Self::Document(d) => View::invalidate(d),
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &koru_draw::ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        match self {
            Self::Document(d) => View::draw(d, scene, workspace, images, root, caret_on, pointer),
        }
    }

    fn document_root(&self) -> Option<NodeId> {
        match self {
            Self::Document(d) => View::document_root(d),
        }
    }

    fn take_wants_open(&mut self) -> Option<NodeId> {
        match self {
            Self::Document(d) => View::take_wants_open(d),
        }
    }

    fn take_wants_sharing_toggle(&mut self) -> bool {
        match self {
            Self::Document(d) => View::take_wants_sharing_toggle(d),
        }
    }

    fn take_wants_image(&mut self) -> Option<NodeId> {
        match self {
            Self::Document(d) => View::take_wants_image(d),
        }
    }

    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        match self {
            Self::Document(d) => View::take_wants_attachment(d),
        }
    }

    fn take_wants_dictation(&mut self) -> bool {
        match self {
            Self::Document(d) => View::take_wants_dictation(d),
        }
    }

    fn insert_attachment(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        files: &[(koru_core::DocumentId, String)],
    ) {
        match self {
            Self::Document(d) => View::insert_attachment(d, ws, node, files),
        }
    }

    fn attachment_cancelled(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        match self {
            Self::Document(d) => View::attachment_cancelled(d, ws, node),
        }
    }

    fn take_wants_file_action(
        &mut self,
    ) -> Option<(koru_core::DocumentId, koru_workspace_host::FileAction)> {
        match self {
            Self::Document(d) => View::take_wants_file_action(d),
        }
    }

    fn take_wants_save_diagram_as(
        &mut self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::SaveDiagram> {
        match self {
            Self::Document(d) => View::take_wants_save_diagram_as(d, ws),
        }
    }

    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        match self {
            Self::Document(d) => View::take_wants_create_tag(d),
        }
    }

    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        match self {
            Self::Document(d) => View::take_wants_workspace_word(d),
        }
    }

    fn set_tag_table(&mut self, tags: &[koru_core::system::TagEntry]) -> bool {
        match self {
            Self::Document(d) => View::set_tag_table(d, tags),
        }
    }

    fn set_list_table(&mut self, lists: &[koru_core::system::ListEntry]) -> bool {
        match self {
            Self::Document(d) => View::set_list_table(d, lists),
        }
    }

    fn set_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        match self {
            Self::Document(d) => View::set_workspace(d, workspace),
        }
    }

    fn take_wants_copy_image(&mut self) -> Option<String> {
        match self {
            Self::Document(d) => View::take_wants_copy_image(d),
        }
    }

    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        match self {
            Self::Document(d) => View::take_wants_copy_html(d),
        }
    }

    fn apply_pending_image_cut(&mut self, ws: &mut WorkspaceDoc) -> bool {
        match self {
            Self::Document(d) => View::apply_pending_image_cut(d, ws),
        }
    }

    fn take_wants_paste_image(&mut self) -> Option<koru_workspace_host::PasteTarget> {
        match self {
            Self::Document(d) => View::take_wants_paste_image(d),
        }
    }

    fn take_wants_paste_rich(&mut self) -> Option<(koru_workspace_host::PasteTarget, String)> {
        match self {
            Self::Document(d) => View::take_wants_paste_rich(d),
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
            Self::Document(d) => View::insert_image(d, ws, node, hash, mime, width, height),
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
            Self::Document(d) => View::append_image(d, ws, hash, mime, width, height),
        }
    }

    fn paste_images(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        images: &[koru_workspace_host::PastedImage],
    ) {
        match self {
            Self::Document(d) => View::paste_images(d, ws, target, images),
        }
    }

    fn paste_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        match self {
            Self::Document(d) => View::paste_text(d, ws, text),
        }
    }

    fn paste_rich(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        blocks: &[koru_workspace_host::RichBlock],
    ) {
        match self {
            Self::Document(d) => View::paste_rich(d, ws, target, blocks),
        }
    }

    fn restore(&mut self, ws: &Workspace, settings: &Settings) {
        match self {
            Self::Document(d) => View::restore(d, ws, settings),
        }
    }

    fn capture(&self, settings: &mut Settings) {
        match self {
            Self::Document(d) => View::capture(d, settings),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::WorkspaceId;

    fn ch(value: &str) -> Key {
        Key::Character(value.into())
    }

    /// The title action follows the selection chip, not the page edge. Measuring
    /// `effective_w - 28 - 8` put it in the right-hand margin that only exists
    /// to keep Page mode centred.
    #[test]
    fn the_title_action_sits_inside_the_title_chip() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = koru_workspace::document(&mut ws, "Bugs").unwrap();
        let mut leaf = Leaf::Document(BuiltInDocument::open(&ws, root));
        leaf.set_viewport(800.0, 600.0);

        let slot = leaf.title_action_rect().expect("a titled document");
        let Leaf::Document(document) = &leaf;
        let view = &document.editor.view;
        let row = &document.editor.body.rows[0];
        let w = view.view_mode.effective_w(view.viewport.0);
        let chip = koru_editor::rowlayout::row_selection_rect(row, w, row.y - view.scroll);
        let chip_right = view.content_x + chip.x + chip.w;
        assert!(
            slot.x + slot.w <= chip_right - koru_editor::rowlayout::TITLE_ACTION_INSET + 0.01,
            "inside the chip, inset from its right edge"
        );
        let page_edge_x = view.content_x + w - 28.0 - 8.0;
        assert!(
            slot.x < page_edge_x,
            "not the old page-edge placement: {slot:?} vs {page_edge_x}"
        );
        let ai = leaf.title_ai_action_rect().expect("AI beside Context");
        assert_eq!(ai.x + ai.w + 4.0, slot.x);
    }

    #[test]
    fn title_actions_scroll_with_row_zero_instead_of_clamping_to_the_viewport() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = koru_workspace::document(&mut ws, "Bugs").unwrap();
        let mut leaf = Leaf::Document(BuiltInDocument::open(&ws, root));
        leaf.set_viewport(800.0, 600.0);
        let before = leaf.title_action_rect().unwrap();
        let Leaf::Document(document) = &mut leaf;
        document.editor.view.scroll = 200.0;
        let after = leaf.title_action_rect().unwrap();
        assert_eq!(after.y, before.y - 200.0);
        assert!(after.y < 0.0, "the action left with the title row");
    }

    /// **An open find bar takes the title's page-furniture button away.**
    ///
    /// Both park at the top right of the content — the panel by design, the
    /// button because its slot is pinned to the top edge once the title has
    /// scrolled off — so the icon was painted across the find bar's `Aa`. The
    /// drawing was the visible half; the click was the worse one, because the
    /// container hit-tests this rect *before* it hands the press to the leaf.
    #[test]
    fn an_open_find_bar_hides_the_title_action_it_would_cover() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = koru_workspace::document(&mut ws, "Bugs").unwrap();
        let mut leaf = Leaf::Document(BuiltInDocument::open(&ws, root));
        leaf.set_viewport(800.0, 600.0);

        let slot = leaf
            .title_action_rect()
            .expect("the button is there with the bar shut");
        assert!(leaf.search_panel().is_none(), "no panel before Ctrl+F");

        assert!(leaf.key(&mut ws, &ch("f"), Some("f"), koru_keys::PRIMARY));
        let panel = leaf.search_panel().expect("Ctrl+F opens the find bar");
        assert!(
            panel.intersects(slot),
            "this test is only meaningful while the two overlap: {panel:?} vs {slot:?}"
        );
        assert!(
            leaf.title_action_rect().is_none(),
            "the button must not be drawn, hovered or clicked under the panel"
        );

        // ...and it comes back, rather than being lost for the session.
        assert!(leaf.key(
            &mut ws,
            &Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            ModifiersState::empty()
        ));
        assert!(leaf.search_panel().is_none(), "Esc shuts the bar");
        assert_eq!(
            leaf.title_action_rect().map(|r| (r.x, r.y)),
            Some((slot.x, slot.y)),
            "the button returns where it was"
        );
    }

    /// The leaf is the container/document dispatch boundary. A release sent
    /// through it must reach local search, not just the shared row editor, or
    /// subsequent pointer movement continues selecting the query with no mouse
    /// button held.
    #[test]
    fn mouse_up_ends_a_local_search_query_drag() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap();
        let root = koru_workspace::document(&mut ws, "abc ab").unwrap();
        let mut leaf = Leaf::Document(BuiltInDocument::open(&ws, root));
        leaf.set_viewport(800.0, 600.0);

        assert!(leaf.key(&mut ws, &ch("f"), Some("f"), koru_keys::PRIMARY,));
        assert!(leaf.key(&mut ws, &ch("a"), Some("a"), ModifiersState::empty(),));

        // At an 800 px viewport the 430 px panel starts at x=354. Press near
        // the query's right edge, release through Leaf, then move back across
        // it. If the release is misrouted, that move selects the whole query.
        assert!(leaf.mouse_down(&mut ws, 520.0, 32.0, false));
        assert!(leaf.mouse_up(&mut ws));
        assert!(leaf.mouse_move(368.0, 32.0));
        assert!(leaf.key(&mut ws, &ch("b"), Some("b"), ModifiersState::empty(),));

        let Leaf::Document(document) = &leaf;
        let ranges: Vec<_> = document
            .editor
            .view
            .search_highlights
            .iter()
            .map(|highlight| highlight.range.clone())
            .collect();
        assert_eq!(ranges, [0..2, 4..6], "the query stayed `ab`, not `b`");
    }
}
