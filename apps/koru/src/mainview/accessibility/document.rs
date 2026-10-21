// SPDX-License-Identifier: MPL-2.0

//! **The document a person is actually reading, in the shipping window.**
//!
//! One sentence, and what it excludes: *this module turns the active tab's real
//! editor into the ordered paragraphs, runs, selection and inline objects a
//! screen reader reads, keeps the identities stable across a rebuild, and hands
//! a request back to the editor action that already performs it.* It excludes
//! what a paragraph *is* — that is `koru_editor::accessibility`'s, and nothing
//! here shapes text, computes a rectangle or counts a character. It excludes
//! identity, publishing and stale-action refusal, which are
//! `koru_workspace_host::accessibility`'s.
//!
//! **One request in this file writes, and it is not text.** A checklist row's
//! box is a control beside the paragraph, not part of it, and
//! [`MainView::document_check_action`] hands a tick to `Body::set_done` — the
//! same call one line below the press in `Editor::mouse_down`. Everything about
//! the paragraph's *text* stays as ADR-342 left it: `Action::SetValue` and
//! `Action::ReplaceSelectedText` are refused in
//! `koru_editor::accessibility::actions`, which has no write path at all, and
//! [`MainView::document_action`] still takes no `WorkspaceDoc` so it could not
//! acquire one by accident.
//!
//! Until Plan 1b.3 the active tab published a named `TabPanel` with **no
//! children** and the incremental projection ran only in the interactive proof.
//! This is where the two meet: `koru.exe` publishes the real document, and it
//! publishes it as a difference.
//!
//! ## The shape of it
//!
//! ```text
//! TabPanel   the active tab's region      koru:wN/vV/s:shell#content
//! └── Group      the document body        koru:wN/vV/s:body#root      label = the title
//!     ├── MultilineTextInput  a paragraph koru:wN/vV/n:<node uuid>#text
//!     │   ├── TextRun  …                  (not a control; addressed only inside a text position)
//!     │   ├── CheckBox  the drawn box     koru:wN/vV/n:<node uuid>#check   (a checklist row)
//!     │   ├── Link | Label  a pill        koru:wN/vV/n:<attr uuid>#pill
//!     │   └── Label   the task lifecycle  koru:wN/vV/n:<node uuid>#state   (a task)
//!     ├── Image     a diagram's picture      koru:wN/vV/n:<node uuid>#text   (a `mermaid` block, drawn or drawing)
//!     └── …
//! ```
//!
//! **The picture is the first block with no text and a name of its own.** A
//! `mermaid` block showing its drawing — or the placeholder waiting for one —
//! is published as `Role::Image` named with its narration, under the same
//! locator its text would have, with Focus and ScrollIntoView and no text
//! pattern; opening the text turns it back into the paragraph above. A block
//! whose text is showing because it does not draw keeps its text and wears
//! *Diagram, not drawn: …* as its name, so the reason is heard first. See
//! [`picture`].
//!
//! **The box and the lifecycle are two controls because they are two facts.**
//! `Paragraph::is_task` asks whether this row draws a checkbox, which a plain
//! checklist item does without ever being a task; `Paragraph::task` asks
//! whether `koru_core::tasks::is_task` holds. `docs/features/tasks/SPEC.md` §5 lets
//! them disagree — a task with structural children has a box that is only a box
//! — so a reader hears *checked* about the one and *completed* about the other,
//! and hears both on a row where they differ. One node saying both would be a
//! lie about the model.
//!
//! **`Role::Group`, not `Role::Document`.** A `Document` acquires a Text
//! pattern of its own — `accesskit_consumer` finds text runs *through*
//! intermediate nodes — and Koru would then publish two carets for one editor,
//! which §5 forbids. That is Q-B1, parked by the maintainers pending a listening
//! session, and this plan does not change the default.
//!
//! **The locators are the ones Plan 1b.1's proof already searches by.** The
//! interactive harness and this module mint the same occurrence for the same
//! paragraph — `s:body#root`, `n:<uuid>#text`, `n:<attr uuid>#pill` — so the
//! external proof and the shipping window are addressable the same way.
//!
//! ## The three publishes, and which one a keystroke gets
//!
//! | What happened | What goes out |
//! | --- | --- |
//! | a client attached, or one detached | the whole window, the whole body |
//! | the chrome changed — a tab, a menu, the save status | the shell subtree, the container with its **complete child list**, and only the paragraphs that changed |
//! | anything else — a keystroke, a caret move, a peer edit | the changed paragraphs alone, as loose nodes whose parent the client already holds |
//!
//! The middle row is the one worth reading twice. An AccessKit node is replaced
//! whole, so re-sending the container drops every child it does not name — but
//! `SemanticNode::child_order` names them without carrying them, so opening a
//! menu over a thousand-paragraph document re-sends one container and no
//! paragraphs.
//!
//! ## Why the state is a `RefCell`
//!
//! A text run's AccessKit id has to survive `Body::rebuild`, which throws every
//! `Paragraph` away and makes it again — so the ids, parley's span map and the
//! record of what was last published live beside the *view*, not on a row. That
//! makes `DocumentAccessibility::paragraph` a `&mut self` method, and
//! `View::semantics` is `&self`, deliberately: the `&self` is what makes
//! `Editing::reflow` and the Store unreachable from a semantic query. Plan
//! 1b.2's answer was a `RefCell` beside the view rather than a widened seam,
//! and this is that `RefCell`. What mutates behind it is accessibility
//! bookkeeping; the document, its layout and its caret are read through
//! `&Paragraph` and cannot be touched.

