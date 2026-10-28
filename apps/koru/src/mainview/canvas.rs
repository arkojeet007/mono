// SPDX-License-Identifier: MPL-2.0

//! **The container's half of a canvas** (CANVAS P1.3, P1.4): which document tab
//! is a canvas, how one is made, and where the gestures a canvas answers — the
//! zoom the app would otherwise take, and the wheel with a position — reach it.
//!
//! A canvas is an ordinary document under the manifest kind
//! [`KIND_CANVAS`]. A tab over one persists as `TabRef::Document` exactly as a
//! page's does, and every door that builds a document tab — Explore, a pill,
//! search, recents, a background open, a drop into a pane, a session restore —
//! asks [`MainView::document_view`] which view to build, so the kind is decided
//! in one place and never at a door. The draft path is the one exception, and
//! a deliberate one: a draft is a *page* waiting for its first keystroke, and a
//! canvas is made at once.
//!
//! Kept out of `mod.rs`, which gets one-line arms that call in here.

use koru_core::explore::KIND_CANVAS;
use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId};
use koru_workspace::Workspace;
use koru_workspace_host::{PaneRequest, View, ZoomIntent};
use winit::event::MouseScrollDelta;

use super::MainView;
use super::panes::PanePath;
use super::tab::TabView;
use crate::canvas_view::CanvasView;

impl MainView {
    /// Whether the listing this window was last handed names `document` as a
    /// canvas. `None` when it does not name the document at all — which is a
    /// different answer from *it is a page*, and the restore path asks the
    /// workspace's own manifest in that case.
    fn listed_canvas(&self, document: DocumentId) -> Option<bool> {
        self.entries
            .as_ref()?
            .iter()
            .find(|entry| entry.id == document)
            .map(|entry| entry.kind == KIND_CANVAS)
    }

    /// Whether `document` is a canvas, as far as this window knows.
    pub(super) fn is_canvas_document(&self, document: DocumentId) -> bool {
        self.listed_canvas(document).unwrap_or(false)
    }

    /// **A canvas this build hides** (`ENABLE_CANVAS` off, koru-features):
    /// every door that builds a document tab asks this first and opens
    /// nothing, so a search result, a link or a background open cannot put
    /// a canvas in a tab the Canvases view and the `+` menu no longer offer.
    /// A canvas is not opened as a page instead: its children are placed
    /// objects, and the page editor would let a person reshape them as rows.
    /// (The one exception is a door that opened before the listing named the
    /// kind — see [`adopt_canvas_kinds`](Self::adopt_canvas_kinds).)
    pub(super) fn canvas_hidden(&self, document: Option<DocumentId>) -> bool {
        !koru_features::canvas() && document.is_some_and(|d| self.is_canvas_document(d))
    }

    /// **The view a document tab is built with** — the one decision every door
    /// that opens a document makes, made here from the document's manifest
    /// kind: a [`CanvasView`] for a canvas, the document editor's [`Leaf`] for
    /// everything else.
    ///
    /// `document` is the tab's manifest id: the active document on a landing,
    /// the named one on a background open. `None` — a workspace with no
    /// manifest, which is every workspace in the editor's own suite — is a page.
    ///
    /// [`Leaf`]: crate::leaf::Leaf
    pub(super) fn document_view(
        &self,
        ws: &WorkspaceDoc,
        root: NodeId,
        document: Option<DocumentId>,
    ) -> TabView {
        match document {
            Some(document) if self.is_canvas_document(document) => {
                TabView::Canvas(CanvasView::over(document, root, ws))
            }
            _ => TabView::Leaf(self.document_leaf(ws, root)),
        }
    }

    /// [`document_view`](Self::document_view) for a tab **coming back from the
    /// session file**, or `None` for a canvas this build hides — session
    /// restore drops that tab, as it drops a hidden Tasks or agent tab.
    ///
    /// Restore runs before the shell has pushed this window its first listing,
    /// so the listing may not name the document yet; the workspace being
    /// restored from always can, through its manifest. The listing still wins
    /// when it has an answer — it is the same manifest, and it is already in
    /// hand. A page is built bare, as the restore path always has; the entity
    /// captions arrive with the listing.
    pub(super) fn restored_document_view(
        &self,
        workspace: &Workspace,
        doc: &WorkspaceDoc,
        root: NodeId,
        document: DocumentId,
    ) -> Option<TabView> {
        let canvas = self.listed_canvas(document).unwrap_or_else(|| {
            workspace
                .document_kind(document)
                .is_some_and(|kind| kind == KIND_CANVAS)
        });
        Some(if !canvas {
            TabView::Leaf(crate::leaf::Leaf::document(doc, root))
        } else if koru_features::canvas() {
            TabView::Canvas(CanvasView::over(document, root, doc))
        } else {
            return None;
        })
    }

