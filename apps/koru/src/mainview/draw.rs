// SPDX-License-Identifier: MPL-2.0

//! What the container puts on screen, and in what order.
//!
//! The order is the thing worth stating, because no single piece can know it:
//!
//! ```text
//! the active view, clipped to the content rect
//!   the explore column      \  over the view, not beside it — a column sliding
//!   the context column      /  out still covers space it has already given back
//!     the caption row          always last: nothing may paint over the tabs
//! ```
//!
//! The **clip is not optional**. `draw_document` renders one row of margin
//! either side of the visible range at an unclamped `y`, and the drag feedback
//! fills a full-width band at the same one — both escape the content rect and
//! would land on the caption.

use std::time::Instant;

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::{ImageCache, icons, scene, theme};
use koru_editor::Editing;
use koru_shell_chrome::{caption, regions::Hit};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill};

use super::panes;
use super::tab::TabView;
use super::{CONTEXT_TOGGLE, MainView, WINDOW_BUTTONS, reminder_bell};

#[cfg(test)]
mod tests;

/// The status chip's detail line, for what this window's workspace knows.
///
/// **A map with no words in it.** The branching and every sentence live in
/// `koru_i18n::store`, because `apps/koru-document` draws the same chip in its
/// own caption and two copies of a three-branch assembly is how two captions
/// start describing one store two ways. What is left here — five fields copied
/// across — has nothing in it to disagree about, and
/// `the_two_captions_say_the_same_thing` in each app's suite says so.
pub(crate) fn store_detail(
    lang: koru_i18n::Lang,
    trouble: &koru_workspace::StoreTrouble,
) -> String {
    koru_i18n::store::popover_detail(
        lang,
        koru_i18n::store::Trouble {
            documents: trouble.unconfirmed.documents,
            document_list: trouble.unconfirmed.document_list,
            not_buffered: trouble.unconfirmed.not_buffered.is_some(),
            fatal: trouble.fatal,
            message: &trouble.message,
        },
    )
}