use std::cell::RefCell;
use std::collections::HashMap;

use accesskit::{Action, ActionData, Role};
use koru_core::NodeId as KoruNodeId;
use koru_core::schema::WorkspaceDoc;
use koru_editor::accessibility::{
    DocumentAccessibility, Publish, RowFacts, RowText, RowUpdate, ViewGeometry, actions, events,
};
use koru_i18n::{Key, t};
use koru_workspace_host::accessibility::{
    ControlPart, ObjectKey, Occurrence, Revision, Runs, Scope, SemanticNode, TextBody,
};
use koru_workspace_host::routing::{EditableTarget, TabId};

use super::Shell;
use crate::leaf::Leaf;
use crate::mainview::MainView;
use crate::mainview::tab::TabView;

/// The document body region, as one object. A locator then reads
/// `.../s:body#root`, and it is the same surface whichever document is showing:
/// this is *where the document is in this window*, not which document it is.
pub(crate) const BODY: &str = "body";

/// The part of a paragraph that carries its text. A code keyword, never a
/// label. The same one Plan 1b.1's proof searches by.
pub(crate) const TEXT: ControlPart = ControlPart::new("text");

/// An inline object inside a paragraph — a Koru pill. Scoped by the
/// **attribute's** own uuid, so two pills in one paragraph are two controls and
/// a pill keeps its identity when the text around it is edited.
pub(crate) const PILL: ControlPart = ControlPart::new("pill");

/// **The box a checklist row draws**, as its own control — the same square
/// `Editor::checkbox_rect` resolves a press against. It carries the `done`
/// mirror and nothing else.
pub(crate) const CHECK: ControlPart = ControlPart::new("check");

/// **The canonical task lifecycle**, as a sentence a reader hears. A second
/// control rather than a second reading of the box, because
/// [`SPEC`](../../../../../docs/features/tasks/SPEC.md) §5 lets the two disagree: a task
/// with structural children has a box that is only a box, and a completed task
/// still holding unfinished work is concurrent residue. Publishing one number
/// for both would be a lie about the model.
pub(crate) const STATE: ControlPart = ControlPart::new("state");

/// What one document view has published, and to whom.
///
/// Keyed by **tab instance**, not by document: two tabs on one document are two
/// places, and each holds its own run ids. Pruned to the tabs that still exist
/// on every publish, so a long session does not accumulate one entry per tab
/// ever opened.
#[derive(Default)]
pub(crate) struct Bodies {
    by_tab: HashMap<u64, DocumentAccessibility>,
    /// The tab whose body the client is currently holding, when it is holding
    /// one. `None` after a publish that carried no body at all — a blocker
    /// standing, or a tab that is not a document — which is exactly when the
    /// next body publish has to be a whole one, because the paragraphs the
    /// client had were dropped with their container.
    published_tab: Option<u64>,
    /// The container's label as last published. A rename touches no row, so a
    /// difference cannot see it by looking at the paragraphs.
    published_title: Option<String>,
    /// The focused editor viewport last published, in logical window coordinates.
    published_viewport: Option<koru_draw::geometry::Rect>,
    /// Whether the paragraphs went out as read-only.
    ///
    /// **A property of the projection, not of the layout.** A document becoming
    /// read-only — or its find bar taking the keyboard — changes nothing the
    /// per-row cache compares, so a difference would leave every unchanged
    /// paragraph telling a client it is still editable. Noticing it here is
    /// what forces the whole body out again.
    published_read_only: Option<bool>,
    /// The pills published under each row, so one that has gone can be retired.
    published_pills: HashMap<KoruNodeId, Vec<uuid::Uuid>>,
    /// What each row last went out saying about its box and its lifecycle, so
    /// a control that has left it can be retired.
    ///
    /// **A row that stops being a task keeps its paragraph and loses a child**,
    /// which is exactly the case ADR-343-retiredsurfaces was written about: the
    /// body is published as a difference, so nothing but this record can tell
    /// the host that `n:<uuid>#state` is no longer on screen, and an id left in
    /// the occurrence map goes on being routable.
    published_facts: HashMap<KoruNodeId, koru_editor::accessibility::RowFacts>,
    /// Controls that have left the tree, waiting for the host to drain them.
    /// See `View::accessibility_retired`.
    retired: Vec<Occurrence>,
    /// The chrome fingerprint the client is holding. `None` means it holds
    /// none, so the next publish carries the shell whatever else it carries.
    published_chrome: Option<u64>,
}

impl Bodies {
    /// Take what has gone since the host last asked.
    pub(crate) fn take_retired(&mut self) -> Vec<Occurrence> {
        std::mem::take(&mut self.retired)
    }

    /// Forget what has gone, because a whole snapshot is about to say so by
    /// omission. `PaneAccessibility::publish` empties the occurrence map for
    /// one, so there is nothing left to take away — and a list nobody drains is
    /// a list that grows for the life of the window.
    pub(crate) fn retirement_is_moot(&mut self) {
        self.retired.clear();
    }
}

