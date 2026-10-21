// SPDX-License-Identifier: MPL-2.0

//! **The shipping window publishing a real document, read back the way a
//! client reads it.**
//!
//! Every test here stands a real `MainView` up over an in-memory workspace,
//! drives the real editor with real keys, and applies the real `TreeUpdate`s
//! the real host would publish into a small **client** that keeps a tree the
//! way `accesskit_consumer` does. The assertions are then made against *that*
//! tree rather than against the values the projection just produced, because a
//! test that read the projection's own answer back would agree with any bug
//! that was consistent.
//!
//! One fake, in one place: the platform adapter. What it stands in for is
//! `WM_GETOBJECT` and `update_if_active` — a client attaching, and the rule
//! that nothing is built before one has. Everything below it is the shipping
//! code.
//!
//! Nothing here touches disk, the Store, a browser or a desktop. The one thing
//! it cannot establish — what Narrator and NVDA *say* — is the interactive
//! lane's, and is written down in `docs/features/accessibility/README.md`.

use std::sync::{Arc, Mutex};

use accesskit::{
    ActionRequest, Live, Node, NodeId, TextPosition, TextSelection, TreeId, TreeUpdate,
};
use koru_core::node::Node as CoreNode;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::geometry::Rect;
use koru_editor::Editing;
use koru_window::{AccessibilityPending as Pending, AccessibilityProvider as Provider};
use koru_workspace::Workspace;
use koru_workspace_host::View;
use koru_workspace_host::accessibility::{
    PaneAccessibility, Scope, ViewSlot, WindowSemantics, WindowSlot,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::mainview::harness::*;

const WINDOW: WindowSlot = WindowSlot::from_raw(1);
const VIEW: ViewSlot = ViewSlot::from_raw(1);

// ------------------------------------------------------------- the adapter

#[derive(Default)]
struct Mailbox {
    attached: bool,
    pending: Pending,
    published: Vec<TreeUpdate>,
}

/// The platform, as far as `PaneAccessibility` can tell.
#[derive(Clone, Default)]
struct Adapter(Arc<Mutex<Mailbox>>);

impl Adapter {
    fn attach(&self) {
        let mut mailbox = self.0.lock().expect("the mailbox");
        mailbox.attached = true;
        mailbox.pending.initial_tree = true;
    }

    fn detach(&self) {
        let mut mailbox = self.0.lock().expect("the mailbox");
        mailbox.attached = false;
        mailbox.pending.deactivated = true;
    }

    fn queue(&self, request: ActionRequest) {
        self.0
            .lock()
            .expect("the mailbox")
            .pending
            .actions
            .push(request);
    }

    fn take_published(&self) -> Vec<TreeUpdate> {
        std::mem::take(&mut self.0.lock().expect("the mailbox").published)
    }
}

impl Provider for Adapter {
    fn process_event(
        &mut self,
        _window: &winit::window::Window,
        _event: &winit::event::WindowEvent,
    ) {
    }

    fn take_pending(&mut self) -> Pending {
        std::mem::take(&mut self.0.lock().expect("the mailbox").pending)
    }

    fn publish(&mut self, build: &mut dyn FnMut() -> TreeUpdate) -> bool {
        // `update_if_active` does not call its closure before a client has
        // attached. That is the whole of "an ordinary session pays nothing".
        if !self.0.lock().expect("the mailbox").attached {
            return false;
        }
        let update = build();
        self.0.lock().expect("the mailbox").published.push(update);
        true
    }
}

// -------------------------------------------------------------- the client

/// A tree, kept the way a consumer keeps one: a node is replaced whole by an
/// update, and everything else is retained.
#[derive(Default)]
struct Client {
    nodes: std::collections::HashMap<NodeId, Node>,
    root: Option<NodeId>,
    focus: Option<NodeId>,
    /// How many nodes each update carried, in order. The delta's size.
    sizes: Vec<usize>,
}

impl Client {
    fn apply(&mut self, update: &TreeUpdate) {
        for (id, node) in &update.nodes {
            self.nodes.insert(*id, node.clone());
        }
        if let Some(tree) = &update.tree {
            self.root = Some(tree.root);
        }
        self.focus = Some(update.focus);
        self.sizes.push(update.nodes.len());
    }

    fn node(&self, id: NodeId) -> &Node {
        self.nodes.get(&id).expect("a node the client holds")
    }

    /// Every node reachable from the root, which is what the platform's own
    /// walk sees. A child named by a parent but never published would panic
    /// here — and inside `accesskit_consumer` it aborts the process.
    fn reachable(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.expect("a root")];
        while let Some(id) = stack.pop() {
            let node = self
                .nodes
                .get(&id)
                .unwrap_or_else(|| panic!("a parent names {id:?}, which was never published"));
            out.push(id);
            stack.extend(node.children().iter().copied());
        }
        out
    }

    fn by_locator(&self, locator: &str) -> Option<NodeId> {
        self.reachable()
            .into_iter()
            .find(|id| self.node(*id).author_id() == Some(locator))
    }

    /// The run nodes of one paragraph, in reading order.
    fn runs(&self, paragraph: NodeId) -> Vec<NodeId> {
        self.node(paragraph)
            .children()
            .iter()
            .copied()
            .filter(|id| self.node(*id).role() == Role::TextRun)
            .collect()
    }

    /// The paragraph's text, as `GetText` would return it: the concatenated
    /// values of its runs.
    fn text(&self, paragraph: NodeId) -> String {
        self.runs(paragraph)
            .into_iter()
            .filter_map(|id| self.node(id).value().map(str::to_owned))
            .collect()
    }

    /// A text position turned into a byte offset the way a client turns one:
    /// every run before it in reading order, plus that run's own character
    /// lengths up to the index.
    fn offset(&self, paragraph: NodeId, at: &TextPosition) -> usize {
        let mut bytes = 0;
        for run in self.runs(paragraph) {
            let node = self.node(run);
            if run == at.node {
                let lengths = node.character_lengths();
                assert!(
                    at.character_index <= lengths.len(),
                    "a published position is past the end of its run"
                );
                return bytes
                    + lengths[..at.character_index]
                        .iter()
                        .map(|n| *n as usize)
                        .sum::<usize>();
            }
            bytes += node.value().map(str::len).unwrap_or_default();
        }
        panic!("a published position names a run that is not in its paragraph");
    }

    /// The caret or selection this paragraph is publishing, in byte offsets.
    fn selection(&self, paragraph: NodeId) -> Option<(usize, usize)> {
        let selection = self.node(paragraph).text_selection()?;
        Some((
            self.offset(paragraph, &selection.anchor),
            self.offset(paragraph, &selection.focus),
        ))
    }

    /// Every node that would speak without being asked.
    fn announcing(&self) -> Vec<NodeId> {
        self.reachable()
            .into_iter()
            .filter(|id| {
                let node = self.node(*id);
                node.live().is_some_and(|live| live != Live::Off)
            })
            .collect()
    }
}

// ---------------------------------------------------------------- the shell

struct Screen {
    v: MainView,
    w: Workspace,
    pane: PaneAccessibility,
    adapter: Adapter,
    client: Client,
}

