// SPDX-License-Identifier: MPL-2.0

//! What a window needs from a document: a frame with a toolbar in it.

use std::time::Instant;

use koru_core::node::{Attr, AttrValue, BlobHash, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{ListStyle, Mark, NodeId};
use koru_draw::ImageCache;
use koru_editor::draw::{Frame, frame};
use koru_editor::{Editing, OpenWhere, PasteBatch, PasteReplacement};
use koru_workspace_host::settings::DocViewPrefs;
use koru_workspace_host::{
    PasteTarget, PastedImage, RichBlock, RichTextBlock, RichTextKind, Settings, View,
};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use crate::caption::Hit as CaptionHit;

use crate::BuiltInDocument;
use crate::bubble;
use crate::caption;
use crate::draw::draw_chrome;
use crate::upsert_doc_prefs;
use koru_workspace::Workspace;

/// The first empty paragraph teaches dictation without pinning a platform
/// chord at the call site. When voice is unavailable, or Guide hints are
/// hidden, it remains an ordinary writing prompt.
pub(crate) fn body_placeholder(voice_available: bool, hint: Option<String>) -> String {
    if voice_available && let Some(hint) = hint {
        format!("Start typing, or press {hint} to dictate…")
    } else {
        "Start typing…".to_owned()
    }
}

/// One open source-list level while a structured paste is being rebuilt in the
/// workspace tree. A nested `<ul>/<ol>` is its own `ListBlock`, attached to the
/// item that contains it, so a change of marker style at that level remains a
/// semantic list rather than becoming marker characters in paragraph text.
struct RichListLevel {
    container: NodeId,
    style: ListStyle,
    parent_item: Option<NodeId>,
    last_item: Option<NodeId>,
}

/// One top-level insertion cursor for every shell-owned paste format. A
/// replacement begins at the selected anchor's old slot; an ordinary paste
/// begins below its anchor; `None` appends to the document body.
struct PasteCursor {
    parent: Parent,
    index: usize,
    after: Option<NodeId>,
}

impl PasteCursor {
    fn new(ws: &WorkspaceDoc, document: Parent, target: Option<&PasteTarget>) -> Option<Self> {
        match target {
            None => Some(Self {
                parent: document,
                index: ws.children(document).ok()?.len(),
                after: None,
            }),
            Some(target) => {
                let parent = ws.parent(target.anchor).ok()?;
                let index = ws.sibling_index(target.anchor).ok()?;
                Some(Self {
                    parent,
                    index: index + usize::from(target.replacing.is_empty()),
                    after: target.replacing.is_empty().then_some(target.anchor),
                })
            }
        }
    }

    fn insert(&mut self, ws: &mut WorkspaceDoc, node: Node) -> Option<NodeId> {
        let id = match self.after {
            Some(anchor) => ws.insert_after(anchor, node).ok()?,
            None => {
                let id = ws.insert(self.parent, node).ok()?;
                ws.move_node(id, self.parent, self.index).ok()?;
                id
            }
        };
        self.after = Some(id);
        Some(id)
    }
}

fn capture_paste_replacements(
    ws: &WorkspaceDoc,
    target: Option<&PasteTarget>,
) -> Option<Vec<PasteReplacement>> {
    let mut replaced = Vec::new();
    for id in target.into_iter().flat_map(|target| &target.replacing) {
        replaced.push(PasteReplacement {
            cutting: ws.cutting(*id).ok()?,
            parent: ws.parent(*id).ok()?,
            index: ws.sibling_index(*id).ok()?,
        });
    }
    Some(replaced)
}

/// Convert a source byte boundary after the row's CR/LF removal, then apply
/// the HTML marks through the same CRDT API toolbar formatting uses.
fn rich_offset(rich: &RichTextBlock, at: usize) -> usize {
    let at = at.min(rich.text.len());
    let keep_newlines = matches!(&rich.kind, RichTextKind::Code { .. });
    rich.text
        .char_indices()
        .take_while(|(offset, ch)| *offset + ch.len_utf8() <= at)
        .filter(|(_, ch)| keep_newlines || !matches!(ch, '\n' | '\r'))
        .map(|(_, ch)| ch.len_utf8())
        .sum::<usize>()
}

fn apply_rich_marks(ws: &mut WorkspaceDoc, id: NodeId, rich: &RichTextBlock) {
    for run in &rich.runs {
        let range = rich_offset(rich, run.start)..rich_offset(rich, run.end);
        if range.is_empty() {
            continue;
        }
        for mark in Mark::ALL {
            if run.marks.has(mark) {
                let _ = ws.set_mark(id, range.clone(), mark, true);
            }
        }
    }
}

/// Replace source link labels with Koru's native labelled-link pills. Work from
/// right to left so shortening a later label cannot invalidate an earlier byte
/// range. Bare URLs are linkified afterwards by the existing paste path.
fn apply_rich_links(ws: &mut WorkspaceDoc, id: NodeId, rich: &RichTextBlock) {
    let mut links = rich.links.clone();
    links.sort_by_key(|link| link.start);
    for link in links.into_iter().rev() {
        if link.start >= link.end
            || link.end > rich.text.len()
            || !rich.text.is_char_boundary(link.start)
            || !rich.text.is_char_boundary(link.end)
        {
            continue;
        }
        let label: String = rich.text[link.start..link.end]
            .chars()
            .filter(|ch| {
                matches!(&rich.kind, RichTextKind::Code { .. }) || !matches!(ch, '\n' | '\r')
            })
            .collect();
        if label.is_empty() || link.url.is_empty() {
            continue;
        }
        let range = rich_offset(rich, link.start)..rich_offset(rich, link.end);
        let attr = Attr::labelled("link", AttrValue::Url(link.url), label);
        let _ = ws.delete_text(id, range.clone());
        let _ = ws.insert_pill(id, range.start, &attr);
    }
}

impl View for BuiltInDocument {
    /// A standalone document window routes a dictation the same way a tab
    /// does — see [`BuiltInDocument::dictate`]. Spelled once, as an inherent
    /// method, because `Leaf` reaches it directly and a second body would be a
    /// second opinion.
    fn dictate(&mut self, ws: &mut WorkspaceDoc, action: &koru_dictation::DictationAction) -> bool {
        BuiltInDocument::dictate(self, ws, action)
    }

    /// The document was replaced, not edited. Rebuilding is the only correct
    /// answer: `Body`'s `ChangeTracker` is a subscription on the *old* document
    /// and is dead, so patching would leave this view frozen with no error.
    fn reseat(&mut self, ws: &WorkspaceDoc) {
        // The node this window was showing belongs to the document that has
        // just gone, so there is nothing to reopen it at: this rebuilds on
        // whatever the new document offers, exactly as a cold start does.
        *self = BuiltInDocument::first(ws);
    }

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        BuiltInDocument::key(self, ws, key, text, mods)
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        BuiltInDocument::mouse_down(self, ws, x, y, ctrl)
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        BuiltInDocument::mouse_move(self, x, y)
    }

    /// **A popped-out document shares the container's workspace**, so it is
    /// owed the same answer about the store — and for a while it was the one
    /// window that never got it, because it draws its own caption instead of
    /// `MainView`'s.
    ///
    /// Compared on what is drawn, not on the whole value: `since` is an
    /// `Instant` that never stops moving, so comparing the struct would report
    /// a change every tick and redraw the window forever.
    fn set_store_trouble(&mut self, trouble: Option<koru_workspace::StoreTrouble>) -> bool {
        let shown = |t: &Option<koru_workspace::StoreTrouble>| {
            t.as_ref().map(|t| (t.fatal, t.unconfirmed.clone()))
        };
        let changed = shown(&self.store_trouble) != shown(&trouble);
        if trouble.is_none() {
            // Nothing to say, so nothing to keep open.
            self.offline_open = false;
        }
        self.store_trouble = trouble;
        changed
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.search.mouse_up();
        if self.search.is_open() {
            return true;
        }
        Editing::mouse_up(self, ws)
    }

    fn take_wants_task_view(&mut self) -> Option<koru_core::NodeId> {
        self.editor.view.wants_task_view.take()
    }

    fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        if self.search.is_open() {
            self.search.close(ws, &mut self.editor, true);
        }
        BuiltInDocument::context_menu(self, ws, x, y)
    }

    /// The bubble and its menus are chrome drawn over the rows, so they answer
    /// before the rows do — otherwise a button two millimetres above the words
    /// it acts on would wear the I-beam of the line underneath it.
    ///
    /// **That is the only thing these arms change.** Koru does not use the hand
    /// cursor (`docs/features/design/DESIGN.md`), so a bubble button and the gap beside it
    /// answer the same shape; what matters is that neither of them is `Text`.
    fn cursor_icon(&self) -> CursorIcon {
        // Ahead of the search bar and the caption alike: a modal covers both.
        if let Some(cursor) = self.confirm_cursor() {
            return cursor;
        }
        if let Some(cursor) = self.search.cursor_icon(&self.editor) {
            return cursor;
        }
        let (x, y) = self.editor.view.pointer;
        // Its own caption, when it *is* the window: the four buttons are things
        // you press, the bare row is what you drag the window by.
        if self.window_chrome().is_some() {
            match caption::hit(x, y, self.editor.view.viewport.0) {
                Some(CaptionHit::Caption) => return CursorIcon::Default,
                Some(_) => return koru_ui::cursor::PRESS,
                None => {}
            }
        }
        if let Some(open) = self.menu {
            if let Some(panel) = self.context_toolbar_panel().filter(|p| p.contains(x, y)) {
                return match bubble::hit(panel, x, y) {
                    Some(_) => koru_ui::cursor::PRESS,
                    None => CursorIcon::Default,
                };
            }
            let rows = self.menu_shape();
            let vp = self.editor.view.viewport;
            // Menu owns the cursor everywhere while open — not only over the
            // panel — matching mouse_move, which no longer falls through.
            return match open.menu().item_at(&rows, vp, x, y) {
                Some(_) => koru_ui::cursor::PRESS,
                None => CursorIcon::Default,
            };
        }
        if let Some(panel) = self.bubble_panel().filter(|p| p.contains(x, y)) {
            return match bubble::hit(panel, x, y) {
                Some(_) => koru_ui::cursor::PRESS,
                // The gaps between buttons are still the bubble's.
                None => CursorIcon::Default,
            };
        }
        self.editor.view.cursor_icon()
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        BuiltInDocument::mouse_double_click(self, ws, x, y)
    }

    fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
    }

    fn take_window_request(&mut self) -> Option<koru_window::WindowRequest> {
        self.window_request.take()
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        Editing::scroll_by(self, delta)
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        Editing::set_viewport(self, w, h)
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.editor.view.mods = mods;
    }

    /// A smooth scroll in flight — or a `+` tooltip counting down to its own
    /// appearance, which needs a frame when the wait is over and would
    /// otherwise get one only if something else happened to ask. The emoji
    /// picker's long-press timer is the same kind of deadline.
    fn animating(&self) -> bool {
        // The caption's window-menu button is still turning, or the menu just moved and
        // the glyph has not been told yet — `draw` is what tells it.
        if self.menu_morph.animating() || self.menu_morph.is_on() != self.window_menu_open() {
            return true;
        }
        let now = Instant::now();
        Editing::is_scroll_animating(self)
            || self.editor.tooltip_animating()
            || self.editor.table_controls_animating(now)
            || self.editor.copy_affordance_animating(now)
            || self.editor.task_warning_animating(now)
            || self.bubble_tooltip_animating()
            || self.editor.overlay_press_animating()
            || self.search.animating(now)
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        [
            self.editor.search_flash_deadline(),
            self.editor.table_controls_maintenance_deadline(),
            self.editor.copy_affordance_maintenance_deadline(),
            self.editor.task_warning_maintenance_deadline(),
            self.search.maintenance_deadline(),
            self.spelling.maintenance_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: Instant) -> bool {
        let search = self.editor.maintain_search_flash(now);
        let table = self.editor.maintain_table_controls(now);
        let copy = self.editor.maintain_copy_affordance(now);
        let warning = self.editor.maintain_task_warning(now);
        let local = self.search.maintain(now);
        let spelling = self.spelling.maintain(&mut self.editor, ws, now);
        // The layouts the last reflow asked for go out; the drawings that
        // came back land, and the next frame takes its heights from them.
        let diagrams = self.diagrams.maintain(&mut self.editor);
        // Suggestions that just landed belong in the open context menu's
        // placeholder, not behind a reopen.
        let filled = self.fill_context_suggestions();
        search || table || copy || warning || local || spelling || diagrams || filled
    }

    fn adopt(&mut self, ws: &WorkspaceDoc) {
        // A rename — this window's or a peer's — arrives here, and the title bar
        // is the one place showing the name now that it is not a row.
        self.refresh_title(ws);
        Editing::adopt(self, ws);
        self.search.set_standalone_title(
            self.window_chrome
                .then(|| self.editor.body.root.map(|root| (root, self.title.clone())))
                .flatten(),
        );
        self.search.refresh(&mut self.editor);
        // A peer's edit arrives here with no local frame promised, so the
        // spelling controller notices it now rather than at the next draw.
        self.spelling.observe(&mut self.editor, ws);
    }

    fn invalidate(&mut self) {
        self.editor.view.engine.clear_pill_cache();
        self.editor.body.invalidate();
        self.reflow();
    }

    /// Resolve the view mode and page colour for this document: a per-document
    /// override if there is one, otherwise the global default. The fields are
    /// set directly rather than through `set_view_mode` — `restore` runs before
    /// any window exists, so there is no viewport to reflow against; the first
    /// `set_viewport` lays the rows out with the mode already in place.
    fn restore(&mut self, _ws: &koru_workspace::Workspace, settings: &Settings) {
        koru_editor::emoji::restore_usage(&settings.emoji_recents);
        koru_editor::emoji::set_preferred_skin_tone(koru_editor::emoji::parse_skin_tone(
            &settings.emoji_skin_tone,
        ));
        let (mode, canvas_bg) = settings.doc_layout(self.editor.body.root);
        let vw = self.editor.view.viewport.0;
        self.editor.view.view_mode = mode;
        self.editor.view.content_x = mode.content_x(vw);
        self.editor.view.canvas_bg = canvas_bg;
    }

    /// `restore` above ran against the registry's default width; now that the
    /// authority has said what the person's is, resolve this document again by
    /// the same rule. Through `set_view_mode` this time: there is a viewport,
    /// and the rows were laid out at the old width.
    fn adopt_startup_preferences(&mut self, settings: &Settings) {
        self.adopt_preferences(settings);
        let (mode, canvas_bg) = settings.doc_layout(self.editor.body.root);
        if self.editor.view.view_mode != mode {
            Editing::set_view_mode(self, mode);
        }
        self.editor.view.canvas_bg = canvas_bg;
    }

    /// Write this document's mode and page colour back as a per-document
    /// override — dropping any field that matches the global default, so the
    /// file only records what is genuinely personal to this document.
    fn capture(&self, settings: &mut Settings) {
        settings.emoji_recents = koru_editor::emoji::recent_usage();
        settings.emoji_skin_tone =
            koru_editor::emoji::skin_tone_token(koru_editor::emoji::preferred_skin_tone())
                .to_owned();
        let Some(root) = self.editor.body.root else {
            return;
        };
        let mode = (self.editor.view.view_mode != settings.view_mode)
            .then_some(self.editor.view.view_mode);
        let canvas_bg = (self.editor.view.canvas_bg != settings.canvas_bg)
            .then_some(self.editor.view.canvas_bg)
            .flatten();
        let prefs = DocViewPrefs { mode, canvas_bg };
        upsert_doc_prefs(settings, root, prefs);
    }

    fn document_root(&self) -> Option<NodeId> {
        self.editor.body.root
    }

    /// The node alone: a standalone window has no tabs, so "a new tab" and
    /// "this tab" both mean the window the shell is about to make. The container
    /// reads the disposition through `take_wants_open_where`.
    fn take_wants_open(&mut self) -> Option<NodeId> {
        self.editor.view.wants_open.take().map(|(node, _)| node)
    }

    fn take_wants_sharing_toggle(&mut self) -> bool {
        std::mem::take(&mut self.editor.view.wants_sharing_toggle)
    }

    fn take_wants_image(&mut self) -> Option<NodeId> {
        self.editor.view.wants_image.take()
    }

    fn take_wants_dictation(&mut self) -> bool {
        std::mem::take(&mut self.editor.view.wants_dictation)
    }

    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        let want = self.editor.view.wants_attachment.take();
        if want.is_some() {
            koru_workspace_host::attach_debug!("document take_wants_attachment node={want:?}");
        }
        want
    }

    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        self.editor.view.wants_create_tag.take()
    }

    fn take_wants_join_new_list(&mut self) -> Option<(Vec<NodeId>, String, Option<String>)> {
        self.editor.view.wants_join_new_list.take()
    }

    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        self.editor.view.wants_workspace_word.take()
    }

    fn set_tag_table(&mut self, tags: &[koru_core::system::TagEntry]) -> bool {
        // Compared against this view's copy, so the shell's every-turn push
        // costs a small Vec compare and nothing else.
        if self.tag_table == tags {
            return false;
        }
        self.tag_table = tags.to_vec();
        self.editor.view.tags.replace(tags.iter().map(|tag| {
            (
                tag.id,
                koru_editor::chips::TagFace {
                    name: tag.name.clone(),
                    color: tag.color.clone(),
                },
            )
        }));
        // A renamed tag is the same `ChipSpec` with a new face: nothing in
        // the rows changed, so repaint is all a table change needs.
        true
    }

    /// The list table, resolved down to what a chip needs: `ListId` → accent.
    ///
    /// **The tag table's twin, and pushed on the same turn.** A list has a name
    /// too, and the rail draws it — but in a document the membership shows as
    /// the due chip's colour rather than as a lozenge of its own, so only the
    /// accent crosses this seam. A list with no colour, and one this table has
    /// never heard of, both resolve to `None`: the chip draws neutral, the same
    /// answer a forgotten tag gets and for the same reason.
    fn set_list_table(&mut self, lists: &[koru_core::system::ListEntry]) -> bool {
        if self.list_table == lists {
            return false;
        }
        self.list_table = lists.to_vec();
        self.editor.view.lists.replace_named(
            lists
                .iter()
                .map(|l| (l.id, l.name.clone(), l.color.clone())),
        );
        // A recoloured list is the same rows with a new face — repaint, no
        // reflow. The accent changes no width; that is why it may be pushed
        // this cheaply.
        true
    }

    fn set_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        // Nothing on screen depends on it — the clipboard reads it at a copy
        // and at a paste — so it never asks for a repaint.
        self.editor.view.workspace = workspace;
        false
    }

    fn insert_attachment(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        files: &[(koru_core::DocumentId, String)],
    ) {
        koru_workspace_host::attach_debug!(
            "document insert_attachment node={node:?} n={} names={:?}",
            files.len(),
            files.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>()
        );
        let Some((first, rest)) = files.split_first() else {
            return;
        };
        let put =
            |ws: &mut WorkspaceDoc, target: NodeId, file: koru_core::DocumentId, label: &str| {
                let attr = koru_core::Attr::labelled(
                    "attachment",
                    koru_core::AttrValue::Ref(koru_core::Ref::in_doc(
                        NodeId::from_uuid(file.as_uuid()),
                        koru_core::DocRef::Document(file),
                    )),
                    label,
                );
                ws.set_text(target, "").is_ok() && ws.insert_pill(target, 0, &attr).is_ok()
            };
        if !put(ws, node, first.0, &first.1) {
            return;
        }
        let mut after = node;
        for (file, label) in rest {
            let Ok(para) = koru_core::Node::paragraph("") else {
                break;
            };
            let Ok(row) = ws.insert_after(after, para) else {
                break;
            };
            if !put(ws, row, *file, label) {
                break;
            }
            after = row;
        }
        self.editor.body.rebuild(ws);
        self.editor.reflow();
    }

    fn attachment_cancelled(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        koru_workspace_host::attach_debug!("document attachment_cancelled node={node:?}");
        let _ = ws.set_text(node, "");
        self.editor.body.rebuild(ws);
        self.editor.reflow();
    }

    fn take_wants_file_action(
        &mut self,
    ) -> Option<(koru_core::DocumentId, koru_workspace_host::FileAction)> {
        self.editor
            .view
            .wants_file_action
            .take()
            .map(|(file, action)| {
                let mapped = match action {
                    koru_editor::FileAction::Open => koru_workspace_host::FileAction::Open,
                    koru_editor::FileAction::Reveal => koru_workspace_host::FileAction::Reveal,
                    koru_editor::FileAction::Save => koru_workspace_host::FileAction::Save,
                    koru_editor::FileAction::Restore => koru_workspace_host::FileAction::Restore,
                };
                koru_workspace_host::attach_debug!(
                    "document take_wants_file_action file={} action={mapped:?}",
                    file.to_storage()
                );
                (file, mapped)
            })
    }

    fn take_wants_save_diagram_as(
        &mut self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::SaveDiagram> {
        let request = BuiltInDocument::take_wants_save_diagram_as(self)?;
        // The menu offered the row on a drawing; a keystroke since may have
        // replaced it with a placeholder, and then there is nothing to save.
        let prepared = self.drawn_diagram(request.node)?;
        Some(koru_workspace_host::SaveDiagram {
            prepared,
            title: self.page_name(ws),
        })
    }

    fn take_wants_copy_image(&mut self) -> Option<String> {
        self.editor.view.wants_copy_image.take()
    }

    fn apply_pending_image_cut(&mut self, ws: &mut WorkspaceDoc) -> bool {
        self.editor.apply_pending_cut_image(ws)
    }

    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        self.editor.view.wants_copy_html.take()
    }

    fn take_wants_paste_image(&mut self) -> Option<PasteTarget> {
        self.editor
            .view
            .wants_paste_image
            .take()
            .map(|(anchor, replacing)| PasteTarget { anchor, replacing })
    }

    fn take_wants_paste_rich(&mut self) -> Option<(PasteTarget, String)> {
        self.editor
            .view
            .wants_paste_rich
            .take()
            .map(|(anchor, replacing, text)| (PasteTarget { anchor, replacing }, text))
    }

    /// Convert the `/image` node into an image. `set_image` swaps the kind and
    /// content in place, and a rebuild picks up the new shape.
    fn insert_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        let _ = ws.set_image(node, BlobHash(hash.into()), mime, width, height);
        self.editor.body.rebuild(ws);
        self.editor.reflow();
    }

    /// A dropped file becomes a new image row at the end of the document body.
    fn append_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        let parent = self
            .editor
            .body
            .root
            .map(koru_core::schema::Parent::Node)
            .unwrap_or(koru_core::schema::Parent::Root);
        if let Ok(img) = Node::image(BlobHash(hash.into()), mime, width, height) {
            let _ = ws.insert(parent, img);
            self.editor.body.rebuild(ws);
            self.editor.reflow();
        }
    }

    /// Pasted images become new blocks at the target the editor recorded, one
    /// each, in the order the clipboard reported them.
    /// The whole paste is one undo group, and one `rebuild` + `reflow` — N
    /// images must not cost N layouts. Each landed node threads in as the
    /// anchor for the next, so the sequence keeps its order and stays flat.
    fn paste_images(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<PasteTarget>,
        images: &[PastedImage],
    ) {
        if images.is_empty() {
            return;
        }
        let document = self
            .editor
            .body
            .root
            .map(Parent::Node)
            .unwrap_or(Parent::Root);
        let Some(mut cursor) = PasteCursor::new(ws, document, target.as_ref()) else {
            return;
        };
        let Some(replaced) = capture_paste_replacements(ws, target.as_ref()) else {
            return;
        };
        // Close any open typing group first so this paste is its own Loro step
        // (guide/contributing/PRINCIPLES.md — use UndoManager, do not invent a second stack).
        ws.checkpoint();
        let grouped = ws.begin_group();
        let mut inserted: Vec<NodeId> = Vec::new();
        for img in images {
            let Ok(node) =
                Node::image(BlobHash(img.hash.clone()), &img.mime, img.width, img.height)
            else {
                continue;
            };
            let Some(id) = cursor.insert(ws, node) else {
                continue;
            };
            inserted.push(id);
            if let Some(name) = &img.name {
                let _ = ws.set_name(id, Some(name));
            }
        }
        if !inserted.is_empty()
            && let Some(target) = &target
        {
            for &id in &target.replacing {
                let _ = ws.delete(id);
            }
        }
        ws.checkpoint();
        if !inserted.is_empty() {
            self.editor.view.last_image_redos.clear();
            self.editor.view.paste_redo_exhausted = false;
            self.editor.view.last_image_pastes.push(PasteBatch {
                inserted: inserted.clone(),
                replaced,
            });
        }
        koru_editor::clipboard::undo_dbg(format!(
            "paste_images inserted={} grouped={grouped} in_group={} can_undo={} can_redo={} active={} paste_stack={}",
            inserted.len(),
            ws.in_group(),
            ws.can_undo(),
            ws.can_redo(),
            self.editor.view.active,
            self.editor.view.last_image_pastes.len()
        ));
        self.editor.body.clear_selection();
        self.editor.body.rebuild(ws);
        if let Some(last) = cursor.after {
            self.editor.body.focus_on(last);
        }
        self.editor.view.active = true;
        self.editor.settle();
        if let Some(last) = cursor.after
            && let Some(i) = self.editor.body.index_of(last)
        {
            self.editor.scroll_row_into_view(i);
            self.editor.scroll_to_caret();
        }
    }

    /// The plain-text fallback for a rich paste that carried no pictures: hand
    /// the carried text to the same plain-text paste path Ctrl+V always used.
    fn paste_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        koru_editor::clipboard::paste_plain_text(&mut self.editor, ws, text);
    }

    /// A mixed text/image paste, interleaved in document order at `target` (or
    /// appended when it is `None`). Adapts [`paste_images`]: each block
    /// threads in after the last, so the whole paste keeps its order, costs one
    /// `rebuild` + `reflow`, and is a single undo step. HTML list metadata is
    /// rebuilt as `ListBlock` containers rather than discarded or painted into
    /// the text. URLs in text blocks become link pills, the way they do in an
    /// ordinary text paste.
    ///
    /// [`paste_images`]: Self::paste_images
    fn paste_rich(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<PasteTarget>,
        blocks: &[RichBlock],
    ) {
        if blocks.is_empty() {
            return;
        }
        let document = self
            .editor
            .body
            .root
            .map(Parent::Node)
            .unwrap_or(Parent::Root);
        let Some(mut cursor) = PasteCursor::new(ws, document, target.as_ref()) else {
            return;
        };
        let Some(replaced) = capture_paste_replacements(ws, target.as_ref()) else {
            return;
        };
        // Close any open typing group first so this paste is its own Loro step.
        ws.checkpoint();
        let grouped = ws.begin_group();
        let mut inserted: Vec<NodeId> = Vec::new();
        let mut last_visible = None;
        let mut invalid_table = None;
        let mut list_levels: Vec<RichListLevel> = Vec::new();
        for block in blocks {
            if let RichBlock::Text(rt) = block
                && let Some(spec) = rt.list
            {
                // Source depth is advisory: malformed/ragged HTML may ask for
                // level two before level one. Clamp to the number of open
                // levels so the tree never skips a parent.
                let mut depth = (spec.depth as usize).min(list_levels.len());
                let mut parent_item = if depth == 0 {
                    None
                } else {
                    list_levels.get(depth - 1).and_then(|level| level.last_item)
                };
                if depth > 0 && parent_item.is_none() {
                    depth = 0;
                    parent_item = None;
                }

                // Returning to a shallower source level closes every deeper
                // container. At the requested level, a style change or a new
                // parent item starts a new ListBlock.
                list_levels.truncate((depth + 1).min(list_levels.len()));
                let reusable = list_levels.get(depth).is_some_and(|level| {
                    level.style == spec.style && level.parent_item == parent_item
                });
                if !reusable {
                    list_levels.truncate(depth);
                    let Ok(list_node) = Node::list_block() else {
                        continue;
                    };
                    let list_id = if let Some(parent_item) = parent_item {
                        ws.insert(Parent::Node(parent_item), list_node).ok()
                    } else {
                        cursor.insert(ws, list_node)
                    };
                    let Some(list_id) = list_id else { continue };
                    let _ = ws.set_list_style(list_id, Some(spec.style));
                    inserted.push(list_id);
                    list_levels.push(RichListLevel {
                        container: list_id,
                        style: spec.style,
                        parent_item,
                        last_item: None,
                    });
                }

                let text = rt.text.replace(['\n', '\r'], "");
                let Ok(item) = Node::paragraph(text) else {
                    continue;
                };
                let container = list_levels[depth].container;
                let Ok(id) = ws.insert(Parent::Node(container), item) else {
                    continue;
                };
                list_levels[depth].last_item = Some(id);
                list_levels.truncate(depth + 1);
                inserted.push(id);
                last_visible = Some(id);
                if let Some(h) = rt.heading {
                    let _ = ws.set_heading(id, Some(h));
                }
                apply_rich_marks(ws, id, rt);
                apply_rich_links(ws, id, rt);
                let _ = koru_editor::clipboard::linkify_urls_in_node(ws, id);
                continue;
            }

            // Any ordinary paragraph or image ends the adjacent source-list
            // run. The outer list's container remains the cursor, so this block
            // lands after the entire list rather than inside its final item.
            list_levels.clear();
            let (node, heading, language) = match block {
                RichBlock::Table(table) => (Node::table(table.source.clone()), None, None),
                RichBlock::Image(img) => (
                    Node::image(
                        BlobHash(img.hash.clone()),
                        img.mime.clone(),
                        img.width,
                        img.height,
                    ),
                    None,
                    None,
                ),
                RichBlock::Text(rt) => {
                    // A document row is one line ([ADR-23]); strip any stray
                    // newline a degenerate RTF may have smuggled through.
                    let node = match &rt.kind {
                        RichTextKind::Paragraph => {
                            Node::paragraph(rt.text.replace(['\n', '\r'], ""))
                        }
                        RichTextKind::Quote => Node::quote(rt.text.replace(['\n', '\r'], "")),
                        RichTextKind::Code { .. } => Node::code(rt.text.clone()),
                    };
                    let language = match &rt.kind {
                        RichTextKind::Code { language } => language.as_deref(),
                        _ => None,
                    };
                    (node, rt.heading, language)
                }
            };
            let Ok(node) = node else {
                continue;
            };
            let id = cursor.insert(ws, node);
            let Some(id) = id else { continue };
            if let RichBlock::Table(table) = block
                && koru_editor::table::parse_source(&table.source).is_err()
            {
                invalid_table = Some(id);
            }
            inserted.push(id);
            last_visible = Some(id);
            if let Some(h) = heading {
                let _ = ws.set_heading(id, Some(h));
            }
            if let Some(language) = language {
                let _ = ws.set_language(id, Some(language));
            }
            if let RichBlock::Text(rt) = block {
                // A code block pastes as plain text: source has no marks, no
                // labelled links and no URL linkification — it is the text.
                if !matches!(&rt.kind, RichTextKind::Code { .. }) {
                    apply_rich_marks(ws, id, rt);
                    apply_rich_links(ws, id, rt);
                    let _ = koru_editor::clipboard::linkify_urls_in_node(ws, id);
                }
            }
            if let RichBlock::Image(img) = block {
                if let Some(name) = &img.name {
                    let _ = ws.set_name(id, Some(name));
                }
            }
        }
        if !inserted.is_empty()
            && let Some(target) = &target
        {
            for &id in &target.replacing {
                let _ = ws.delete(id);
            }
        }
        ws.checkpoint();
        if !inserted.is_empty() {
            self.editor.view.last_image_redos.clear();
            self.editor.view.paste_redo_exhausted = false;
            self.editor.view.last_image_pastes.push(PasteBatch {
                inserted: inserted.clone(),
                replaced,
            });
        }
        koru_editor::clipboard::undo_dbg(format!(
            "paste_rich inserted={} grouped={grouped} in_group={} can_undo={} can_redo={} active={} paste_stack={}",
            inserted.len(),
            ws.in_group(),
            ws.can_undo(),
            ws.can_redo(),
            self.editor.view.active,
            self.editor.view.last_image_pastes.len()
        ));
        self.editor.body.clear_selection();
        self.editor.body.rebuild(ws);
        if let Some(id) = invalid_table {
            self.editor.view.table_source = Some(id);
            if let Ok(node) = ws.read(id)
                && let Err(errors) =
                    koru_editor::table::parse_source(node.content.as_text().unwrap_or_default())
            {
                self.editor.view.notice = errors
                    .first()
                    .map(|error| format!("Table line {}: {}", error.line, error.message));
            }
        }
        if let Some(last) = last_visible {
            self.editor.body.focus_on(last);
            // A rebuilt paragraph starts at byte zero. Queue the insertion
            // point after the rich payload, matching plain multi-row paste;
            // `place_caret` clamps this sentinel to the final grapheme after
            // linkification has replaced any URL text with pill anchors.
            self.editor.place_caret(usize::MAX);
        }
        self.editor.view.active = true;
        // settle reflows and scrolls to the caret — plain multi-row paste does
        // the same. Without it a large foreign paste left the caret at the end
        // while the viewport stayed where the paste started.
        self.editor.settle();
        if let Some(last) = last_visible
            && let Some(i) = self.editor.body.index_of(last)
        {
            self.editor.scroll_row_into_view(i);
            self.editor.scroll_to_caret();
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
        let ws = &workspace.ws;
        // A frame always follows a local edit, so this is where the spelling
        // controller sees the editing view's own changes (`adopt` only runs
        // for the panes that did not make the edit). Free when nothing
        // changed: one tracker drain of an empty set.
        self.spelling.observe(&mut self.editor, ws);
        // Laid out before the bubble is placed. `frame` reflows too — cheap
        // when the rows are already clean — but the bubble is anchored to the
        // *selection's* rects, and measuring those against a stale layout puts
        // it a line away from the words it is about for one frame.
        self.reflow();
        // The bubble's place is decided once, here, and pinned — see
        // `place_bubble`.
        self.place_bubble();
        // The caption's window-menu button follows whether its menu is open, so closing
        // the menu any way at all turns the glyph back without each of those
        // paths having to remember to. The frame this draw was asked for *is*
        // the tick — `animating` is why it was asked.
        let now = Instant::now();
        let open = self.window_menu_open();
        self.menu_morph.set(open, now);
        self.menu_morph.advance(now);
        // Both computed before the closure borrows `self` through `frame`.
        let chrome = self.window_chrome();
        let bubble = self.context_toolbar_panel().or_else(|| self.bubble_panel());
        let tooltip = self.bubble_tooltip_progress();
        let placeholder = body_placeholder(
            koru_dictation::available(),
            koru_keys::show_hint(koru_keys::ShortcutId::Dictate, koru_keys::Section::Guide),
        );
        frame(
            scene,
            root,
            self,
            images,
            Frame {
                caret_on,
                pointer,
                placeholder: &placeholder,
                title_placeholder: "Title",
                // Documents have no footer. `Frame` keeps this generic field
                // for other editor projections, but its height is zero here.
                status: ("", ""),
            },
            |scene, root, body, view| {
                draw_chrome(scene, root, body, view, ws, chrome, bubble, tooltip)
            },
        );

        // Search is viewport furniture: above rows and document chrome, below
        // no other document-owned surface (opening it closes those surfaces).
        self.search.draw(scene, root, &mut self.editor);

        // A modal question paints over everything, scrim included: while it is
        // up, nothing behind it is answerable. It is drawn from here rather
        // than inside `frame` because it is not document chrome.
        if self.confirm_open() {
            self.draw_confirm(scene, root);
            return;
        }

        // An open menu paints last, over the bubble it hangs from and over the
        // rows — the same reason the list flyout used to.
        let Some(open) = self.menu else {
            return;
        };
        let rows = self.menu_rows(ws);
        let vp = self.editor.view.viewport;
        let swatches = crate::page_menu::swatch_colors();
        let engine = &mut self.editor.view.engine;
        // Only the page menu's width rows carry leading art (a little page
        // diagram — Phosphor has no icon that says "how wide the column
        // runs"). Every other menu's icon-less row means "no icon": a
        // spelling suggestion should read as the word it is.
        if matches!(self.menu, Some(crate::OpenMenu::Page(_))) {
            open.menu().draw(
                scene,
                root,
                engine,
                &rows,
                vp,
                &swatches,
                crate::page_menu::draw_width_icon,
            );
        } else {
            open.menu().draw(
                scene,
                root,
                engine,
                &rows,
                vp,
                &swatches,
                |_, _, _, _, _| {},
            );
        }
    }
}

impl BuiltInDocument {
    /// The open request *with* its disposition, for a host that owns tabs.
    ///
    /// The `View` impl above hands the shell the node alone, because a window
    /// with no tabs has nowhere to put "a new tab". This is the same field, and
    /// only one of the two ever drains it: a container's `MainView` returns
    /// `None` from `take_wants_open`, so the shell never reaches this view's.
    pub fn take_wants_open_where(&mut self) -> Option<(NodeId, OpenWhere)> {
        self.editor.view.wants_open.take()
    }
}