/// The active tab's editor, when the active tab is a document.
fn leaf(view: &MainView) -> Option<&Leaf> {
    match &view.tabs().active_tab().view {
        TabView::Leaf(leaf) => Some(leaf),
        _ => None,
    }
}

fn leaf_mut(view: &mut MainView) -> Option<&mut Leaf> {
    let index = view.tabs().active();
    match view.tabs_mut().get_mut(index).map(|tab| &mut tab.view) {
        Some(TabView::Leaf(leaf)) => Some(leaf),
        _ => None,
    }
}

fn editor(leaf: &Leaf) -> &koru_editor::Editor {
    let Leaf::Document(document) = leaf;
    &document.editor
}

impl MainView {
    /// **Whether the chrome the client holds is still the chrome this window
    /// has.**
    ///
    /// Split out of [`semantics_revision`](MainView::semantics_revision) rather
    /// than recomputed: the window's revision is that number *plus* the
    /// document's, and a publish needs to know which half moved. The whole
    /// revision decides whether to publish at all; this one decides whether the
    /// publish has to carry the shell.
    pub(crate) fn chrome_revision(&self) -> u64 {
        let mut rev = Revision::new();
        self.revise_chrome(&mut rev);
        rev.finish()
    }

    /// Everything the document body contributes to this publish.
    ///
    /// `Some(container)` when the caller is publishing the shell and wants a
    /// node to nest under the content region; the loose form is
    /// [`body_delta`](Self::body_delta).
    pub(crate) fn body_semantics(&self, cx: &Shell, scope: Scope) -> Option<SemanticNode> {
        let (container, _) = self.project(cx, true, scope)?;
        container
    }

    /// The paragraphs that changed, as loose nodes whose parent the client
    /// already holds. Only ever a [`Scope::Delta`]: a whole window goes out
    /// through [`body_semantics`](Self::body_semantics), nested under the
    /// content region with the rest of the shell.
    pub(crate) fn body_delta(&self, cx: &Shell) -> Vec<SemanticNode> {
        match self.project(cx, false, Scope::Delta) {
            Some((container, rows)) => container.into_iter().chain(rows).collect(),
            None => Vec::new(),
        }
    }

    /// The one place the body is projected.
    ///
    /// `nested` says whether the caller is going to hang the result under the
    /// content region. When it is, the container always comes back and carries
    /// the paragraphs this publish is sending; when it is not, the container
    /// comes back **only** if its label, child list or viewport moved, and the
    /// paragraphs come back loose — because a node published twice is a node
    /// with two parents.
    fn project(
        &self,
        cx: &Shell,
        nested: bool,
        scope: Scope,
    ) -> Option<(Option<SemanticNode>, Vec<SemanticNode>)> {
        let leaf = leaf(self)?;
        let editor = editor(leaf);
        let tab = self.tabs().active_tab().id;
        // Whether a typed character would land in this body right now. **The
        // same answer the keyboard gives**, through the router that already
        // owns the question: a read-only document, an open document menu, a
        // focused find bar and an open overlay each take no text, and each of
        // them is a document a screen reader must be told is not editable.
        //
        // **It does not reach a Windows client**, and it is published anyway.
        // `accesskit_windows` carries `IsReadOnly` only on `ValuePattern` and
        // offers that pattern only to a node with a `value`, which a paragraph
        // deliberately has none of — a `ValuePattern` also offers `SetValue`,
        // and no accessibility request writes a Koru document. So this is right
        // in the schema, read by other platforms, and invisible on this
        // adapter, exactly as `level` is (`guide/contributing/deps/ACCESSKIT.md`). What it
        // *does* do here is decide whether a selection request is performed at
        // all, in `document_action` below.
        let read_only = self.editable_target() != EditableTarget::Tab(TabId(self.tabs().active()));
        let pane = self.focused_view_rect();
        let viewport = body_viewport(editor, pane);
        let mut state = self.bodies.borrow_mut();

        // **A body the client is not holding is published whole.** Its
        // paragraphs went when their container did — a blocker stood over them,
        // or another tab was showing — so a difference would name children that
        // are not there, and a dangling child reference is not a wrong answer:
        // `accesskit_consumer` resolves one with an `unwrap()`.
        //
        // …and so is one whose read-only state moved, for the reason
        // [`published_read_only`](Bodies::published_read_only) gives — and so
        // is **every** body in a whole snapshot, whatever this view remembers
        // publishing. A client that detached and came back holds nothing, and
        // `PaneAccessibility` empties its occurrence map for a whole snapshot,
        // so a body that answered one with a difference would publish a focus
        // naming a paragraph that is in no map anywhere.
        let held = scope == Scope::Delta
            && state.published_tab == Some(tab)
            && state.published_read_only == Some(read_only);
        let how = if held { Publish::Delta } else { Publish::Whole };
        if !held {
            state.retire_other_tabs(cx, tab);
        }

        let live: Vec<u64> = self.tabs().iter().map(|tab| tab.id).collect();
        state.by_tab.retain(|id, _| live.contains(id));
        let access = state.by_tab.entry(tab).or_default();
        if !held {
            // The ids and the cache are still good — the layout did not change
            // because somebody stopped looking at it — so this costs no
            // reshaping. What is forgotten is only what the client has.
            access.forget_published();
        }
        let update = access.body(
            &editor.body.rows,
            ViewGeometry {
                origin: (pane.x, pane.y),
                content_x: editor.view.content_x,
                scroll: editor.view.scroll,
                viewport_h: editor.view.viewport.1,
            },
            editor.view.active.then_some(editor.body.focus),
            how,
            |id| paragraph_occurrence(cx, id).node_id(),
        );
        // **§5, checked before a client can see it.** A published selection
        // naming a run this same update takes away is an abort inside a UI
        // Automation callback rather than a wrong answer. Cheap — one pass over
        // the rows the update carries, which for a keystroke is one row.
        debug_assert_eq!(
            events::coherent(&update),
            Ok(()),
            "the document body published an incoherent update"
        );

        let title = self.document_title(cx, leaf);
        let renamed = state.published_title.as_deref() != Some(title.as_str());
        let moved = state.published_viewport != Some(viewport);
        let reordered = update.order.is_some();

        state.retire_departed(cx, &update);
        let rows: Vec<SemanticNode> = update
            .rows
            .iter()
            .map(|row| {
                state.remember_pills(row);
                paragraph(cx, row, read_only)
            })
            .collect();
        let order: Vec<accesskit::NodeId> = update
            .order
            .clone()
            .unwrap_or_else(|| editor.body.rows.iter().map(|row| row.id).collect())
            .iter()
            .map(|id| paragraph_occurrence(cx, *id).node_id())
            .collect();

        state.published_tab = Some(tab);
        state.published_title = Some(title.clone());
        state.published_viewport = Some(viewport);
        state.published_read_only = Some(read_only);

        if !nested && !renamed && !reordered && !moved {
            // Neither the container's name, child list nor viewport moved, so it is
            // exactly what the client already holds and is left out entirely.
            return Some((None, rows));
        }
        let container = SemanticNode::new(cx.surface(BODY, ControlPart::ROOT), Role::Group)
            .with_label(title)
            .with_bounds(viewport)
            // **How many paragraphs there are, on the container**, which is
            // where AccessKit looks for a total — and where a `Role::Group`
            // swallows it, because the consumer only walks up to a parent whose
            // role is one it counts as a container of selectable children
            // (`Tree`, `TabList`, `Menu`, …). Published anyway: it is right in
            // the schema, other platforms read it, and the day Q-B1 settles on
            // a different container role it is already here. A paragraph
            // carries its own zero-based position beside it, which *does*
            // arrive. See `guide/contributing/deps/ACCESSKIT.md`.
            .with_size_of_set(order.len())
            // A node is republished whole, so the container states its complete
            // child list even when only its label moved.
            .with_child_order(order)
            .with_children(rows);
        Some((Some(container), Vec::new()))
    }