impl Screen {
    /// One pass of the loop's accessibility service, written the way
    /// `App::service_accessibility` writes it — a test that assembled the
    /// snapshot differently would be testing its own assembly.
    fn service(&mut self) -> Option<Scope> {
        if self.pane.listening() {
            let revision = self.v.semantics_revision();
            self.pane.observe(revision);
        }
        let wants = self.pane.poll();
        if !wants.snapshot {
            self.drain();
            return None;
        }
        let scope = if wants.full {
            Scope::Whole
        } else {
            Scope::Delta
        };
        let mut children = if scope == Scope::Whole {
            koru_workspace_host::accessibility::window_commands(
                WINDOW,
                false,
                koru_workspace_host::accessibility::ui_lang(),
                View::window_command_bounds(&self.v),
            )
        } else {
            Vec::new()
        };
        children.extend(View::semantics(&self.v, VIEW, scope));
        let semantics = WindowSemantics {
            window: WINDOW,
            label: "Koru".to_string(),
            scale: 1.0,
            client: Rect::new(0.0, 0.0, 1200.0, 700.0),
            focus: View::accessibility_focus(&self.v, VIEW),
            children,
            scope,
            retired: match scope {
                Scope::Whole => Vec::new(),
                Scope::Delta => View::accessibility_retired(&self.v),
            },
        };
        self.pane.publish(&semantics);
        self.drain();
        Some(scope)
    }

    fn drain(&mut self) {
        for update in self.adapter.take_published() {
            self.client.apply(&update);
        }
    }

    /// Perform whatever a client queued, the way the loop does.
    fn perform(&mut self) -> usize {
        let resolved = self.pane.resolve();
        let mut done = 0;
        for request in resolved {
            if View::accessibility_action(
                &mut self.v,
                &mut self.w.ws,
                request.occurrence,
                request.action,
                request.data.as_ref(),
            ) {
                done += 1;
            }
        }
        done
    }

    fn press(&mut self, key: Key, text: Option<&str>, mods: ModifiersState) {
        View::key(&mut self.v, &mut self.w.ws, &key, text, mods);
    }

    fn editor(&self) -> &koru_editor::Editor {
        super::editor_of(&self.v).expect("a document tab")
    }

    fn caret(&self) -> (usize, usize) {
        let editor = self.editor();
        let row = &editor.body.rows[editor.body.focus];
        (
            row.selection.anchor().index().min(row.text.len()),
            row.caret(),
        )
    }

    fn paragraph(&self, row: KoruNodeId) -> NodeId {
        let cx = Shell {
            window: WINDOW,
            view: VIEW,
            viewport: (1200.0, 700.0),
            lang: koru_workspace_host::accessibility::ui_lang(),
        };
        paragraph_occurrence(&cx, row).node_id()
    }

    fn locator(&self, row: KoruNodeId) -> String {
        let cx = Shell {
            window: WINDOW,
            view: VIEW,
            viewport: (1200.0, 700.0),
            lang: koru_workspace_host::accessibility::ui_lang(),
        };
        paragraph_occurrence(&cx, row).locator()
    }

    /// The size of the last publish, in node ids.
    fn last_publish(&self) -> usize {
        *self.client.sizes.last().expect("a publish")
    }

    /// What the editor's projection has done since the window was stood up.
    fn counters(&self) -> koru_editor::accessibility::Counters {
        self.v.document_counters()
    }
}

/// A window showing a document of `texts`, with a client attached and the first
/// whole tree published, and the caret in the first body paragraph.
fn reading(texts: &[&str]) -> Screen {
    // Built before the tab is opened, so the body is exactly these paragraphs:
    // `koru_workspace::document` leaves an empty one behind, and a fixture
    // with a paragraph nobody asked for is a fixture every count is wrong by.
    let (mut v, mut w) = container();
    let doc =
        w.ws.insert(Parent::Root, CoreNode::document("Notes").unwrap())
            .expect("the document node");
    for text in texts {
        w.ws.insert(Parent::Node(doc), CoreNode::paragraph(*text).unwrap())
            .expect("a paragraph");
    }
    w.ws.enable_undo();
    v.seat_accessibility(WINDOW);
    v.open_node(&w.ws, doc);
    v.recents.clear();
    let first = *body_rows(&v).first().expect("a body paragraph");
    put_caret_in(&mut v, first);

    let adapter = Adapter::default();
    let pane = PaneAccessibility::new(WINDOW, VIEW, Some(Box::new(adapter.clone())));
    let mut screen = Screen {
        v,
        w,
        pane,
        adapter,
        client: Client::default(),
    };
    screen.pane.revealed();
    screen.adapter.attach();
    assert_eq!(screen.service(), Some(Scope::Whole));
    // **One idle pass, because the loop makes many.** `PaneAccessibility::
    // observe` deliberately does not treat the *first* revision it is shown as
    // a change, so a fixture that published once and then typed would find
    // nothing owed. It publishes nothing, which is the other thing worth
    // knowing.
    assert_eq!(screen.service(), None, "an idle pass published something");
    screen
}

/// The paragraphs under the document's title row.
fn body_rows(v: &MainView) -> Vec<KoruNodeId> {
    super::body_rows_of(v)
}

/// Land the caret in one paragraph through the editor's own reveal.
fn put_caret_in(v: &mut MainView, row: KoruNodeId) {
    super::put_caret_in(v, row)
}

/// A peer replica of this workspace, for the CRDT import path.
fn peer(w: &Workspace) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&w.ws.export_snapshot().expect("a snapshot"))
        .expect("a peer replica")
}

/// Merge a peer's changes the way the transport does, and let the window adopt
/// them the way it does when one arrives.
fn import(screen: &mut Screen, from: &WorkspaceDoc) {
    let since = screen.w.ws.version();
    let bytes = from
        .export_updates_since(&since)
        .expect("the peer's own changes");
    screen.w.ws.import(&bytes).expect("the import");
    // The same call the shell makes when a remote update lands: every tab
    // drains its tracker, the active one catches up and reflows.
    View::adopt(&mut screen.v, &screen.w.ws);
}

// ================================================================= the tree

#[test]
fn the_shipping_window_publishes_the_document_body() {
    let screen = reading(&["The first paragraph.", "The second paragraph."]);
    let rows = body_rows(&screen.v);

    // The container is under the content region, where a reader walking the
    // chrome arrives at it.
    let body = screen
        .client
        .by_locator("koru:w1/v1/s:body#root")
        .expect("the document body is not in the tree");
    assert_eq!(screen.client.node(body).role(), Role::Group);
    let content = screen
        .client
        .by_locator("koru:w1/v1/s:shell#content")
        .expect("the content region");
    assert!(
        screen.client.node(content).children().contains(&body),
        "the body is not under the tab's region"
    );

    for row in &rows {
        let locator = screen.locator(*row);
        let id = screen
            .client
            .by_locator(&locator)
            .unwrap_or_else(|| panic!("paragraph {locator} is not in the tree"));
        assert_eq!(screen.client.node(id).role(), Role::MultilineTextInput);
    }
    assert_eq!(
        screen.client.text(screen.paragraph(rows[0])),
        "The first paragraph."
    );
    assert_eq!(
        screen.client.text(screen.paragraph(rows[1])),
        "The second paragraph."
    );
}

#[test]
fn every_child_a_parent_names_was_actually_published() {
    let screen = reading(&["One.", "Two.", "Three."]);
    // `reachable` panics on a dangling child reference. A dangling
    // `next_on_line` is worse: it aborts the process inside a UI Automation
    // callback with no backtrace. See `guide/contributing/deps/ACCESSKIT.md`.
    for id in screen.client.reachable() {
        for run in screen.client.runs(id) {
            let node = screen.client.node(run);
            for link in [node.next_on_line(), node.previous_on_line()]
                .into_iter()
                .flatten()
            {
                assert!(
                    screen.client.nodes.contains_key(&link),
                    "a run links to a run that was never published"
                );
            }
        }
    }
}