    /// **Tabs built before the listing knew they were canvases become canvas
    /// tabs when it does**, and canvas tabs are dressed again — a page an
    /// output node links may have a new title. Called from `set_entries`.
    ///
    /// Every door asks [`document_view`](Self::document_view), but a door can
    /// open before the listing that names the kind has arrived — a
    /// cross-workspace landing announces the document before the new
    /// workspace's listing is pushed. Such a tab was built as a page over the
    /// canvas's root; it keeps its place, its name, its pin and its home, and
    /// only what it is looked at through changes. The reverse cannot arise: a
    /// document's kind never changes, and a canvas tab is only ever built from
    /// a listing that said so.
    pub(super) fn adopt_canvas_kinds(&mut self) {
        // An output node's page may have been renamed, or listed at last.
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Canvas(canvas) = &mut tab.view {
                    canvas.request_decor();
                }
            }
        }
        // A build that hides canvases builds no canvas tab, here either. A
        // page opened over a canvas before the listing named it — a
        // cross-workspace landing — stays the page it was built as.
        if !koru_features::canvas() {
            return;
        }
        let Some(entries) = self.entries.as_ref() else {
            return;
        };
        let canvases: std::collections::HashSet<DocumentId> = entries
            .iter()
            .filter(|entry| entry.kind == KIND_CANVAS)
            .map(|entry| entry.id)
            .collect();
        if canvases.is_empty() {
            return;
        }
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let Some(document) = tab.document.filter(|d| canvases.contains(d)) else {
                    continue;
                };
                if tab.draft.is_some() || tab.whats_new.is_some() {
                    continue;
                }
                let TabView::Leaf(leaf) = &tab.view else {
                    continue;
                };
                let Some(root) = View::document_root(leaf) else {
                    continue;
                };
                tab.view = TabView::Canvas(CanvasView::new(document, root));
                tab.invalidate_size();
                changed = true;
            }
        }
        if changed {
            self.mark_tab_widths_stale();
        }
    }

    /// **New canvas**: the `+` menu's Canvases flyout and the empty Canvases
    /// section's button.
    ///
    /// **Made at once**, not drafted: a canvas is created when it is chosen
    /// (the maintainers' call for this milestone), so the ask goes straight to the
    /// shell, which registers it, seeds its root, lists it and announces it —
    /// and the announcement lands it in a tab here like any new document. The
    /// title is asked for **empty**, as a new page's is: *Untitled* is a
    /// display fallback, never a name somebody has to delete.
    pub(super) fn new_canvas(&mut self) -> bool {
        if !koru_features::canvas() {
            return false;
        }
        self.ask_pane(PaneRequest::CreateCanvas(String::new()))
    }

    /// The `+` menu's recents **split by kind**: `(canvases, pages)`, for the
    /// Canvases and Documents flyouts. Order is kept on both sides, so each
    /// flyout lists its kind most recent first.
    #[allow(clippy::type_complexity)]
    pub(super) fn split_canvas_recents(
        &self,
        recents: Vec<(DocumentId, String)>,
    ) -> (Vec<(DocumentId, String)>, Vec<(DocumentId, String)>) {
        recents
            .into_iter()
            .partition(|(id, _)| self.is_canvas_document(*id))
    }

    /// **Primary+wheel or a pinch over a canvas zooms the canvas**, not the app.
    ///
    /// The shell offers it here first (`View::zoom_wheel`); this answers `true`
    /// only when the pointer is over the view of a pane whose active tab is a
    /// canvas, nothing modal is standing over the window, and that canvas
    /// claims it — with the raw delta and the pointer in the canvas's own
    /// space, the pane's origin subtracted. Anything else declines, and the
    /// display zoom does what it always did.
    pub(super) fn canvas_zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) -> bool {
        if self.blockers().is_blocking() || !self.content().contains(x, y) {
            return false;
        }
        let Some(path) = self.pane_at(x, y).filter(|p| self.panes.contains_group(p)) else {
            return false;
        };
        let Some(rect) = self.panes.view_rect_for(self.content(), &path) else {
            return false;
        };
        // Over the pane's tab lane is chrome, not the canvas.
        if !rect.contains(x, y) {
            return false;
        }
        match self.panes.group_mut(&path).map(|group| group.active_view()) {
            Some(TabView::Canvas(canvas)) => {
                View::zoom_wheel(canvas, delta, x - rect.x, y - rect.y)
            }
            // A document claims it only while its diagram viewer is up, for
            // the drawing; a plain page declines and the app zooms.
            Some(TabView::Leaf(leaf)) => View::zoom_wheel(leaf, delta, x - rect.x, y - rect.y),
            _ => false,
        }
    }

    /// **Ctrl+= / Ctrl+- / Ctrl+0 on a canvas zoom the canvas**, not the app —
    /// [`canvas_zoom_wheel`](Self::canvas_zoom_wheel)'s keyboard half.
    ///
    /// The keyboard follows focus where the wheel follows the pointer (see
    /// `wheel_pane`), so this asks the focused pane's active tab. Declined —
    /// the display zoom's again — while a modal stands, while the explore tree
    /// or the AI pane holds the keyboard, or when that tab is not a canvas.
    pub(super) fn canvas_zoom_key(&mut self, intent: ZoomIntent) -> bool {
        if self.blockers().is_blocking()
            || (self.ai_visible() && self.ai_focused)
            || (self.sidebar.is_open() && self.explore.has_focus())
        {
            return false;
        }
        match self.active_view() {
            TabView::Canvas(canvas) => View::zoom_key(canvas, intent),
            TabView::Leaf(leaf) => View::zoom_key(leaf, intent),
            _ => false,
        }
    }

    /// The pointer in `path`'s view space — what a canvas is handed with the
    /// plain wheel, which carries no position of its own. The pane's content
    /// rect when it has one, the content rect when it does not.
    pub(super) fn pane_local(&self, path: &PanePath, x: f32, y: f32) -> (f32, f32) {
        let rect = self
            .panes
            .view_rect_for(self.content(), path)
            .unwrap_or_else(|| self.content());
        (x - rect.x, y - rect.y)
    }
}

mod wire;

#[cfg(test)]
mod tests;