    /// The document's own name, which is what a screen reader calls the body.
    ///
    /// **User content**: never translated, never normalised, never truncated.
    /// The fall-back for a document with no name at all is authored and does go
    /// through `koru-i18n`.
    fn document_title(&self, cx: &Shell, leaf: &Leaf) -> String {
        let tab = self.tabs().active_tab();
        if !tab.label.is_empty() {
            return tab.label.clone();
        }
        let editor = editor(leaf);
        editor
            .body
            .rows
            .first()
            .filter(|_| editor.body.is_title_row(0))
            .map(|row| row.text.clone())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| t(cx.lang, Key::A11yDocumentBody).to_string())
    }

    /// **Which paragraph holds the keyboard**, when the active tab is a
    /// document and its editor is the thing with it.
    ///
    /// Read from `ViewState::active` and `Body::focus` — the same two states the
    /// caret is drawn from. Not a second focus: §5 is explicit that accessible
    /// focus is a function over the state the focus ring is painted from.
    pub(crate) fn document_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let leaf = leaf(self)?;
        let editor = editor(leaf);
        if !editor.view.active {
            return None;
        }
        let row = editor.body.rows.get(editor.body.focus)?;
        Some(paragraph_occurrence(cx, row.id))
    }

    /// What the open document's projection has done — shapings, republished
    /// rows, refused requests.
    ///
    /// **Counters, not a stopwatch.** The numbers §7.3 asks for, read from the
    /// same state the shell publishes from, so a test can say *a keystroke
    /// published one row and shaped one* rather than *it felt quick*.
    ///
    /// Test-only because nothing in the shipping window reads it: the numbers
    /// that matter in a running session are `PaneAccessibility::counters`, which
    /// the one-shot dump already prints.
    #[cfg(test)]
    pub(crate) fn document_counters(&self) -> koru_editor::accessibility::Counters {
        let tab = self.tabs().active_tab().id;
        self.bodies
            .borrow()
            .by_tab
            .get(&tab)
            .map(DocumentAccessibility::counters)
            .unwrap_or_default()
    }

    /// Fold the document's state into this window's revision.
    ///
    /// **What is in it and what is not.** In: every row's identity, its shaping
    /// stamp, whether it has a layout at all, where that layout sits, and the
    /// focused row's caret and selection — because each of those changes what
    /// is published. Also the scroll, because §5 says *scrolling updates
    /// geometry* and a paragraph that scrolls into the band acquires a
    /// rectangle it did not have. Not in: anything that moves per frame without
    /// changing a published value — a caret blink has no state here, and a
    /// hover none either.
    ///
    /// One pass over the rows, and **only while a client is attached**
    /// (`PaneAccessibility::listening`), which is the same bargain the explore
    /// column's revision makes: an ordinary session never runs this at all.
    pub(crate) fn document_revision(&self, rev: &mut Revision) {
        let Some(leaf) = leaf(self) else {
            rev.byte(0);
            return;
        };
        rev.byte(1);
        let editor = editor(leaf);
        rev.u64(self.tabs().active_tab().id);
        let viewport = body_viewport(editor, self.focused_view_rect());
        rev.f32(viewport.x)
            .f32(viewport.y)
            .f32(viewport.w)
            .f32(viewport.h);
        rev.f32(editor.view.scroll);
        rev.f32(editor.view.content_x);
        rev.f32(editor.view.viewport.1);
        rev.bool(editor.view.active);
        // **Where a character would go**, which is what the paragraphs publish
        // as `read_only` — so it has to be in the number that says the tree
        // changed, or a document that quietly became unwritable would go on
        // telling a client it is editable.
        rev.bool(self.editable_target() == EditableTarget::Tab(TabId(self.tabs().active())));
        rev.u64(editor.body.focus as u64);
        rev.u64(editor.body.rows.len() as u64);
        for row in &editor.body.rows {
            rev.uuid(row.id.as_uuid());
            // The stamp `rowlayout::build_row` leaves behind: text, marks,
            // width, kind, heading and depth all reach the layout through a
            // reshape, so one number covers every one of them.
            rev.u64(row.shaped_at());
            rev.bool(row.dirty);
            rev.f32(row.y).f32(row.height);
            // What the row says about itself as a diagram — an answer that
            // landed, a text that opened or closed — which moves no glyph and
            // need not move the stamp or the height.
            match RowFacts::of(row).diagram {
                Some(fact) => rev.byte(1).bool(fact.picture).u64(fact.stamp),
                None => rev.byte(0),
            };
        }
        if let Some(row) = editor.body.rows.get(editor.body.focus) {
            rev.u64(row.selection.anchor().index() as u64);
            rev.u64(row.caret() as u64);
        }
    }

    /// Do what a client asked about a paragraph or a pill of the open document.
    ///
    /// Answers whether anything was done, which is what `View::accessibility_action`
    /// promises — a `false` is the honest answer to the `Focus` Windows sends
    /// before every invoke, and to everything else this document does not
    /// perform.
    ///
    /// **It takes no `WorkspaceDoc`**, where the shell's other action routes do.
    /// That asymmetry is the point: an accessibility request cannot write a Koru
    /// document, and a route that has nothing to write to cannot start.
    pub(crate) fn document_action(
        &mut self,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        let ObjectKey::Model(uuid) = target.object else {
            return false;
        };
        if target.part == PILL {
            // **Announced, not activated.** Q-B2's second half is parked with
            // the maintainers; advertising nothing and performing nothing is the same
            // answer Plan 1b.2 shipped, said in the one place a request could
            // otherwise be quietly accepted.
            return false;
        }
        if target.part != TEXT {
            return false;
        }
        // **Where a typed character would go, asked of the router that owns
        // it.** A selection request while the find bar has focus, an overlay is
        // open, or the document is read-only would move a caret the keyboard
        // cannot move — so it is refused, and refused with the same answer the
        // keyboard gives rather than a second opinion about it.
        let editable = self.editable_target() == EditableTarget::Tab(TabId(self.tabs().active()));
        if action == Action::SetTextSelection && !editable {
            return false;
        }
        let tab = self.tabs().active_tab().id;
        let row = KoruNodeId::from_uuid(uuid);
        // **Lifted out and put back.** The projection state lives behind a
        // `RefCell` on this view and the editor lives on this view too, so a
        // borrow of one is a borrow of the other. Taking the entry out for the
        // duration of the call is the smallest thing that lets the two be held
        // at once, and it cannot lose an entry: the only early return between
        // here and the restore is the one that never took it.
        let Some(mut access) = self.bodies.borrow_mut().by_tab.remove(&tab) else {
            return false;
        };
        let performed = match leaf_mut(self) {
            // Every arm of this is one call into `Editing`, and none of them
            // writes the document. See `koru_editor::accessibility::actions`.
            Some(leaf) => actions::perform(leaf, &mut access, row, action, data).is_ok(),
            None => false,
        };
        self.bodies.borrow_mut().by_tab.insert(tab, access);
        performed
    }

    /// **Tick or untick a checklist row's box**, through the one call the press
    /// already makes.
    ///
    /// A route of its own, and that is the point: [`document_action`] takes no
    /// `WorkspaceDoc` on purpose — *an accessibility request cannot write a Koru
    /// document, and a route that has nothing to write to cannot start* — and
    /// handing it one so it could reach this would have spent that invariant on
    /// a control it is not about. ADR-342's *three actions and no write* is
    /// about a paragraph's **text**: the mutating text requests stay refused in
    /// `koru_editor::accessibility::actions`, which still has no write path at
    /// all.
    ///
    /// What this reaches is [`Body::set_done`](koru_editor::Body::set_done) —
    /// the **pointer's own owner**, one line below the press in
    /// `Editor::mouse_down`. It is that call, and not this one, that decides
    /// whether the box is the canonical lifecycle (`tasks::checkbox_completes`
    /// holds, so the six commands run) or only the `done` mirror. Nothing here
    /// decides it, and nothing here writes a task attribute.
    ///
    /// [`Body::set_done`]: koru_editor::Body::set_done
    pub(crate) fn document_check_action(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Occurrence,
        action: Action,
    ) -> bool {
        let ObjectKey::Model(uuid) = target.object else {
            return false;
        };
        if target.part != CHECK || action != Action::Click {
            return false;
        }
        // **A document nothing may be typed into is a document nothing may be
        // ticked in**, asked of the router that already owns the question — the
        // same answer a keystroke gets rather than a second opinion.
        if self.editable_target() != EditableTarget::Tab(TabId(self.tabs().active())) {
            return false;
        }
        use koru_editor::Editing as _;
        let row = KoruNodeId::from_uuid(uuid);
        let Some(leaf) = leaf_mut(self) else {
            return false;
        };
        // **A row that no longer draws a box refuses**, which is the stale
        // request arriving after the row stopped being a checklist item.
        let Some(done) = leaf
            .body()
            .rows
            .iter()
            .find(|candidate| candidate.id == row && candidate.is_task())
            .map(|candidate| candidate.done)
        else {
            return false;
        };
        leaf.body_mut().set_done(ws, row, !done);
        // The second line of the press, for the press's reason: `set_done`
        // rebuilds, and a rebuilt row has no layout until something reflows it.
        leaf.reflow();
        true
    }
}