impl MainView {
    /// One frame. The body of `View::draw`, which is otherwise only a signature.
    pub(super) fn draw_frame(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        // Clone only the already-discovered client. No Store request occurs on
        // the frame thread; both reminder controllers dispatch work later.
        self.reminder_preferences
            .set_client(workspace.store_client());
        self.reminders.set_client(workspace.store_client());
        // The Welcome ticks come off a worker thread too; no request here.
        self.welcome_progress.set_client(workspace.store_client());
        // A Welcome tab made anywhere (a closed last tab, + → Welcome, a
        // restore) learns who it greets the first time it is in front. Each
        // tab compares before it re-measures, so a frame with nothing new
        // costs a few string comparisons.
        if matches!(self.tabs().active_tab().view, TabView::Welcome(_)) {
            self.sync_welcome_lists();
        }
        if crate::taskbar::take_new_document() {
            self.prompt_new_document();
        }
        // A Recent entry chosen while Koru was already running. Drained here
        // rather than by the shell, for the same reason the prompt is: this is
        // the nearest point that has a document to look the id up in.
        if let Some(node) = crate::taskbar::take_open() {
            self.open_document_by_id(&workspace.ws, node);
        }
        // A store-scoped Recent (or pinned) entry: a stable document id, opened
        // through the store so it lands in whichever workspace it lives in.
        if let Some(document) = crate::taskbar::take_open_document() {
            tracing::info!(target: "koru::taskbar", document = %document.to_storage(), "open-document drained");
            self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                target: koru_workspace_host::NavigationTarget::Document {
                    document,
                    node: None,
                },
                match_text: None,
                disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            });
        }
        // The taskbar's *Search* task is drained by the shell's `about_to_wait`
        // (the host's `take_search_activation`), not here — a redraw is not a
        // reliable wake, and the overlay must open the moment the listener
        // fires.
        // The chord the agent actually registered, once it has said so. The
        // taskbar's *Search* task title is rebuilt only when this changes.
        if let Some(chord) = self.agent.take_shortcut() {
            crate::taskbar::set_shortcut(chord);
        }
        // Advanced here rather than on a clock of its own: the shell asked for
        // this frame *because* `animating` said so, so the frame is the tick.
        self.advance_panels(&workspace.ws);
        // The one place every way of moving the caret converges, which is what
        // makes it the right place to notice that it has left a title.
        self.settle_naming(&workspace.ws);
        // Before sizing: a tab that was in the background while a peer's changes
        // arrived owes itself a rebuild, and laying out rows that are about to
        // be replaced would be a wasted pass.
        self.catch_up_active(&workspace.ws);
        self.size_visible_groups();
        // The tree is only built when there is a column to draw it in. Building
        // it recurses through every node in the library — paragraphs included,
        // since a picture inside one still earns a row — and doing that for a
        // column of zero width is the whole cost for none of the benefit. The
        // staleness flag survives, so opening the sidebar builds it then.
        self.refresh_explore(&workspace.ws, self.sidebar_rect().w > 0.0);

        // The zoom indicator. Zoom is a shell-level transform this view never
        // sees, so it notices the `theme::zoom()` global move and flashes the
        // percentage centred, like a TV's volume bar. `tick` also expires it.
        let now = Instant::now();
        let zoom = theme::zoom();
        if zoom != self.last_zoom {
            self.last_zoom = zoom;
            self.zoom_label.show(now);
            // The window menu's own stepper is a zoom source that runs *while*
            // the menu is up, and zoom moves the button it hangs from.
            self.follow_window_menu_button();
        }
        let zoom_alpha = self.zoom_label.tick(now);
        // Dropping an expired toast is what lets `animating` reach false.
        self.tick_toast(now);

        let c = self.content();
        if c.w <= 0.0 || c.h <= 0.0 {
            crate::mainview::nav_debug::nav_debug!(
                "[koru:nav] draw empty content w={} h={}",
                c.w as i32,
                c.h as i32,
            );
            self.draw_caption_band(scene, root);
            self.draw_caption(scene, root, &workspace.ws);
            self.draw_reminder_surfaces(scene, root);
            self.draw_menu(scene, root);
            self.draw_tooltip(scene, root);
            self.draw_new_document(scene, root);
            self.draw_snapshot_prompt(scene, root);
            self.draw_import_dialog(scene, root);
            self.draw_error(scene, root);
            return;
        }

        let clip = vello::kurbo::Rect::new(
            c.x as f64,
            c.y as f64,
            (c.x + c.w) as f64,
            (c.y + c.h) as f64,
        );
        scene.push_clip_layer(Fill::NonZero, root, &clip);
        let inspect_content_clip = koru_draw::inspect::clip(root, clip);
        for (path, rect) in self.panes.leaf_rects(self.content()) {
            let Some(lane) = self.lane_rect_for(&path) else {
                continue;
            };
            let view_rect = panes::PaneTree::view_rect_in(c, rect);
            let leaf_root = root * Affine::translate((view_rect.x as f64, view_rect.y as f64));
            let _inspect_pane = koru_draw::inspect::scope(
                "Content pane",
                "container",
                root,
                scene::rect(view_rect.x, view_rect.y, view_rect.w, view_rect.h),
                |r| {
                    r.property(
                        "layout",
                        "active view in split pane",
                        "MainView::panes; inner arrangement not exposed",
                    );
                },
            );
            let local = (pointer.0 - view_rect.x, pointer.1 - view_rect.y);

            // A top-row lane is in the title bar — outside the content clip —
            // and draws in the second pass, over the caption band.
            if lane.y >= c.y {
                self.draw_group_strip(scene, root, &path, lane, pointer, &workspace.ws);
            }

            let group = self.panes.group_mut(&path).unwrap();
            // A draft draws from its own workspace, which is the whole reason it is
            // a real editor rather than a picture of one.
            let (view, ws) = group.active_mut().workspace(workspace);
            match view {
                TabView::Welcome(w) => w.draw(scene, ws, images, leaf_root, caret_on, local),
                TabView::Manage(w) => {
                    // The already-discovered client, cloned rather than
                    // opened: the Profile prompt's answer is a local Store
                    // write, and this is the seam every other panel uses. No
                    // request happens on the frame thread.
                    w.set_client(workspace.store_client());
                    View::draw(w, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Trash(t) => {
                    View::draw(t, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Tasks(t) => {
                    View::draw(t, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Reminders(r) => {
                    View::draw(r, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Settings(s) => {
                    View::draw(s, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Feedback(f) => {
                    View::draw(f, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Diff(d) => {
                    View::draw(d, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::AiChat(a) => {
                    View::draw(a, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Socials(a) => {
                    View::draw(a, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Session(s) => {
                    View::draw(s, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Run(r) => {
                    View::draw(r, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Result(r) => {
                    View::draw(r, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Canvas(c) => {
                    View::draw(c, scene, workspace, images, leaf_root, caret_on, local)
                }
                TabView::Leaf(l) => {
                    if l.body().rows.is_empty() {
                        crate::mainview::nav_debug::nav_debug!(
                            "[koru:nav] draw rows=0 content={}x{}",
                            view_rect.w as i32,
                            view_rect.h as i32,
                        );
                    }
                    View::draw(l, scene, ws, images, leaf_root, caret_on, local)
                }
            }
            // The page's Context and AI buttons live in its title row, so
            // they draw inside the pane's boundary — and, for the inspector,
            // inside its node too.
            if &path == self.panes.focused_path() {
                if let Some(button) = self.document_ai_button() {
                    button.draw(scene, root, &mut self.engine);
                }
                if let Some(button) = self.document_context_button() {
                    button.draw(scene, root, &mut self.engine);
                }
            }
        }

        // Active split preview: drawn over the target pane inside the content clip.
        if let Some(intent) = self.split_intent.as_ref() {
            if let Some(view_rect) = self.panes.view_rect_for(self.content(), &intent.target) {
                let preview = panes::preview_rect(view_rect, intent.dir);
                let alpha = (now - intent.since).as_secs_f32() / 0.15;
                let alpha = alpha.clamp(0.0, 1.0) * panes::PREVIEW_TINT_ALPHA;
                panes::draw_split_preview(
                    scene,
                    &mut self.engine,
                    root,
                    preview,
                    intent.dir,
                    alpha,
                );
            }
        }

        // While a divider is dragged, each side wears its viewport size — the
        // same `W × H` answer the image resize gives, styled like the sidebar's
        // width tooltip. A drag has no other feedback: the views deliberately
        // do not reflow until release, so without numbers the only thing
        // moving is a hairline.
        if let Some(path) = self.divider_drag.as_ref().map(|d| d.path.clone()) {
            let sides = self.panes.split_sides(self.content(), &path);
            if let Some((a, b)) = sides {
                for side in [a, b] {
                    self.draw_pane_size(scene, root, panes::PaneTree::view_rect_in(c, side));
                }
            }
        }

        scene.pop_layer();
        drop(inspect_content_clip);

        // The title bar a top-row pane's lane shares: the band's tint and its
        // closing hairline first, the lanes over it, then the caption's own
        // furniture over anything of theirs that remains. One **HeaderRow**
        // owns the row in the inspector: the band fill, the tab strip, and
        // every caption control are drawn inside the same rectangle, so they
        // are its children rather than loose records beside it. Drawing the
        // furniture here rather than after the panes changes nothing on
        // screen: nothing between here and the menu reaches into the band,
        // and the tab menu still paints over it by drawing later.
        {
            let cap = self.drawn_caption();
            let _header = koru_draw::inspect::scope(
                "HeaderRow",
                "container",
                root,
                scene::rect(cap.x, cap.y, cap.w, cap.h),
                |r| r.property("layout", "Row / horizontal", "window header host"),
            );
            self.draw_caption_band(scene, root);
            for (path, _) in self.panes.leaf_rects(self.content()) {
                let Some(lane) = self.lane_rect_for(&path) else {
                    continue;
                };
                if lane.y < c.y && lane.w > 0.0 {
                    self.draw_group_strip(scene, root, &path, lane, pointer, &workspace.ws);
                }
            }
            self.draw_caption(scene, root, &workspace.ws);
        }

        // Pane dividers: over every pane's content (a pane never reaches into
        // the divider's pixel — `split_with_divider` carved it out). A divider
        // between two top-row panes extends through the title bar, so their
        // lanes read as two (`divider_draw_rect`).
        let border = theme::p().panel_border;
        for divider in self.panes.divider_rects(self.content()) {
            let d = panes::divider_draw_rect(c, divider);
            scene::fill(scene, root, border, &scene::rect(d.x, d.y, d.w, d.h));
        }

        // The landing slot of a cross-pane drop: a bar in the target strip
        // where the tab will go — the answer VS Code's `|` gives to the same
        // question, in the same accent tint the split preview wears, so
        // "where the tab lands" and "where the pane lands" read as one
        // language. The index is the state; the x is re-derived from the
        // strip's current widths every frame. Outside the clip: a top-row
        // lane is in the title bar.
        if let Some((path, index)) = self.drop_slot.clone() {
            let marker = self.lane_rect_for(&path).and_then(|h| {
                self.panes
                    .group(&path)
                    .and_then(|g| g.slot_marker_x(h, index))
                    .map(|mx| (h, mx))
            });
            if let Some((h, mx)) = marker {
                scene::fill(
                    scene,
                    root,
                    theme::p().accent.with_alpha(panes::PREVIEW_TINT_ALPHA),
                    &scene::rect(
                        (mx - 1.0).max(h.x),
                        h.y + theme::TAB_INSET_TOP,
                        2.0,
                        h.h - theme::TAB_INSET_TOP - theme::TAB_INSET_BOTTOM,
                    ),
                );
            }
        }

        // Over the tab, under everything else: a document that is in the bin has
        // to say so wherever you are in it, including while you are typing.
        self.draw_in_trash_notice(scene, root, &workspace.ws);

        let sb = self.sidebar_rect();
        if sb.w > 0.0 {
            let _inspect_sidebar = koru_draw::inspect::scope(
                "Explore sidebar",
                "container",
                root,
                scene::rect(sb.x, sb.y, sb.w, sb.h),
                |r| r.property("layout", "Column / vertical", "explore host"),
            );
            let avatar = self
                .explore
                .avatar_hash()
                .and_then(|h| images.get(h))
                .map(|a| a.as_ref());
            self.explore.draw(scene, root, &mut self.engine, sb, avatar);
        }
        let ai = self.ai_rect();
        if ai.w > 0.0 {
            let current_workspace = self
                .current_workspace()
                .map(|workspace| koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid()));
            // The AI pane follows the document in the current tab, not the
            // workspace's notion of "active document", which can lag behind or
            // point at a different item when the user switches tabs.
            let active_document = self.tabs().active_tab().document.or(self.active_document);
            let active_label = self.tabs().active_tab().label.clone();
            let active_tab_document = self.tabs().active_tab().document;
            let document_context = active_document.map(|document| {
                let title = workspace
                    .ws
                    .node_of(document)
                    .map(|node| koru_workspace::name_of(&workspace.ws, node))
                    .or_else(|| {
                        if active_tab_document == Some(document) && !active_label.is_empty() {
                            Some(active_label.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "Current document".into());
                let id = koru_store_core::ids::DocumentId::from_uuid(document.as_uuid());
                (id, title)
            });
            self.ai_chat.set_context_workspace(current_workspace);
            self.ai_chat.set_context_document(document_context);
            self.ai_chat.set_viewport(ai);
            self.ai_chat
                .submit_embedded_when_durable(workspace.owed() == 0);
            self.ai_chat.draw(scene, root);
            // AI occupies Context's right-hand slot, so both panes keep their
            // divider and resize grip on the left edge.
            let edge = ai.x;
            scene::fill(
                scene,
                root,
                theme::p().panel_border,
                &scene::rect(edge, ai.y, 1.0, ai.h),
            );
        }
        // The rail's numbers come off a worker thread; this is only where the
        // client reaches it, the same seam History uses. No request is made
        // here, and a window with no store simply draws a rail with no numbers.
        self.task_counts.set_client(workspace.store_client());
        // The delegation job clones this when a person presses the
        // button; no request is made here, and a window with no store
        // refuses honestly instead of opening a thread that answers never.
        self.delegate_client = workspace.store_client();
        // The Tasks tab's page comes off a worker thread too; this is the one
        // place the client reaches it, and no request is made here.
        let client = workspace.store_client();
        self.each_tab_mut(|tab| {
            if let TabView::Tasks(t) = &mut tab.view {
                t.panel_mut().set_client(client.clone());
            }
            false
        });
        self.refresh_tasks_context(workspace);
        let cx = self.context_rect();
        if cx.w > 0.0 {
            self.document_history.set_client(workspace.store_client());
            self.cloud_sync.set_client(workspace.store_client());
            // **The one sweep in this frame that walks the document**, and it
            // runs only while the Cloud tab is showing and only when the tab's
            // own poll has asked for it — see `cloud_images_due`. The image
            // cache lives on the frame thread and the document tree does not
            // reach the maintenance pass with it, so this is where the two
            // meet.
            if self.context_view.section() == crate::panes::context::Section::CloudSync
                && std::mem::take(&mut self.cloud_images_due)
            {
                self.cloud_images = missing_images(&workspace.ws, images);
            }
            let subject = self.document_root();
            self.context_view
                .draw(scene, root, &mut self.engine, cx, subject, &workspace.ws);
        }
        // **After both columns**, and deliberately not from inside the pane's
        // own `draw`: a popover hangs over the active view, so where it lands in
        // the frame is a z-order decision worth making out loud. It lives
        // entirely in the sidebar's region, so it never reaches the caption,
        // which the HeaderRow drew before the panes and only the tab menu may
        // paint over.
        // `popover_visible`, not `popover_open`: one that has been dismissed is
        // deaf but still on screen for the length of its fade, and asking
        // "is it taking input" here would cut that fade off at frame one.
        if self.explore.popover_visible() {
            let avatar = self
                .explore
                .avatar_hash()
                .and_then(|h| images.get(h))
                .map(|a| a.as_ref());
            let area = self.explore_popover_area();
            self.explore
                .draw_popover(scene, root, &mut self.engine, area, self.viewport, avatar);
        }
        if let Some(alpha) = zoom_alpha {
            self.draw_zoom_label(scene, root, zoom, alpha);
        }
        self.draw_reminder_surfaces(scene, root);
        self.draw_sidebar_width(scene, root);
        self.draw_tear(scene, root, pointer);
        // Over the popover, under the menu: the field replaces a row *in* the
        // switcher, so it belongs inside that stack rather than on top of it.
        self.draw_workspace_rename(scene, root);
        // The rail's inline field, over the row the rail reserved for it.
        self.draw_new_list(scene, root);
        self.draw_lifted_diagram_viewer(scene, root);
        self.draw_menu(scene, root);
        // After the menu, because a menu control's chip must not end up under
        // the panel it belongs to. See `draw_tooltip`.
        self.draw_tooltip(scene, root);
        self.draw_new_document(scene, root);
        self.draw_snapshot_prompt(scene, root);
        self.draw_workspace_icon(scene, root);
        self.draw_link_device(scene, root);
        // Over the menu that opened it: the surface outlives the row.
        self.draw_delegation(scene, root);
        // The recording setup modal, over the menu row that opened it too.
        self.draw_recording(scene, root);
        // Above the update banner, below the modals: a toast is transient and a
        // dialog is not, so a dialog must never end up behind one.
        self.draw_toast(scene, root, now);
        self.draw_confirm(scene, root);
        self.draw_import_dialog(scene, root);
        self.draw_updates_modal(scene, root);
        // Channel-mismatch notice: informational modal, under a hard error.
        self.draw_update_newer_modal(scene, root);
        // Last of all, over even the modal: an error is the one thing that must
        // not end up behind something.
        self.draw_error(scene, root);
    }

    /// The tasks rail's "+ New list" field, over the row the rail left for it.
    ///
    /// Taken and put back for `draw_workspace_rename`'s reason: it needs the
    /// engine and the field at once, and both are behind one `&mut self`. A row
    /// scrolled out of the band answers `None`, and nothing is drawn — the
    /// field is still open and still holds the keyboard, which is what makes
    /// Escape work from anywhere.
    fn draw_new_list(&mut self, scene: &mut Scene, root: Affine) {
        let Some(field_rect) = self.new_list_field_rect() else {
            return;
        };
        if let Some(mut field) = self.new_list.take() {
            field.draw(scene, root, &mut self.engine, field_rect);
            self.new_list = Some(field);
        }
    }

    /// The rename field, drawn over the switcher row it is renaming.
    ///
    /// Taken and put back for the same reason `draw_error` is: it needs the
    /// engine and the field at once, and both are behind one `&mut self`.
    fn draw_workspace_rename(&mut self, scene: &mut Scene, root: Affine) {
        let Some(row) = self.workspace_rename_row() else {
            return;
        };
        if let Some(mut field) = self.workspace_rename.take() {
            field.draw(scene, root, &mut self.engine, row);
            self.workspace_rename = Some(field);
        }
    }

    /// The name prompt is a true modal and therefore paints last, including
    /// over the custom caption row.
    fn draw_new_document(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(dialog) = self.new_document.as_mut() {
            dialog.draw(scene, root, &mut self.engine, self.viewport);
        }
    }

    /// The Ctrl+S snapshot prompt, the same modal footing as the name prompt.
    fn draw_snapshot_prompt(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(dialog) = self.snapshot_prompt.as_mut() {
            dialog.draw(scene, root, &mut self.engine, self.viewport);
        }
    }

    /// The emoji-and-colour modal, when one is open. Taken and put back for the
    /// same reason the other two are.
    fn draw_workspace_icon(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(mut m) = self.workspace_icon.take() {
            m.draw(scene, root, &mut self.engine, self.viewport);
            self.workspace_icon = Some(m);
        }
    }

    /// Device linking is global to the profile collection, so it floats above
    /// workspace chrome and reads only whole snapshots delivered by Agent.
    fn draw_link_device(&mut self, scene: &mut Scene, root: Affine) {
        self.refresh_link_device();
        if let Some(mut dialog) = self.link_device.take() {
            dialog.draw(scene, root, &mut self.engine, self.viewport);
            self.link_device = Some(dialog);
        }
    }

    /// The error dialog, when there is one. `None` is the common case and a
    /// fast return, like the menu above it.
    ///
    /// Taken and put back rather than borrowed, because drawing needs the engine
    /// and the dialog at once and they are behind the same `&mut self`.
    fn draw_error(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(dialog) = self.error.take() {
            dialog.draw(scene, root, &mut self.engine, self.viewport);
            self.error = Some(dialog);
        }
    }

    fn draw_import_dialog(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(dialog) = self.import_dialog.take() {
            dialog.draw(scene, root, &mut self.engine, self.viewport);
            self.import_dialog = Some(dialog);
        }
    }

    /// The context menu, last of all — it is the one thing that paints over the
    /// caption row. `None` (the common case) is a fast return.
    /// The focused document's diagram viewer, lifted to the window: drawn
    /// here, after the chrome and outside the content clip, so its scrim
    /// covers the sidebar and the strip and its drawing may grow past the
    /// pane. Under the container's menus and modals, which still stand
    /// above everything a tab owns. The pane's own `draw` skips a lifted
    /// viewer; `sync_viewer_lift` keeps its window current.
    fn draw_lifted_diagram_viewer(&mut self, scene: &mut Scene, root: Affine) {
        self.sync_viewer_lift();
        if self.diagram_viewer_pane().is_none() {
            return;
        }
        if let TabView::Leaf(leaf) = self.active_view() {
            leaf.draw_lifted_viewer(scene, root);
        }
    }

    fn draw_menu(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(menu) = self.menu.clone() {
            let enabled = self.enabled_items(&menu);
            menu.draw_inspected(scene, root, &mut self.engine, self.viewport, &enabled, None);
        }
        // After its parent, because it overlaps it — a submenu drawn first would
        // have the parent's own panel painted over the top of it.
        if let Some((parent_row, sub)) = self.submenu.clone() {
            let enabled = self.enabled_items(&sub);
            let owner = self.menu.as_ref().map(|menu| menu.row_instance(parent_row));
            sub.draw_inspected(
                scene,
                root,
                &mut self.engine,
                self.viewport,
                &enabled,
                owner,
            );
        }
        if let Some((parent_row, nested)) = self.nested.clone() {
            let enabled = self.enabled_items(&nested);
            let owner = self
                .submenu
                .as_ref()
                .map(|(_, menu)| menu.row_instance(parent_row));
            nested.draw_inspected(
                scene,
                root,
                &mut self.engine,
                self.viewport,
                &enabled,
                owner,
            );
        }
    }

    /// A pane's tab lane, drawn wherever it lives: inside the content for most
    /// panes, in the shared title bar for the top row. The lane's rect already
    /// carries the exclusion-zone cut (`lane_rect_for`), so the strip can never
    /// run under the window buttons.
    fn draw_group_strip(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        path: &panes::PanePath,
        lane: Rect,
        pointer: (f32, f32),
        ws: &WorkspaceDoc,
    ) {
        let is_focused = path == self.panes.focused_path();
        let over_header = lane.contains(pointer.0, pointer.1);
        let hovered_tab = if over_header {
            match self.hover {
                Some(Hit::Tab(i)) | Some(Hit::TabClose(i)) => Some(i),
                _ => None,
            }
        } else {
            None
        };
        let hovered_close = if over_header {
            match self.hover {
                Some(Hit::TabClose(i)) => Some(i),
                _ => None,
            }
        } else {
            None
        };
        let hovered_new = over_header && self.hover == Some(Hit::NewTab);
        let scroll_drag_active = is_focused && self.tab_scroll_drag().is_some();
        let Some(group) = self.panes.group_mut(path) else {
            return;
        };
        let show_scrollbar = over_header && (group.tab_scrollbar_visible() || scroll_drag_active);
        let scrollbar_active =
            over_header && (group.tab_scrollbar_active_at(pointer) || scroll_drag_active);
        group.draw_strip(
            scene,
            root,
            &mut self.engine,
            lane,
            hovered_tab,
            hovered_close,
            hovered_new,
            show_scrollbar,
            scrollbar_active,
            &self.note_documents,
            ws,
        );
    }

    /// The title bar's tint and closing hairline — the band a top-row pane's
    /// lane shares. Drawn before the lanes; the caption's own furniture draws
    /// after them, in `draw_caption`.
    fn draw_caption_band(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        // The *drawn* caption, not the settled one: it begins at the pane's
        // drawn right edge, so the logo and the tabs slide along with the pane
        // instead of jumping when it lands.
        let cap = self.drawn_caption();
        scene::fill(
            scene,
            root,
            p.status_bg,
            &scene::rect(cap.x, cap.y, cap.w, cap.h),
        );
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(cap.x, cap.y + cap.h - 1.0, cap.w, 1.0),
        );
    }

    /// Takes the document because a tab's *icon* can depend on it: a document
    /// that is in the bin wears the trash glyph, so a background tab says so
    /// without having to be selected.
    fn draw_caption(&mut self, scene: &mut Scene, root: Affine, _ws: &WorkspaceDoc) {
        let cap = self.drawn_caption();
        // Copied, not borrowed: `ensure_tab_widths` below needs `&mut self`,
        // and a closure holding `self.hover` would keep the whole view frozen
        // for the rest of the function.
        let hover_now = self.hover;
        let hovered = |h: Hit| hover_now == Some(h);
        // **The pane's toggle wears a pane, not the brand.** The corner used to
        // hold the Koru mark, which said whose app this is and nothing about
        // what pressing it does — a logo is not a verb. Its open state is
        // already visible in the layout, so the icon keeps the ordinary rest
        // and hover treatment rather than looking permanently selected.
        let logo_slot = caption::sidebar_toggle(cap);
        // The same chip the `+` wears, from the same function, so the two line
        // up with the tabs between them instead of each picking its own size —
        // this one was 24px against the tabs' 27 and read as a smaller control.
        // `hover_tint` at radius 6, matching the profile and workspace rows.
        koru_ui::icon_button::draw(
            scene,
            root,
            &mut self.engine,
            logo_slot,
            icons::PANEL_LEFT,
            koru_ui::icon_button::State {
                hovered: hovered(Hit::ToggleSidebar),
                active: false,
                disabled: false,
            },
        );
        // Caption controls share the same rest and hover states. Context is a
        // document action and normally lives on the title block; this remains
        // behind the zero-width product switch for compatible chrome geometry.
        if CONTEXT_TOGGLE {
            koru_ui::icon_button::draw(
                scene,
                root,
                &mut self.engine,
                caption::context_toggle_with_host_action(
                    cap,
                    WINDOW_BUTTONS,
                    CONTEXT_TOGGLE,
                    reminder_bell(),
                ),
                icons::PANEL_RIGHT,
                koru_ui::icon_button::State {
                    hovered: hovered(Hit::ToggleContext),
                    active: false,
                    disabled: false,
                },
            );
        }
        // Widths before the tab list borrow — measure needs `&mut self.engine`.
        if reminder_bell() {
            self.reminder_shell.draw_bell(
                scene,
                root,
                &mut self.engine,
                caption::host_action_with_context(
                    cap,
                    WINDOW_BUTTONS,
                    CONTEXT_TOGGLE,
                    reminder_bell(),
                ),
                hovered(Hit::HostAction),
                crate::format::lang(),
            );
        }
        self.draw_branch_pill(scene, root);
        self.draw_offline_chip(scene, root);

        if WINDOW_BUTTONS {
            self.draw_window_buttons(scene, root, cap);
        }
    }

    /// Every hover chip in the window, **after the menu**.
    ///
    /// It used to be the last thing `draw_caption` did, which was fine while
    /// every tooltip belonged to a caption button — and stopped being fine the
    /// moment a menu row grew controls of its own, because `draw_menu` runs
    /// after the caption and the panel would paint straight over the chip.
    ///
    /// Still *before* the dialogs and the error, which is the other half of the
    /// rule: a chip explains the thing under the pointer, and a modal is not
    /// something it may cover.
    fn draw_tooltip(&mut self, scene: &mut Scene, root: Affine) {
        self.tooltip.draw(
            scene,
            root,
            &mut self.engine,
            Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
            Instant::now(),
        );
    }

    fn draw_reminder_surfaces(&mut self, scene: &mut Scene, root: Affine) {
        let cap = self.drawn_caption();
        let bell =
            caption::host_action_with_context(cap, WINDOW_BUTTONS, CONTEXT_TOGGLE, reminder_bell());
        self.reminder_shell.draw_surfaces(
            scene,
            root,
            &mut self.engine,
            self.viewport,
            bell,
            crate::format::lang(),
        );
    }

    /// The pane's width, while it is being dragged.
    ///
    /// A drag has no other feedback: the content deliberately does not follow
    /// until release (see `Panel::committed`), so without a number the only
    /// thing moving is a hairline. It follows the edge rather than sitting in a
    /// corner, because it is about *that* edge.
    ///
    /// Only while dragging, never on hover — the cursor already says the edge is
    /// grabbable, and a number that appears whenever you cross the pane's border
    /// would be noise.
    fn draw_sidebar_width(&mut self, scene: &mut Scene, root: Affine) {
        if self.sidebar_drag.is_none() {
            return;
        }
        let w = self.sidebar.full();
        // Monospace: the digits hold still while the number changes under them.
        let badge =
            koru_ui::KoruMeasurementBadge::measure(&mut self.engine, &format!("{w:.0} px"), 1.0);
        let (tw, _) = badge.size();
        let edge = self.sidebar_rect();
        let x = (edge.x + edge.w + 8.0).min((self.viewport.0 - tw - 4.0).max(4.0));
        // Below the caption so it never covers the tabs, and a little way down
        // from the top so it reads as belonging to the edge rather than to them.
        let y = theme::CAPTION_H + 12.0;
        badge.draw(scene, root, x, y);
    }

    /// The `W × H` chip a pane wears while its divider is being dragged,
    /// centred over the pane's viewport: the image resize's answer, in the
    /// sidebar tooltip's dress. Monospace, so the digits hold still while the
    /// number changes under them. Too small to read, and it stays away — at
    /// the minimum pane size the numbers are already known.
    fn draw_pane_size(&mut self, scene: &mut Scene, root: Affine, view: Rect) {
        if view.w < 120.0 || view.h < 80.0 {
            return;
        }
        let badge = koru_ui::KoruMeasurementBadge::measure(
            &mut self.engine,
            &format!("{} × {}", view.w.round() as u32, view.h.round() as u32),
            1.0,
        );
        let (tw, th) = badge.size();
        let x = view.x + (view.w - tw) * 0.5;
        let y = view.y + (view.h - th) * 0.5;
        badge.draw(scene, root, x, y);
    }

    /// **Offline**, at the end of the tab strip, while the store cannot be
    /// reached.
    ///
    /// The one thing on screen that says a person's work is not being saved
    /// *while it is happening*, rather than ten seconds later when the dialog
    /// arrives. It has to be visible without being alarming: a store being
    /// restarted shows this for a second or two as a matter of course, and a
    /// chip that shouted would be a chip somebody learns to ignore.
    ///
    /// **`bubble_bg`, which is the tooltip's recipe, and no new colour.** The
    /// inverted surface reads as an instrument rather than as part of the page —
    /// which is exactly what this is — and `DESIGN.md` records a standing
    /// preference for not adding `Palette` fields. It is dark in both themes for
    /// the same reason the selection bubble is.
    ///
    /// **One word.** How many edits are at stake is the popover's to say —
    /// clicking the chip is how somebody asks. A count sitting permanently in
    /// the caption reads as an alarm, and this is a state that a store being
    /// restarted puts a window in for a second at a time.
    ///
    /// **The chip itself lives in `koru-shell-chrome`** (`status_chip`), because a popped-out
    /// document draws its own caption and needs the same one. For a while only
    /// this window had it, so the window somebody had put a document into was
    /// the one window that never mentioned the store being away.
    /// The branch pill, when this window came out of a working tree that is
    /// not on `dev` or `main`.
    ///
    /// Several worktrees of this repo are open at once and their windows are
    /// otherwise identical; Welcome has said which branch for a while, but
    /// Welcome is a tab somebody closes and the caption is not.
    ///
    /// **No hit-testing and no popover.** The name is the whole of what it
    /// says, so there is nothing to click — which is also what keeps it out of
    /// `koru_shell_chrome::regions::hit` and its `Hit` enum.
    fn draw_branch_pill(&mut self, scene: &mut Scene, root: Affine) {
        let (Some(pill), Some(name)) = (self.branch_pill(), crate::build_stamp::caption_branch())
        else {
            return;
        };
        koru_shell_chrome::branch::draw(scene, root, &mut self.engine, pill, name);
    }

    fn draw_offline_chip(&mut self, scene: &mut Scene, root: Affine) {
        let Some(chip) = self.offline_chip() else {
            return;
        };
        let Some(trouble) = self.store_trouble.clone() else {
            return;
        };
        let lang = crate::format::lang();
        koru_shell_chrome::status_chip::draw(
            scene,
            root,
            &mut self.engine,
            chip,
            koru_shell_chrome::status_chip::Text {
                label: koru_i18n::t(lang, koru_i18n::Key::StoreOfflineChip),
                title: koru_i18n::t(lang, koru_i18n::Key::StoreOfflineTitle),
                detail: &store_detail(lang, &trouble),
            },
            self.offline_open,
        );
    }

    /// The support window's frame: the caption row with its window buttons
    /// and a title — no tabs, no sidebar — and the chat below it.
    pub(super) fn draw_support_frame(&mut self, scene: &mut Scene, root: Affine) {
        let p = theme::p();
        let cap = self.regions.caption;
        // The page's own ground, not the status colour, and no rule under
        // it: the Messenger's header sits directly below and draws its own,
        // and a grey band above a white header read as two toolbars.
        scene::fill(scene, root, p.bg, &scene::rect(cap.x, cap.y, cap.w, cap.h));
        // The headset ahead of the title: the window's mark, the same glyph
        // as the overflow row that opened it and the floating icon.
        let glyph = self
            .engine
            .icon_layout(koru_draw::icons::HEADSET, theme::text_metrics::LG.size);
        scene::draw_layout(
            scene,
            root,
            &glyph,
            cap.x + 14.0,
            cap.y + (cap.h - glyph.height()) * 0.5,
            Some(p.text),
        );
        let title = self.engine.simple_layout(
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::SupportWindowTitle),
            theme::role::subtitle(),
        );
        scene::draw_layout(
            scene,
            root,
            &title,
            cap.x + 14.0 + glyph.width() + 8.0,
            cap.y + (cap.h - title.height()) * 0.5,
            Some(p.text),
        );
        self.draw_window_buttons(scene, root, cap);
        let content = self.support_content();
        let viewport = self.viewport;
        if let Some(support) = self.support.as_mut() {
            support.draw(scene, root, &mut self.engine, content, viewport);
        }
    }

    fn draw_window_buttons(&mut self, scene: &mut Scene, root: Affine, cap: Rect) {
        let hovered = match self.hover {
            Some(Hit::Window(action)) => Some(action),
            _ => None,
        };
        koru_ui::caption::draw(
            scene,
            root,
            &mut self.engine,
            cap,
            koru_ui::caption::State {
                hovered,
                maximized: self.maximized,
                // The support window has no menu: its ⋯ slot is the pin.
                menu: if self.support_only {
                    koru_ui::caption::MenuVisual::Pin {
                        pinned: self.pinned,
                    }
                } else {
                    koru_ui::caption::MenuVisual::Static
                },
            },
        );
    }

    /// The floating "release to pop out" indicator that follows the pointer
    /// once a tab drag has torn off the strip. It shows the pop-out glyph beside
    /// the tab's name, so the gesture announces what releasing will do — the
    /// same job the cursor would do if the platform offered a pop-out shape.
    fn draw_tear(&mut self, scene: &mut Scene, root: Affine, pointer: (f32, f32)) {
        if !self.drag_tearing() {
            return;
        }
        // The tear indicator promises "release to pop out"; only show it when the
        // pointer is actually outside the content area. Inside the content the split
        // preview speaks for where the tab will land.
        if self.pane_drag.is_some() && self.content().contains(pointer.0, pointer.1) {
            return;
        }
        // A shell or agent tab has nowhere to pop out to, so it gets no
        // pop-out promise — releasing it outside the window simply cancels.
        if self
            .pane_drag
            .as_ref()
            .is_some_and(|d| !self.pane_drag_poppable(d))
        {
            return;
        }
        // Over a target lane the slot marker speaks instead: a top-row lane
        // sits in the title bar, outside the content rect, and dropping there
        // is not a tear-off.
        if self.drop_slot.is_some() {
            return;
        }
        // Over another window that accepts the drag, that window's preview
        // speaks — and the pop-out this pill promises is not what a release
        // will do.
        if self.foreign_targeted {
            return;
        }
        let label = if let Some(d) = self.pane_drag.as_ref() {
            self.group_at(&d.source)
                .iter_tabs()
                .find(|t| t.id == d.tab_id)
                .map(|t| t.label.clone())
        } else {
            self.group_drag()
                .as_ref()
                .and_then(|d| self.tabs().get(d.index).map(|t| t.label.clone()))
        };
        let Some(label) = label else {
            return;
        };
        let p = theme::p();
        let icon = self.engine.icon_layout(icons::POP_OUT, theme::CAPTION_ICON);
        // Cap the label so a long title does not stretch the pill across the
        // window — the strip truncates for the same reason.
        let room = (self.viewport.0 * 0.5).max(80.0);
        let text = self
            .engine
            .simple_layout_wrapped(&label, theme::role::row(), room);
        let pad = 9.0;
        let gap = 8.0;
        let line_h = text.lines().next().map_or(0.0, |l| l.metrics().line_height);
        let w = pad * 2.0 + icon.width() + gap + text.width().min(room);
        let h = line_h.max(icon.height()) + pad * 2.0;
        let x = (pointer.0 - w * 0.5).clamp(4.0, (self.viewport.0 - w - 4.0).max(4.0));
        let y = (pointer.1 - h - 16.0).max(4.0);
        // Raised — it follows the pointer over arbitrary content and has to
        // separate from all of it — but borderless: the accent fill is the edge.
        scene::floating_panel(
            scene,
            root,
            x,
            y,
            w,
            h,
            &scene::Panel::popover(6.0).fill(p.accent).borderless(),
        );
        scene::draw_layout(
            scene,
            root,
            &icon,
            x + pad,
            y + (h - icon.height()) * 0.5,
            Some(p.on_accent),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &text,
            x + pad + icon.width() + gap,
            y + (h - line_h) * 0.5,
            Some(p.on_accent),
        );
    }

    /// The transient "{n}%" indicator, centred on the window. Drawn at a fixed
    /// size by undoing zoom in the transform — otherwise it would swell at 200%
    /// and all but vanish at 50%.
    fn draw_zoom_label(&mut self, scene: &mut Scene, root: Affine, zoom: f32, alpha: f32) {
        let p = theme::p();
        // Undo zoom so the indicator ignores it; `lr` is then just `scale(dpi)`.
        let lr = root * Affine::scale((1.0 / zoom) as f64);
        let text = theme::zoom_percent(zoom);
        let label = self.engine.simple_layout(&text, theme::role::row());
        let pad = 9.0;
        let w = label.width() + pad * 2.0;
        let h = label.height() + pad * 2.0;
        // `self.viewport` is the view's zoomed-down space (W/zoom); multiply by
        // zoom to centre on the true window, in the space `lr` draws in.
        let x = self.viewport.0 * zoom * 0.5 - w * 0.5;
        let y = self.viewport.1 * zoom * 0.5 - h * 0.5;
        // `faded` carries the shadow with it. Fading the panel while its shadow
        // stayed put left the shadow as the last thing on screen, cast by
        // nothing.
        scene::floating_panel(
            scene,
            lr,
            x,
            y,
            w,
            h,
            &scene::Panel::popover(6.0).faded(alpha),
        );
        scene::draw_layout(
            scene,
            lr,
            &label,
            x + pad,
            y + pad,
            Some(fade(p.text, alpha)),
        );
    }
}

/// Scale a colour's alpha by `a`, to fade the zoom indicator uniformly.
fn fade(mut c: Color, a: f32) -> Color {
    c.components[3] *= a;
    c
}

/// Which images in the document showing this device does not hold, and the
/// devices a parked hydration is known to be waiting on.
///
/// **The same two facts the missing-image placeholder already paints**, read
/// from the same cache — `koru_editor::draw::missing_image_detail` and this
/// must never disagree, and they cannot, because neither has its own source.
/// An image whose bytes are here is not counted; an image nobody has named a
/// holder for is counted with no device beside it.
fn missing_images(ws: &WorkspaceDoc, images: &ImageCache) -> (usize, Vec<String>) {
    let Ok(nodes) = ws.walk_all() else {
        return (0, Vec::new());
    };
    let mut missing = 0usize;
    let mut waiting: Vec<String> = Vec::new();
    for (node, _) in nodes {
        let Ok(read) = ws.read(node) else {
            continue;
        };
        let koru_core::node::Content::Blob { hash, mime, .. } = &read.content else {
            continue;
        };
        if !mime.starts_with("image/") || images.get(&hash.0).is_some() {
            continue;
        }
        missing += 1;
        for name in images.pending(&hash.0).unwrap_or_default() {
            if !name.is_empty() && !waiting.contains(name) {
                waiting.push(name.clone());
            }
        }
    }
    (missing, waiting)
}