#[test]
fn the_focused_paragraph_is_where_the_window_says_the_keyboard_is() {
    let screen = reading(&["First.", "Second."]);
    let rows = body_rows(&screen.v);
    assert_eq!(
        View::accessibility_focus(&screen.v, VIEW).map(|at| at.locator()),
        Some(screen.locator(rows[0])),
        "the window's focus is not the paragraph the caret is in"
    );
    assert_eq!(screen.client.focus, Some(screen.paragraph(rows[0])));
    assert_eq!(screen.pane.counters().unresolved_focus, 0);
}

// ==================================================== movement and editing

/// Character, word, visual-line and paragraph movement, through the real keys,
/// with the published tree agreeing after each.
#[test]
fn every_kind_of_movement_agrees_with_the_published_tree() {
    let long = "This paragraph is deliberately long enough that it has to wrap onto more \
                than one visual line at the width this window uses, which is the case a \
                caret has to keep straight.";
    let mut screen = reading(&[long, "The second paragraph."]);
    let rows = body_rows(&screen.v);

    let steps: [(&str, Key, ModifiersState); 8] = [
        (
            "right",
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        ),
        (
            "right again",
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::empty(),
        ),
        (
            "word right",
            Key::Named(NamedKey::ArrowRight),
            koru_keys::PRIMARY,
        ),
        (
            "word left",
            Key::Named(NamedKey::ArrowLeft),
            koru_keys::PRIMARY,
        ),
        (
            "line down",
            Key::Named(NamedKey::ArrowDown),
            ModifiersState::empty(),
        ),
        (
            "end of line",
            Key::Named(NamedKey::End),
            ModifiersState::empty(),
        ),
        ("home", Key::Named(NamedKey::Home), ModifiersState::empty()),
        (
            "select right",
            Key::Named(NamedKey::ArrowRight),
            ModifiersState::SHIFT,
        ),
    ];
    for (what, key, mods) in steps {
        screen.press(key, None, mods);
        screen.service();
        let focused = {
            let editor = screen.editor();
            editor.body.rows[editor.body.focus].id
        };
        let published = screen
            .client
            .selection(screen.paragraph(focused))
            .unwrap_or_else(|| panic!("after {what}, the focused paragraph published no caret"));
        assert_eq!(published, screen.caret(), "after {what}");
    }

    // …and across the paragraph boundary, which is a change of focus rather
    // than a change of caret.
    for _ in 0..400 {
        screen.press(
            Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
    }
    screen.service();
    let focused = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].id
    };
    assert_eq!(
        focused, rows[1],
        "arrowing off the end of the first paragraph did not reach the second"
    );
    assert_eq!(
        screen.client.selection(screen.paragraph(rows[1])),
        Some(screen.caret())
    );
    assert!(
        screen.client.selection(screen.paragraph(rows[0])).is_none(),
        "a paragraph the caret has left is still publishing one"
    );
}

/// An assistive technology selects a range through the real host action route,
/// and real typing replaces exactly that range.
#[test]
fn an_at_selection_is_replaced_by_exactly_what_was_typed() {
    let mut screen = reading(&["The second paragraph."]);
    let row = body_rows(&screen.v)[0];
    let paragraph = screen.paragraph(row);
    let first = screen.client.runs(paragraph)[0];

    // `ITextRangeProvider::Select()` sends `SetTextSelection` at the *text
    // container*, in run ids and character indices. Exactly this.
    screen.adapter.queue(ActionRequest {
        action: Action::SetTextSelection,
        target_tree: TreeId::ROOT,
        target_node: paragraph,
        data: Some(ActionData::SetTextSelection(TextSelection {
            anchor: TextPosition {
                node: first,
                character_index: 4,
            },
            focus: TextPosition {
                node: first,
                character_index: 10,
            },
        })),
    });
    screen.service();
    assert_eq!(screen.perform(), 1, "the selection request was refused");
    screen.service();
    assert_eq!(screen.caret(), (4, 10));
    assert_eq!(screen.client.selection(paragraph), Some((4, 10)));

    screen.press(
        Key::Character("X".into()),
        Some("X"),
        ModifiersState::empty(),
    );
    screen.service();
    let text = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].text.clone()
    };
    assert_eq!(text, "The X paragraph.");
    assert_eq!(screen.client.text(paragraph), "The X paragraph.");
}

#[test]
fn a_split_a_merge_an_undo_and_a_redo_keep_the_tree_agreeing() {
    let mut screen = reading(&["Alpha beta."]);
    let row = body_rows(&screen.v)[0];
    for _ in 0..5 {
        screen.press(
            Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
    }
    screen.service();
    assert_eq!(screen.caret(), (5, 5));

    screen.press(Key::Named(NamedKey::Enter), None, ModifiersState::empty());
    screen.service();
    let after = body_rows(&screen.v);
    assert_eq!(after.len(), 2, "Enter did not split the row");
    assert_eq!(screen.client.text(screen.paragraph(after[0])), "Alpha");
    assert_eq!(screen.client.text(screen.paragraph(after[1])), " beta.");

    screen.press(
        Key::Named(NamedKey::Backspace),
        None,
        ModifiersState::empty(),
    );
    screen.service();
    assert_eq!(body_rows(&screen.v).len(), 1);
    assert_eq!(screen.client.text(screen.paragraph(row)), "Alpha beta.");

    screen.press(Key::Character("z".into()), None, koru_keys::PRIMARY);
    screen.service();
    assert_eq!(body_rows(&screen.v).len(), 2, "undo did not bring it back");

    screen.press(Key::Character("z".into()), None, koru_keys::PRIMARY_SHIFT);
    screen.service();
    assert_eq!(body_rows(&screen.v).len(), 1);
    assert_eq!(screen.client.text(screen.paragraph(row)), "Alpha beta.");

    // Whatever happened to the structure, the published caret is the editor's.
    let focused = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].id
    };
    assert_eq!(
        screen.client.selection(screen.paragraph(focused)),
        Some(screen.caret())
    );
}

// ================================================= refusal, and no mutation

#[test]
fn a_request_naming_a_paragraph_that_has_gone_is_refused() {
    let mut screen = reading(&["First.", "Second."]);
    let rows = body_rows(&screen.v);
    let doomed = screen.paragraph(rows[1]);

    put_caret_in(&mut screen.v, rows[1]);
    screen.press(Key::Named(NamedKey::Home), None, ModifiersState::empty());
    screen.press(
        Key::Named(NamedKey::Backspace),
        None,
        ModifiersState::empty(),
    );
    screen.service();
    assert_eq!(body_rows(&screen.v).len(), 1);

    let refused_before = screen.pane.counters().actions_refused;
    screen.adapter.queue(ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: doomed,
        data: None,
    });
    screen.service();
    assert_eq!(
        screen.perform(),
        0,
        "a paragraph that left the document was still routable"
    );
    assert!(
        screen.pane.counters().actions_refused > refused_before,
        "the request was dropped rather than refused"
    );
}

/// **No request a client can send changes a paragraph.** ADR-342's rule, and
/// Plan 2.1 did not move it: the checkbox a checklist row gained is a control
/// *beside* the paragraph with its own locator, and `document_action` — the
/// route every request to `#text` takes — still holds no `WorkspaceDoc` at all.
#[test]
fn no_request_a_client_can_send_changes_the_document() {
    let mut screen = reading(&["Untouched."]);
    let row = body_rows(&screen.v)[0];
    let paragraph = screen.paragraph(row);
    let before = screen.w.ws.version();
    let text = screen.client.text(paragraph);

    for action in [
        Action::SetValue,
        Action::ReplaceSelectedText,
        Action::Click,
        Action::Increment,
        Action::ShowContextMenu,
        Action::Blur,
    ] {
        screen.adapter.queue(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: paragraph,
            data: Some(ActionData::Value("never".into())),
        });
    }
    screen.service();
    assert_eq!(screen.perform(), 0);
    screen.service();
    assert_eq!(screen.w.ws.version(), before, "the document was written to");
    assert_eq!(screen.client.text(paragraph), text);
}