impl Bodies {
    /// Retire everything another tab's body published, because the client is
    /// about to stop holding it.
    ///
    /// The container itself is **not** retired: `s:body#root` is *where the
    /// document is in this window*, and it survives a change of document the
    /// way the tab strip survives a change of tab.
    fn retire_other_tabs(&mut self, cx: &Shell, keeping: u64) {
        let gone: Vec<u64> = self
            .published_tab
            .into_iter()
            .filter(|tab| *tab != keeping)
            .collect();
        for tab in gone {
            let Some(access) = self.by_tab.get(&tab) else {
                continue;
            };
            let rows: Vec<KoruNodeId> = access.published_order().to_vec();
            for row in rows {
                self.retired.push(paragraph_occurrence(cx, row));
                for pill in self.published_pills.remove(&row).unwrap_or_default() {
                    self.retired.push(pill_occurrence(cx, pill));
                }
                self.retire_facts(cx, row, None);
            }
        }
    }

    /// Retire whatever `row` last published about its box and its lifecycle
    /// that it is no longer publishing. `now` is what it says this time;
    /// `None` is the row leaving altogether.
    fn retire_facts(
        &mut self,
        cx: &Shell,
        row: KoruNodeId,
        now: Option<koru_editor::accessibility::RowFacts>,
    ) {
        let before = match now {
            Some(now) => match self.published_facts.insert(row, now) {
                Some(before) => before,
                None => return,
            },
            None => match self.published_facts.remove(&row) {
                Some(before) => before,
                None => return,
            },
        };
        let now = now.unwrap_or_default();
        if before.checkbox.is_some() && now.checkbox.is_none() {
            self.retired.push(check_occurrence(cx, row));
        }
        if before.task.is_some() && now.task.is_none() {
            self.retired.push(state_occurrence(cx, row));
        }
    }

    /// Retire the paragraphs this update takes away, and any pill that went
    /// with them or has left a surviving paragraph.
    fn retire_departed(&mut self, cx: &Shell, update: &koru_editor::accessibility::BodyUpdate) {
        for row in &update.removed {
            self.retired.push(paragraph_occurrence(cx, *row));
            for pill in self.published_pills.remove(row).unwrap_or_default() {
                self.retired.push(pill_occurrence(cx, pill));
            }
            self.retire_facts(cx, *row, None);
        }
        // **A row that stopped being a task, or stopped drawing a box.** The
        // paragraph survives and loses a child, which nothing else in this
        // difference says out loud.
        for row in &update.rows {
            self.retire_facts(cx, row.id, Some(row.facts));
        }
        for row in &update.rows {
            let now: Vec<uuid::Uuid> = row
                .objects
                .iter()
                .map(|object| object.attr.as_uuid())
                .collect();
            let Some(before) = self.published_pills.get(&row.id) else {
                continue;
            };
            for pill in before {
                if !now.contains(pill) {
                    self.retired.push(pill_occurrence(cx, *pill));
                }
            }
        }
    }

    /// Remember which pills this row went out with. A row a publish **skipped**
    /// did not change its shaping, so its pills did not move either, and
    /// nothing here has to be told about it.
    fn remember_pills(&mut self, row: &RowUpdate) {
        self.published_pills.insert(
            row.id,
            row.objects
                .iter()
                .map(|object| object.attr.as_uuid())
                .collect(),
        );
    }

    /// The chrome fingerprint the client is holding, and whether it is still
    /// current.
    pub(crate) fn holds_chrome(&self, revision: u64) -> bool {
        self.published_chrome == Some(revision)
    }

    pub(crate) fn published_chrome(&mut self, revision: u64) {
        self.published_chrome = Some(revision);
    }