#[test]
fn a_read_only_document_says_so_and_refuses_a_selection() {
    let mut screen = reading(&["Cannot be edited."]);
    let row = body_rows(&screen.v)[0];
    let paragraph = screen.paragraph(row);
    let first = screen.client.runs(paragraph)[0];
    assert!(!screen.client.node(paragraph).is_read_only());

    let index = screen.v.tabs().active();
    if let Some(crate::mainview::tab::TabView::Leaf(leaf)) =
        screen.v.tabs_mut().get_mut(index).map(|tab| &mut tab.view)
    {
        leaf.view_mut().read_only = true;
    }
    screen.service();
    assert!(
        screen.client.node(paragraph).is_read_only(),
        "a read-only document did not say so"
    );

    let before = screen.caret();
    screen.adapter.queue(ActionRequest {
        action: Action::SetTextSelection,
        target_tree: TreeId::ROOT,
        target_node: paragraph,
        data: Some(ActionData::SetTextSelection(TextSelection {
            anchor: TextPosition {
                node: first,
                character_index: 0,
            },
            focus: TextPosition {
                node: first,
                character_index: 6,
            },
        })),
    });
    screen.service();
    assert_eq!(
        screen.perform(),
        0,
        "a read-only document accepted a selection request"
    );
    assert_eq!(screen.caret(), before);
}

// ================================================================ peer edits
//
// Every one of these goes through the real in-memory CRDT: a second replica
// made from this workspace's own snapshot, edited, exported and imported, and
// then adopted by the window the way the shell adopts a remote update. Nothing
// here fabricates a replacement layout.

/// A peer inserts text **before** the caret.
///
/// **This is the escalation case, and it is recorded rather than fixed.** Koru's
/// ordinary caret is a byte offset into the row's text (overlay ranges are Loro
/// cursors; ordinary carets are not — Plan 1b.1 established this). So a peer
/// inserting five characters in front of the caret leaves the caret at the same
/// *byte*, which is five characters earlier in the new text. Making it hold the
/// same **logical** position would mean turning every ordinary caret into a
/// CRDT-relative anchor, which is an editor-wide change this plan is explicitly
/// forbidden to make on its own authority.
///
/// What this plan owes is that the editor and the published tree **agree**, and
/// that is what is asserted. If the maintainers decide the caret should follow the
/// text, the assertion below changes with the editor and this test is where the
/// difference is visible.
#[test]
fn a_peer_inserting_before_the_caret_leaves_editor_and_tree_agreeing() {
    let mut screen = reading(&["one two three"]);
    let row = body_rows(&screen.v)[0];
    let paragraph = screen.paragraph(row);
    for _ in 0..8 {
        screen.press(
            Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
    }
    screen.service();
    assert_eq!(screen.caret(), (8, 8));

    let mut peer = peer(&screen.w);
    peer.insert_text(row, 0, "PEER ").expect("the peer's edit");
    import(&mut screen, &peer);
    screen.service();

    assert_eq!(screen.client.text(paragraph), "PEER one two three");
    assert_eq!(
        screen.client.selection(paragraph),
        Some(screen.caret()),
        "the editor and the tree disagree about the caret after a peer insert"
    );
    // Recorded, not asserted as desirable: the byte offset did not move.
    assert_eq!(screen.caret(), (8, 8));
    println!(
        "peer insert before the caret: text {:?}, editor {:?}, tree {:?}",
        screen.client.text(paragraph),
        screen.caret(),
        screen.client.selection(paragraph)
    );
}

/// A peer deletes text **through** a live selection.
#[test]
fn a_peer_deleting_through_a_selection_leaves_editor_and_tree_agreeing() {
    let mut screen = reading(&["alpha bravo charlie"]);
    let row = body_rows(&screen.v)[0];
    let paragraph = screen.paragraph(row);
    // Select "bravo".
    for _ in 0..6 {
        screen.press(
            Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::empty(),
        );
    }
    for _ in 0..5 {
        screen.press(
            Key::Named(NamedKey::ArrowRight),
            None,
            ModifiersState::SHIFT,
        );
    }
    screen.service();
    assert_eq!(screen.caret(), (6, 11));

    let mut peer = peer(&screen.w);
    peer.delete_text(row, 4..14).expect("the peer's deletion");
    import(&mut screen, &peer);
    screen.service();

    assert_eq!(screen.client.text(paragraph), "alpharlie");
    let published = screen
        .client
        .selection(paragraph)
        .expect("the paragraph still holds the caret");
    assert_eq!(
        published,
        screen.caret(),
        "the editor and the tree disagree after a peer deleted through the selection"
    );
    // Whatever the range became, it is inside the text that is left.
    let text = screen.client.text(paragraph);
    assert!(published.0 <= text.len() && published.1 <= text.len());
    println!(
        "peer delete through the selection: text {text:?}, editor {:?}, tree {published:?}",
        screen.caret()
    );
}

/// A peer reorders the paragraph the caret is in.
#[test]
fn a_peer_reordering_the_focused_paragraph_keeps_the_caret_on_it() {
    let mut screen = reading(&["first", "second", "third"]);
    let rows = body_rows(&screen.v);
    put_caret_in(&mut screen.v, rows[2]);
    screen.press(
        Key::Named(NamedKey::ArrowRight),
        None,
        ModifiersState::empty(),
    );
    screen.service();
    assert_eq!(screen.caret(), (1, 1));

    let mut peer = peer(&screen.w);
    let document = screen.v.document_root().expect("the document");
    peer.move_node(rows[2], Parent::Node(document), 0)
        .expect("the peer's reorder");
    import(&mut screen, &peer);
    screen.service();

    let now = body_rows(&screen.v);
    assert_eq!(now[0], rows[2], "the peer's reorder did not arrive");
    let focused = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].id
    };
    assert_eq!(focused, rows[2], "the caret did not stay on its own node");
    assert_eq!(
        screen.client.selection(screen.paragraph(rows[2])),
        Some(screen.caret())
    );
    // The container's child list moved, so the client is holding the new order.
    let body = screen
        .client
        .by_locator("koru:w1/v1/s:body#root")
        .expect("the body");
    let children: Vec<NodeId> = screen.client.node(body).children().to_vec();
    let expected: Vec<NodeId> = now.iter().map(|row| screen.paragraph(*row)).collect();
    for id in &expected {
        assert!(children.contains(id), "a surviving paragraph left the tree");
    }
    // The container publishes every row the editor has, the document's title
    // row included — it is an editable paragraph like any other — so the
    // assertion is about the *order* of the body's own rows within it.
    let positions: Vec<usize> = expected
        .iter()
        .map(|id| {
            children
                .iter()
                .position(|child| child == id)
                .expect("in the tree")
        })
        .collect();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the published child order does not match the document's"
    );
    println!(
        "peer reorder of the focused paragraph: caret stayed on its own node, editor {:?}, tree {:?}",
        screen.caret(),
        screen.client.selection(screen.paragraph(rows[2]))
    );
}