    /// The body is not in this publish at all — a blocker is standing, or the
    /// active tab is not a document. Whatever the client held of it goes with
    /// its container, so the next body publish must be a whole one.
    pub(crate) fn body_withheld(&mut self, cx: &Shell) {
        let Some(tab) = self.published_tab.take() else {
            return;
        };
        let Some(access) = self.by_tab.get(&tab) else {
            return;
        };
        let rows: Vec<KoruNodeId> = access.published_order().to_vec();
        for row in rows {
            self.retired.push(paragraph_occurrence(cx, row));
            for pill in self.published_pills.remove(&row).unwrap_or_default() {
                self.retired.push(pill_occurrence(cx, pill));
            }
            self.retire_facts(cx, row, None);
        }
        self.published_title = None;
        self.published_viewport = None;
    }
}

/// The exact editor paint clip translated by its focused pane draw origin.
/// Pane origin is the draw/hit-test origin; chrome does not move row coordinates.
fn body_viewport(
    editor: &koru_editor::Editor,
    pane: koru_draw::geometry::Rect,
) -> koru_draw::geometry::Rect {
    let clip = editor.view.content_clip();
    koru_draw::geometry::Rect::new(pane.x + clip.x, pane.y + clip.y, clip.w, clip.h)
}
/// One paragraph, as a screen reader meets it.
fn paragraph(cx: &Shell, row: &RowUpdate, read_only: bool) -> SemanticNode {
    if let Some(narration) = row.diagram.as_deref()
        && row.facts.diagram.is_some_and(|diagram| diagram.picture)
    {
        return picture(cx, row, narration);
    }
    let mut node = SemanticNode::new(paragraph_occurrence(cx, row.id), Role::MultilineTextInput)
        // Focus, so an AT can move into a paragraph; SetTextSelection, so it
        // can select; ScrollIntoView, because a paragraph that is not laid out
        // yet is reached by the view's own scroll and by nothing else. Nothing
        // further: an advertised action that performs nothing is worse than one
        // that is absent.
        .with_action(Action::Focus)
        .with_action(Action::SetTextSelection)
        .with_action(Action::ScrollIntoView)
        .with_position(row.index);
    if let Some(bounds) = row.bounds {
        node = node.with_bounds(bounds);
    }
    // **A diagram whose text is showing because it does not draw** wears the
    // reason as its name — *Diagram, not drawn: Line 7: …* — so a reader
    // hears why before the code text it can then fix. The text stays the
    // paragraph's own, read and selected like any other row's.
    if let Some(narration) = &row.diagram {
        node = node.with_label(narration.clone());
    }
    let objects: Vec<SemanticNode> = row
        .objects
        .iter()
        .map(|object| {
            let mut pill =
                SemanticNode::new(pill_occurrence(cx, object.attr.as_uuid()), object.role())
                    // The caption the document already carries. **User content**, never
                    // translated and never invented.
                    .with_label(object.label.clone());
            if let Some(value) = object.value() {
                pill = pill.with_value(value.to_string());
            }
            if let Some(bounds) = object.bounds {
                pill = pill.with_bounds(bounds);
            }
            pill
        })
        .collect();
    // **The box first, the pills where they sit, the lifecycle last.** Child
    // order is reading order, and that is the order down the row: the checkbox
    // in the gutter, the pills inside the text, then a note about what the row
    // *is*. The runs are published ahead of all three by `projection::build`,
    // which is the adapter's rule rather than a choice made here.
    let children: Vec<SemanticNode> = check_semantics(cx, row)
        .into_iter()
        .chain(objects)
        .chain(state_semantics(cx, row))
        .collect();
    if !children.is_empty() {
        node = node.with_children(children);
    }
    match &row.text {
        // No layout yet: discoverable, focusable and revealable, publishing no
        // text pattern rather than a rectangle and a string that do not exist.
        RowText::Absent => node,
        RowText::Runs { runs, selection } => node.with_text(TextBody {
            runs: Runs::Published(runs.clone()),
            selection: *selection,
            read_only,
        }),
        RowText::Retained { runs, selection } => node.with_text(TextBody {
            runs: Runs::Retained(runs.clone()),
            selection: *selection,
            read_only,
        }),
    }
}

/// **A diagram showing its picture**: the drawing, or the placeholder waiting
/// for one.
///
/// The first block in a document with no text to read, so the first with a
/// name of its own: the narration `koru_editor::diagram::narration_text`
/// builds — the title and description the text declares, then *Diagram,
/// flowchart, 10 boxes, 12 arrows*, then every arrow as prose; or *Diagram,
/// drawing* while the layout is in flight. **User content inside it** — the
/// labels, the title — is read as written, never translated or normalised;
/// the words around it come from `koru-i18n`.
///
/// `Role::Image` — `UIA_ImageControlTypeId` on Windows, `NSAccessibilityImageRole`
/// on macOS — carrying Focus, so a reader can land on it and Enter opens the
/// text, and ScrollIntoView like every paragraph. No text pattern and no
/// SetTextSelection: there is no text here to select, and an advertised action
/// that performs nothing is worse than one that is absent. Never `live`: a
/// drawing that lands, or changes under a peer's edit, is republished and not
/// spoken — the policy every row in this document follows. **If a screen
/// reader will not land on an image**, the one change is the role on the
/// first line below: `Role::Group`, with the same name.
fn picture(cx: &Shell, row: &RowUpdate, narration: &str) -> SemanticNode {
    let mut node = SemanticNode::new(paragraph_occurrence(cx, row.id), Role::Image)
        .with_label(narration)
        .with_action(Action::Focus)
        .with_action(Action::ScrollIntoView)
        .with_position(row.index);
    if let Some(bounds) = row.bounds {
        node = node.with_bounds(bounds);
    }
    node
}

/// **The box, if this row draws one** — and only the box.
///
/// `Role::CheckBox` carrying [`toggled`](SemanticNode::toggled), which
/// `accesskit_windows` answers as `UIA_ToggleToggleStatePropertyId` and whose
/// own `Toggle()` comes back as [`Action::Click`] — the same request the press
/// on that square makes. So a screen reader hears *checked* or *unchecked*, and
/// the gesture it offers reaches `Body::set_done` and nothing else.
///
/// The name is authored — *Done* — rather than the row's own text: the text is
/// already the paragraph's, and repeating it here would make a reader hear
/// every checklist line twice. It is deliberately **not** the word a task's
/// lifecycle uses; see [`state_semantics`].
fn check_semantics(cx: &Shell, row: &RowUpdate) -> Option<SemanticNode> {
    let ticked = row.facts.checkbox?;
    let mut node = SemanticNode::new(check_occurrence(cx, row.id), Role::CheckBox)
        .with_label(t(cx.lang, Key::A11yChecklistDone))
        .with_toggled(ticked)
        .with_action(Action::Click);
    if let Some(bounds) = row.checkbox_bounds {
        node = node.with_bounds(bounds);
    }
    Some(node)
}

/// **What this row is as a task**, when it is one at all.
///
/// A `Role::Label`, so it answers when a reader arrives on it and says nothing
/// on its own — and a label's accessible name is read out of its *value*, which
/// `projection::build` fills in from the label for every caller
/// (`guide/contributing/deps/ACCESSKIT.md`). No action: the lifecycle is changed through
/// `/completed`, the row menu and the box, each of which already has an owner.
///
/// Three sentences and no fourth, because there are three states worth telling
/// apart: open, completed, and completed while still holding unfinished
/// structural work — SPEC §5's concurrent residue, which is the whole reason
/// the drawn triangle exists.
fn state_semantics(cx: &Shell, row: &RowUpdate) -> Option<SemanticNode> {
    let task = row.facts.task?;
    let key = match (task.completed, task.unfinished_children) {
        (true, true) => Key::A11yTaskStateResidue,
        (true, false) => Key::A11yTaskStateCompleted,
        (false, _) => Key::A11yTaskStateOpen,
    };
    let mut node =
        SemanticNode::new(state_occurrence(cx, row.id), Role::Label).with_label(t(cx.lang, key));
    // The row's own rectangle: the lifecycle is a fact about the whole line and
    // has no square of its own to point at. A node with no bounds would be
    // legal and unhoverable, which is worse for an inspector than being where
    // the row is.
    if let Some(bounds) = row.bounds {
        node = node.with_bounds(bounds);
    }
    Some(node)
}

pub(crate) fn paragraph_occurrence(cx: &Shell, row: KoruNodeId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, row.as_uuid(), TEXT)
}

pub(crate) fn check_occurrence(cx: &Shell, row: KoruNodeId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, row.as_uuid(), CHECK)
}

pub(crate) fn state_occurrence(cx: &Shell, row: KoruNodeId) -> Occurrence {
    Occurrence::model(cx.window, cx.view, row.as_uuid(), STATE)
}

pub(crate) fn pill_occurrence(cx: &Shell, attr: uuid::Uuid) -> Occurrence {
    Occurrence::model(cx.window, cx.view, attr, PILL)
}

/// A `RefCell` so a `&self` semantic query may mint ids. See the module note.
pub(crate) type State = RefCell<Bodies>;

/// The open document's editor, for the two test modules beside this one.
///
/// They are the pure suite and the interactive Windows proof, and they have to
/// agree about what the shipping window is showing — so the accessor is here
/// rather than written twice.
#[cfg(test)]
pub(crate) fn editor_of(view: &MainView) -> Option<&koru_editor::Editor> {
    leaf(view).map(editor)
}

/// The paragraphs under the open document's title row.
#[cfg(test)]
pub(crate) fn body_rows_of(view: &MainView) -> Vec<KoruNodeId> {
    let Some(editor) = editor_of(view) else {
        return Vec::new();
    };
    editor
        .body
        .rows
        .iter()
        .enumerate()
        .filter(|(i, _)| !editor.body.is_title_row(*i))
        .map(|(_, row)| row.id)
        .collect()
}

/// Land the caret in one paragraph through the editor's own reveal — the same
/// call the explore column makes when a person opens a place in a document.
#[cfg(test)]
pub(crate) fn put_caret_in(view: &mut MainView, row: KoruNodeId) {
    use koru_editor::Editing;
    let Some(leaf) = leaf_mut(view) else {
        panic!("the active tab is not a document");
    };
    assert!(leaf.reveal(row), "the editor would not reveal that row");
}

#[cfg(test)]
mod tests;

#[cfg(all(test, windows))]
mod native;