/// A peer deletes the paragraph the caret is in.
#[test]
fn a_peer_deleting_the_focused_paragraph_retires_it_and_moves_the_caret() {
    let mut screen = reading(&["first", "second", "third"]);
    let rows = body_rows(&screen.v);
    put_caret_in(&mut screen.v, rows[1]);
    screen.service();
    let doomed = screen.paragraph(rows[1]);

    let mut peer = peer(&screen.w);
    peer.delete(rows[1]).expect("the peer's deletion");
    import(&mut screen, &peer);
    screen.service();

    let now = body_rows(&screen.v);
    assert_eq!(now.len(), 2, "the peer's deletion did not arrive");
    assert!(!now.contains(&rows[1]));

    // The caret is somewhere real, and the tree says the same thing.
    let focused = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].id
    };
    assert!(
        now.contains(&focused) || focused == screen.v.document_root().expect("the document"),
        "the caret was left on a paragraph that no longer exists"
    );
    assert_eq!(
        View::accessibility_focus(&screen.v, VIEW).map(|at| at.locator()),
        Some(screen.locator(focused))
    );
    assert_eq!(screen.pane.counters().unresolved_focus, 0);

    // …and the departed paragraph is no longer routable.
    let refused_before = screen.pane.counters().actions_refused;
    screen.adapter.queue(ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: doomed,
        data: None,
    });
    screen.service();
    assert_eq!(screen.perform(), 0);
    assert!(
        screen.pane.counters().actions_refused > refused_before,
        "a paragraph a peer deleted is still routable"
    );
    println!(
        "peer delete of the focused paragraph: {} rows left, focus {:?}, refused the departed one",
        now.len(),
        View::accessibility_focus(&screen.v, VIEW).map(|at| at.locator())
    );
}

/// A peer invalidates the text anchor an open overlay is hung from.
///
/// **The accessibility half of Q-B4, now that it is fixed.** While an overlay is
/// open, `MainView::editable_target` answers `None` — a typed character goes to
/// the overlay, not to the body — and a paragraph publishes `read_only` exactly
/// when that is the answer. So an overlay left hanging from a node a peer had
/// deleted made the *whole document* describe itself to a screen reader as not
/// editable, until somebody dismissed a surface they could not see.
///
/// The overlay now closes, in a tab exactly as it always did in a standalone
/// `koru-document` window: both hosts settle through `Editing::catch_up`
/// (ADR-343-overlaycatchup). So the assertions here are the *corrected*
/// behaviour — the overlay is gone and the document goes back to reporting
/// itself editable — and what this test still owes on its own account is that
/// the published tree stays coherent through the deletion: nothing dangles, the
/// window's focus resolves, and the editor and the tree agree about the caret.
///
/// The two hosts are held together by
/// `mainview::tests::a_peer_deleting_a_pickers_anchor_closes_it_in_both_hosts`,
/// which drives both through one set of assertions. This one is about what a
/// screen reader is told afterwards.
#[test]
fn a_peer_invalidating_an_overlay_anchor_leaves_a_coherent_tree() {
    let mut screen = reading(&["first", "second"]);
    let rows = body_rows(&screen.v);
    put_caret_in(&mut screen.v, rows[1]);
    screen.press(Key::Named(NamedKey::End), None, ModifiersState::empty());
    screen.press(
        Key::Character(" ".into()),
        Some(" "),
        ModifiersState::empty(),
    );
    screen.press(
        Key::Character("/".into()),
        Some("/"),
        ModifiersState::empty(),
    );
    screen.service();
    assert!(
        !matches!(
            screen.editor().view.overlay,
            koru_editor::components::Overlay::None
        ),
        "typing / did not open a document overlay"
    );
    // The accessibility consequence of an overlay, stated: a character typed
    // now goes to the overlay, not to the body, so the body says so.
    assert!(
        screen.client.node(screen.paragraph(rows[0])).is_read_only(),
        "an open overlay did not make the body report read-only"
    );

    let mut peer = peer(&screen.w);
    peer.delete(rows[1]).expect("the peer's deletion");
    import(&mut screen, &peer);
    screen.service();

    // The paragraph the overlay was anchored to is gone from the document and
    // from the tree, and nothing dangles behind it.
    assert!(!body_rows(&screen.v).contains(&rows[1]));
    let _ = screen.client.reachable();
    assert_eq!(screen.pane.counters().unresolved_focus, 0);
    let focused = {
        let editor = screen.editor();
        editor.body.rows[editor.body.focus].id
    };
    assert_eq!(
        View::accessibility_focus(&screen.v, VIEW).map(|at| at.locator()),
        Some(screen.locator(focused)),
        "the window's focus does not resolve after the anchor was invalidated"
    );
    assert_eq!(
        screen.client.selection(screen.paragraph(focused)),
        Some(screen.caret()),
        "the editor and the tree disagree after an overlay anchor was invalidated"
    );

    // The container re-anchors now, so the overlay whose anchor died is gone —
    // and with it the reason every paragraph was publishing `read_only`. A
    // character typed now reaches the body again, and the tree says so.
    assert!(
        matches!(
            screen.editor().view.overlay,
            koru_editor::components::Overlay::None
        ),
        "the overlay is still open over a node that no longer exists — Q-B4 has \
         come back"
    );
    assert!(
        !screen.client.node(screen.paragraph(focused)).is_read_only(),
        "the stale overlay is gone but the document still describes itself as \
         read-only"
    );
    println!(
        "peer invalidating an overlay anchor: {} rows left, focus resolves, overlay closed, body editable again",
        body_rows(&screen.v).len()
    );
}

/// **Both halves of the approved peer-edit speech policy, in one test.**
///
/// No Koru-authored announcement for a remote edit — and the native change
/// notifications still published, because the policy is easy to satisfy by
/// breaking the second one.
#[test]
fn a_peer_edit_adds_no_announcement_and_still_publishes_the_change() {
    let mut screen = reading(&["one", "two"]);
    let rows = body_rows(&screen.v);
    let paragraph = screen.paragraph(rows[0]);
    assert!(
        screen.client.announcing().is_empty(),
        "something was already speaking before the peer edit"
    );
    let publishes = screen.client.sizes.len();

    let mut peer = peer(&screen.w);
    peer.insert_text(rows[0], 3, " changed elsewhere")
        .expect("the peer's edit");
    import(&mut screen, &peer);
    screen.service();

    // The second half: the client was told.
    assert!(
        screen.client.sizes.len() > publishes,
        "a peer edit published nothing at all"
    );
    assert_eq!(screen.client.text(paragraph), "one changed elsewhere");
    // Which is what raises `UIA_Text_TextChangedEventId`: the run's own value
    // changed, and the adapter diffs one update against the last.
    assert!(
        screen
            .client
            .runs(paragraph)
            .iter()
            .any(|run| screen.client.node(*run).value() == Some("one changed elsewhere")),
        "the paragraph's runs do not carry the new text"
    );

    // The first half: nothing Koru wrote speaks.
    assert!(
        screen.client.announcing().is_empty(),
        "a peer edit added a Koru-authored announcement"
    );
}

// ================================================================= the numbers

/// The numbers §7.3 asks for, on the roadmap's long fixture, **in the shipping
/// window** rather than in a harness.
#[test]
fn a_keystroke_publishes_one_paragraph_of_a_thousand() {
    let texts: Vec<String> = (0..1_000)
        .map(|i| format!("Paragraph {i} of the long fixture, with a real line of prose in it."))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let mut screen = reading(&refs);
    let whole = screen.last_publish();
    let shaped_by_the_snapshot = screen.counters().shapings;

    // A keystroke.
    screen.press(
        Key::Character("X".into()),
        Some("X"),
        ModifiersState::empty(),
    );
    assert_eq!(screen.service(), Some(Scope::Delta));
    let typed = screen.last_publish();
    let typed_shapings = screen.counters().shapings - shaped_by_the_snapshot;

    // A caret move.
    screen.press(
        Key::Named(NamedKey::ArrowRight),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(screen.service(), Some(Scope::Delta));
    let moved = screen.last_publish();
    let moved_shapings = screen.counters().shapings - shaped_by_the_snapshot - typed_shapings;

    // An idle pass.
    assert_eq!(screen.service(), None, "an idle pass published something");

    println!(
        "1,000 paragraphs in the shipping window: snapshot {whole} node(s); \
         a keystroke {typed} node(s) and {typed_shapings} shaping(s); \
         a caret move {moved} node(s) and {moved_shapings} shaping(s); \
         an idle pass 0"
    );

    assert!(
        whole > 1_000,
        "the whole snapshot did not carry the document"
    );
    // The delta is proportional to what changed, not to the document.
    assert!(
        typed < 20,
        "a keystroke published {typed} nodes out of a thousand paragraphs"
    );
    assert_eq!(
        typed_shapings, 1,
        "a keystroke shaped more than its own row"
    );
    assert!(moved < 5, "a caret move published {moved} nodes");
    assert_eq!(
        moved_shapings, 0,
        "a caret move shaped a paragraph, which it must never do"
    );
}

/// Scrolling republishes what came into view, and shapes only what the editor's
/// own reflow shaped for drawing.
#[test]
fn a_scroll_publishes_the_band_and_not_the_document() {
    let texts: Vec<String> = (0..1_000)
        .map(|i| format!("Paragraph {i} of the long fixture, with a real line of prose in it."))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let mut screen = reading(&refs);
    let before = screen.counters().shapings;

    // The wheel is resolved against wherever the pointer is, so put it in the
    // content the way a person's is.
    View::mouse_move(&mut screen.v, 600.0, 400.0);
    View::scroll_by(
        &mut screen.v,
        winit::event::MouseScrollDelta::LineDelta(0.0, -30.0),
    );
    let scope = screen.service();
    let published = screen.last_publish();
    let shaped = screen.counters().shapings - before;
    println!("a scroll in the shipping window: {published} node(s), {shaped} shaping(s)");
    assert_eq!(scope, Some(Scope::Delta));
    assert!(
        published < 1_000,
        "a scroll republished {published} nodes of a thousand paragraphs"
    );
}

// ========================================================= away, and back

#[test]
fn switching_away_from_a_document_and_back_leaves_a_coherent_tree() {
    let mut screen = reading(&["first", "second"]);
    let rows = body_rows(&screen.v);
    let paragraph = screen.paragraph(rows[0]);
    let shaped_before = screen.counters().shapings;

    // A second tab, which is a welcome screen and publishes no body.
    screen.v.open_welcome_tab();
    screen.service();
    assert!(
        screen.client.by_locator("koru:w1/v1/s:body#root").is_none()
            || screen.client.by_locator(&screen.locator(rows[0])).is_none(),
        "the document body is still in the tree after switching away from it"
    );
    // The paragraph is no longer routable.
    let refused_before = screen.pane.counters().actions_refused;
    screen.adapter.queue(ActionRequest {
        action: Action::Focus,
        target_tree: TreeId::ROOT,
        target_node: paragraph,
        data: None,
    });
    screen.service();
    assert_eq!(screen.perform(), 0);
    assert!(
        screen.pane.counters().actions_refused > refused_before,
        "a paragraph of a tab that is not showing is still routable"
    );

    // Back again.
    let document_tab = screen
        .v
        .tabs()
        .iter()
        .position(|tab| matches!(tab.view, crate::mainview::tab::TabView::Leaf(_)))
        .expect("the document tab is still open");
    assert!(screen.v.select(document_tab));
    screen.service();
    let _ = screen.client.reachable();
    assert_eq!(
        screen.client.text(screen.paragraph(rows[0])),
        "first",
        "the document did not come back whole"
    );
    assert_eq!(screen.client.text(screen.paragraph(rows[1])), "second");
    assert_eq!(screen.pane.counters().unresolved_focus, 0);
    // **The inactive tab did no work.** Coming back re-publishes the whole body
    // — the client had dropped it — but from the cache the view never threw
    // away, so parley shaped nothing at all in the round trip.
    assert_eq!(
        screen.counters().shapings,
        shaped_before,
        "switching away and back reshaped the document"
    );
}

/// A client detaching throws its tree away; the next one is owed a whole
/// window, and the document comes back without parley shaping anything.
#[test]
fn a_client_that_detaches_and_returns_is_owed_the_whole_document() {
    let mut screen = reading(&["first", "second"]);
    let rows = body_rows(&screen.v);
    let shaped = screen.counters().shapings;

    screen.adapter.detach();
    assert_eq!(screen.service(), None, "a detach published something");

    screen.adapter.attach();
    assert_eq!(
        screen.service(),
        Some(Scope::Whole),
        "a client that came back was handed a difference"
    );
    let _ = screen.client.reachable();
    assert_eq!(screen.client.text(screen.paragraph(rows[0])), "first");
    assert_eq!(screen.client.text(screen.paragraph(rows[1])), "second");
    assert_eq!(
        screen.counters().shapings,
        shaped,
        "a re-attach reshaped the document"
    );
    assert_eq!(screen.pane.counters().unresolved_focus, 0);
}

// ============================== checklists and tasks (Plan 2.1) ==============
//
// **Two facts, and the whole of these tests is keeping them apart.**
// `Paragraph::is_task` asks whether the row draws a box; `Paragraph::task` asks
// whether `koru_core::tasks::is_task` holds. `docs/features/tasks/SPEC.md` §5 lets them
// disagree, and §6 makes one of the disagreements deliberate: removing a task
// leaves its checkbox ticked, because check state is content that predates
// task-ness.

/// A window showing a document whose body rows all draw checkboxes.
fn checklist(texts: &[&str]) -> Screen {
    let (mut v, mut w) = container();
    let doc =
        w.ws.insert(Parent::Root, CoreNode::document("Notes").unwrap())
            .expect("the document node");
    for text in texts {
        w.ws.insert(Parent::Node(doc), CoreNode::task(*text).unwrap())
            .expect("a checklist item");
    }
    w.ws.enable_undo();
    v.seat_accessibility(WINDOW);
    v.open_node(&w.ws, doc);
    v.recents.clear();
    let first = *body_rows(&v).first().expect("a body row");
    put_caret_in(&mut v, first);

    let adapter = Adapter::default();
    let pane = PaneAccessibility::new(WINDOW, VIEW, Some(Box::new(adapter.clone())));
    let mut screen = Screen {
        v,
        w,
        pane,
        adapter,
        client: Client::default(),
    };
    screen.pane.revealed();
    screen.adapter.attach();
    assert_eq!(screen.service(), Some(Scope::Whole));
    assert_eq!(screen.service(), None, "an idle pass published something");
    screen
}

impl Screen {
    fn control(&self, row: KoruNodeId, part: ControlPart) -> Occurrence {
        let cx = Shell {
            window: WINDOW,
            view: VIEW,
            viewport: (1200.0, 700.0),
            lang: koru_workspace_host::accessibility::ui_lang(),
        };
        Occurrence::model(cx.window, cx.view, row.as_uuid(), part)
    }

    /// Ask for `action` on `occurrence`, the way a client does.
    fn ask(&self, occurrence: Occurrence, action: accesskit::Action) {
        self.adapter.queue(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: occurrence.node_id(),
            data: None,
        });
    }

    fn holds(&self, occurrence: Occurrence) -> bool {
        self.client
            .reachable()
            .into_iter()
            .any(|id| id == occurrence.node_id())
    }
}

/// **A checklist item is not a task.** It draws a box and says *checked*; it
/// has no lifecycle to publish, and publishing one would be a claim the model
/// does not make.
#[test]
fn a_checklist_item_publishes_a_box_and_no_lifecycle() {
    let screen = checklist(&["milk", "bread"]);
    let rows = body_rows(&screen.v);
    for row in &rows {
        assert!(
            screen.holds(screen.control(*row, CHECK)),
            "a checklist row published no checkbox"
        );
        assert!(
            !screen.holds(screen.control(*row, STATE)),
            "a row that is not a task published a task lifecycle"
        );
    }
    let box_node = screen.client.node(screen.control(rows[0], CHECK).node_id());
    assert_eq!(box_node.role(), accesskit::Role::CheckBox);
    assert_eq!(box_node.toggled(), Some(accesskit::Toggled::False));
    assert_eq!(
        box_node.label(),
        Some(koru_i18n::t(
            koru_workspace_host::accessibility::ui_lang(),
            koru_i18n::Key::A11yChecklistDone
        )),
        "the box is called *Done*, which is what it is; a task's lifecycle uses \
         a different word on purpose"
    );
}

/// **A client ticks the box, and the document changes** — through
/// `Body::set_done`, the same call one line below the press. Nothing about a
/// paragraph's *text* becomes writable: ADR-342's refusals are untouched.
#[test]
fn a_client_can_tick_a_checklist_item() {
    let mut screen = checklist(&["milk", "bread"]);
    let rows = body_rows(&screen.v);
    let target = screen.control(rows[1], CHECK);

    screen.ask(target, accesskit::Action::Click);
    screen.service();
    assert_eq!(screen.perform(), 1, "the tick was not performed");

    assert!(
        screen.w.ws.read(rows[1]).expect("the row").meta.done,
        "the document did not record the tick"
    );
    assert!(
        !screen.w.ws.read(rows[0]).expect("the row").meta.done,
        "a neighbour was ticked"
    );
    screen.service();
    let _ = screen.client.reachable();
    assert_eq!(
        screen.client.node(target.node_id()).toggled(),
        Some(accesskit::Toggled::True),
        "the client was not told"
    );
}

/// A row that draws no box has no box to tick, whatever a client asks.
#[test]
fn a_paragraph_with_no_box_refuses_a_tick() {
    let mut screen = reading(&["ordinary"]);
    let row = body_rows(&screen.v)[0];
    let target = screen.control(row, CHECK);
    assert!(
        !screen.holds(target),
        "an ordinary paragraph published a checkbox"
    );
    screen.ask(target, accesskit::Action::Click);
    screen.service();
    assert_eq!(screen.perform(), 0, "a paragraph with no box was ticked");
}

/// **The two facts, disagreeing on one row — and the model's own case for it.**
///
/// SPEC §6: *Remove from Tasks* clears the marker and the lifecycle and
/// deliberately leaves the checkbox alone, because check state is content that
/// predates task-ness. So the row goes on saying *checked* and stops having a
/// lifecycle at all — and the control that carried the lifecycle has to stop
/// being routable, which is what ADR-343-retiredsurfaces is about.
#[test]
fn a_row_that_stops_being_a_task_keeps_its_tick_and_retires_its_lifecycle() {
    let mut screen = checklist(&["write it up"]);
    let row = body_rows(&screen.v)[0];

    koru_core::tasks::ensure_task(
        &mut screen.w.ws,
        row,
        koru_core::attributes::TaskReason::Explicit,
    )
    .expect("the marker");
    koru_core::tasks::complete_task(
        &mut screen.w.ws,
        row,
        koru_core::tasks::LifecycleReason::ExplicitComplete,
        koru_editor::tasks::today(),
    )
    .expect("completed");
    View::adopt(&mut screen.v, &screen.w.ws);
    screen.service();
    let _ = screen.client.reachable();

    let state = screen.control(row, STATE);
    assert!(screen.holds(state), "a task published no lifecycle");
    assert_eq!(
        screen.client.node(state.node_id()).value(),
        Some(koru_i18n::t(
            koru_workspace_host::accessibility::ui_lang(),
            koru_i18n::Key::A11yTaskStateCompleted
        )),
        "a Role::Label's accessible name is read out of its value"
    );
    assert_eq!(
        screen
            .client
            .node(screen.control(row, CHECK).node_id())
            .toggled(),
        Some(accesskit::Toggled::True)
    );

    koru_core::tasks::remove_from_tasks(&mut screen.w.ws, row).expect("removed");
    View::adopt(&mut screen.v, &screen.w.ws);
    screen.service();
    let _ = screen.client.reachable();

    assert!(
        screen.holds(screen.control(row, CHECK)),
        "removing a task unchecked its box"
    );
    assert_eq!(
        screen
            .client
            .node(screen.control(row, CHECK).node_id())
            .toggled(),
        Some(accesskit::Toggled::True),
        "SPEC §6: stopping tracking never unchecks a checked item"
    );
    assert!(
        !screen.holds(state),
        "the lifecycle control is still in the tree"
    );

    // …and the host will not route to it either, which is the half a published
    // tree cannot show.
    let refused = screen.pane.counters().actions_refused;
    screen.ask(state, accesskit::Action::Click);
    screen.service();
    assert_eq!(screen.perform(), 0, "a retired control performed something");
    assert!(
        screen.pane.counters().actions_refused > refused,
        "the host routed a request naming a control that has left the window"
    );
}

/// A document nothing may be typed into is a document nothing may be ticked in
/// — asked of the router that already owns the question, so the answer is the
/// one a keystroke gets rather than a second opinion about it.
#[test]
fn a_read_only_document_refuses_a_tick() {
    let mut screen = checklist(&["milk"]);
    let row = body_rows(&screen.v)[0];
    let index = screen.v.tabs().active();
    if let Some(crate::mainview::tab::TabView::Leaf(leaf)) =
        screen.v.tabs_mut().get_mut(index).map(|tab| &mut tab.view)
    {
        leaf.view_mut().read_only = true;
    }
    screen.service();

    screen.ask(screen.control(row, CHECK), accesskit::Action::Click);
    screen.service();
    assert_eq!(screen.perform(), 0, "a read-only document was ticked");
    assert!(!screen.w.ws.read(row).expect("the row").meta.done);
}

#[test]
fn pane_origin_and_clip_republish_with_sidebar_and_focused_split_geometry() {
    use crate::mainview::panes::SplitDir;
    let mut screen = checklist(&["A real checkbox paragraph"]);
    fn assert_geometry(screen: &Screen) {
        let pane = screen.v.focused_view_rect();
        assert!(pane.x > 0.0 && pane.y > 0.0);
        let editor = screen.editor();
        let clip = editor.view.content_clip();
        let body = screen.client.by_locator("koru:w1/v1/s:body#root").unwrap();
        let body_bounds = screen.client.node(body).bounds().unwrap();
        assert_eq!(
            body_bounds,
            accesskit::Rect::new(
                f64::from(pane.x + clip.x),
                f64::from(pane.y + clip.y),
                f64::from(pane.x + clip.x + clip.w),
                f64::from(pane.y + clip.y + clip.h),
            )
        );
        let row = &editor.body.rows[editor.body.focus];
        let paragraph = screen.paragraph(row.id);
        let bounds = screen.client.node(paragraph).bounds().unwrap();
        let local = screen.v.to_leaf(bounds.x0 as f32, bounds.y0 as f32);
        assert!((local.0 - (editor.view.content_x + row.text_x())).abs() < 0.001);
        assert!((local.1 - (row.y - editor.view.scroll)).abs() < 0.001);
        assert!(bounds.x0 >= body_bounds.x0 && bounds.y0 >= body_bounds.y0);
        // Parley glyph bounds may overhang the row box. Compare the actual
        // pane-local projection with the retained client's window coordinates.
        let mut local_access = DocumentAccessibility::default();
        let local_body = local_access.body(
            &editor.body.rows,
            ViewGeometry {
                origin: (0.0, 0.0),
                content_x: editor.view.content_x,
                scroll: editor.view.scroll,
                viewport_h: editor.view.viewport.1,
            },
            Some(editor.body.focus),
            Publish::Whole,
            |id| screen.paragraph(id),
        );
        let local_row = local_body
            .rows
            .iter()
            .find(|local| local.id == row.id)
            .unwrap();
        let RowText::Runs { runs, .. } = &local_row.text else {
            panic!("laid out runs")
        };
        assert!(!runs.is_empty());
        for (run_id, run) in runs {
            let local = run.bounds().unwrap();
            let window = screen.client.node(*run_id).bounds().unwrap();
            assert!((window.x0 - local.x0 - f64::from(pane.x)).abs() < 0.001);
            assert!((window.y0 - local.y0 - f64::from(pane.y)).abs() < 0.001);
        }
        let check = screen
            .client
            .node(screen.control(row.id, CHECK).node_id())
            .bounds()
            .unwrap();
        assert!(check.x0 >= f64::from(pane.x) && check.x1 <= bounds.x0);
    }
    screen.v.sidebar.toggle(std::time::Instant::now());
    screen.v.sidebar.set_full(310.0);
    screen.v.relayout();
    assert_eq!(screen.service(), Some(Scope::Delta));
    assert_geometry(&screen);
    for direction in [SplitDir::Right, SplitDir::Bottom] {
        let path = screen.v.panes.focused_path().clone();
        let active = screen.v.tabs().active_tab().id;
        // The source pane remains valid when its document moves to a new pane.
        screen
            .v
            .panes
            .insert_into_group(&path, crate::mainview::tab::Tab::welcome());
        let tab = screen.v.panes.take_tab_by_id(&path, active).unwrap();
        screen.v.panes.split(&path, direction, tab);
        screen.v.relayout();
        assert_eq!(screen.service(), Some(Scope::Delta));
        assert_geometry(&screen);
    }
    assert!(screen.v.focused_view_rect().y > screen.v.content().y);
}

// ================================================================ diagrams

const DIAGRAM_TEXT: &str = "flowchart LR\n  A --> B";

/// A window showing a paragraph and a `mermaid` block of `text`, with a client
/// attached and the whole tree published, the caret in the paragraph. The
/// block has been asked for and not answered: the host's worker is not here,
/// and [`answer`] stands in for it.
fn reading_diagram(text: &str) -> (Screen, KoruNodeId) {
    let (mut v, mut w) = container();
    let doc =
        w.ws.insert(Parent::Root, CoreNode::document("Notes").unwrap())
            .expect("the document node");
    w.ws.insert(Parent::Node(doc), CoreNode::paragraph("one").unwrap())
        .expect("a paragraph");
    let node =
        w.ws.insert(Parent::Node(doc), CoreNode::code(text).unwrap())
            .expect("the block");
    w.ws.set_language(node, Some(koru_editor::diagram::LANGUAGE))
        .expect("its language");
    w.ws.enable_undo();
    v.seat_accessibility(WINDOW);
    v.open_node(&w.ws, doc);
    v.recents.clear();
    let first = *body_rows(&v).first().expect("a body paragraph");
    put_caret_in(&mut v, first);

    let adapter = Adapter::default();
    let pane = PaneAccessibility::new(WINDOW, VIEW, Some(Box::new(adapter.clone())));
    let mut screen = Screen {
        v,
        w,
        pane,
        adapter,
        client: Client::default(),
    };
    screen.pane.revealed();
    screen.adapter.attach();
    assert_eq!(screen.service(), Some(Scope::Whole));
    assert_eq!(screen.service(), None, "an idle pass published something");
    (screen, node)
}

/// The host's answer for the block's current text, landed the way the loop
/// lands it, and the frame's reflow that mirrors it onto the row.
fn answer(screen: &mut Screen, node: KoruNodeId, status: koru_editor::diagram::Status) {
    let Some(Leaf::Document(document)) = super::leaf_mut(&mut screen.v) else {
        panic!("the active tab is not a document");
    };
    let text = document
        .editor
        .body
        .row(node)
        .expect("the block")
        .text
        .clone();
    assert!(
        document
            .editor
            .diagram_result(node, koru_editor::diagram::text_hash(&text), Err(status)),
        "the answer is for the text in flight"
    );
    document.editor.reflow();
}

/// **The first block with no text and a name of its own**, read back the way
/// a client reads it: an image named *Diagram, drawing* while the host has
/// not answered, with Focus and ScrollIntoView, no runs, no value and nothing
/// to select.
#[test]
fn a_diagram_waiting_for_its_drawing_is_an_image_named_as_such_with_no_text() {
    let (screen, node) = reading_diagram(DIAGRAM_TEXT);
    let id = screen.paragraph(node);
    let image = screen.client.node(id);
    assert_eq!(image.role(), Role::Image);
    assert_eq!(image.label(), Some("Diagram, drawing"));
    assert!(image.value().is_none(), "a picture has no text value");
    assert!(screen.client.runs(id).is_empty(), "a picture has no runs");
    assert!(image.supports_action(Action::Focus));
    assert!(image.supports_action(Action::ScrollIntoView));
    assert!(
        !image.supports_action(Action::SetTextSelection),
        "there is no text in a picture to select"
    );
    assert!(screen.client.announcing().is_empty());
    // The paragraph beside it is untouched.
    let rows = body_rows(&screen.v);
    assert_eq!(screen.client.text(screen.paragraph(rows[0])), "one");
}

/// The host's answer is a change to the tree — the revision moves, the row is
/// republished — and it is never spoken: neither an answer landing nor a
/// peer's rewrite of the text is the person's own doing, and the policy for
/// everything that is not is silence.
#[test]
fn a_host_answer_renames_the_block_moves_the_revision_and_says_nothing() {
    let (mut screen, node) = reading_diagram(DIAGRAM_TEXT);
    let id = screen.paragraph(node);
    let before = screen.v.semantics_revision();
    answer(
        &mut screen,
        node,
        koru_editor::diagram::Status::NotParsed {
            line: 2,
            message: "unexpected token".into(),
            code: "merman.parse.diagram_parse".into(),
        },
    );
    assert_ne!(
        screen.v.semantics_revision(),
        before,
        "an answer landing is a change to the tree"
    );
    assert_eq!(screen.service(), Some(Scope::Delta));
    let row = screen.client.node(id);
    assert_eq!(
        row.role(),
        Role::MultilineTextInput,
        "a block that does not draw shows its text"
    );
    assert_eq!(
        row.label(),
        Some("Diagram, not drawn: Line 2: this line could not be read")
    );
    assert!(
        screen.client.text(id).starts_with("flowchart LR"),
        "the text is there to fix: {:?}",
        screen.client.text(id)
    );
    assert!(
        screen.client.announcing().is_empty(),
        "an answer landing was spoken"
    );

    // A peer rewrites the text, and the host answers for the new text.
    let mut peer = peer(&screen.w);
    peer.set_text(node, "pie\n  \"a\": 1")
        .expect("the peer's edit");
    import(&mut screen, &peer);
    assert_eq!(screen.service(), Some(Scope::Delta));
    assert!(screen.client.text(id).starts_with("pie"));
    answer(
        &mut screen,
        node,
        koru_editor::diagram::Status::KindNotDrawn {
            kind: Some("pie".into()),
        },
    );
    assert_eq!(screen.service(), Some(Scope::Delta));
    assert_eq!(
        screen.client.node(id).label(),
        Some("Diagram, not drawn: Koru does not draw this kind of diagram yet")
    );
    assert!(
        screen.client.announcing().is_empty(),
        "a remote change of a diagram was spoken"
    );
}
