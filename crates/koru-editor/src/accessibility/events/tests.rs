// SPDX-License-Identifier: MPL-2.0

//! **The rule an update has to satisfy before a screen reader may see it.**
//!
//! Two halves. The first is a real editor doing real things — an edit, a
//! split, a merge, an undo — and the assertion that every update it produces is
//! coherent and says something. The second is a set of updates assembled by
//! hand to be *wrong*, because the only way to know a check works is to fail
//! it, and none of the wrong shapes can be produced by the projection today.

use accesskit::{NodeId, TextPosition, TextSelection};
use koru_core::WorkspaceId;
use koru_core::node::Node as CoreNode;
use koru_core::schema::{Parent, WorkspaceDoc};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::accessibility::projection::{DocumentAccessibility, Publish, RowUpdate, ViewGeometry};
use crate::{Editing, Editor};

const VIEWPORT_W: f32 = 760.0;
const VIEWPORT_H: f32 = 620.0;

fn document(texts: &[&str]) -> (WorkspaceDoc, Editor) {
    let mut ws =
        WorkspaceDoc::create(WorkspaceId::new(), "events fixture").expect("an in-memory workspace");
    let doc = ws
        .insert(
            Parent::Root,
            CoreNode::document("Fixture").expect("a document"),
        )
        .expect("the document node");
    for text in texts {
        ws.insert(
            Parent::Node(doc),
            CoreNode::paragraph(*text).expect("a paragraph"),
        )
        .expect("a paragraph node");
    }
    ws.enable_undo();
    let mut editor = Editor::rooted_at(&ws, doc, true);
    editor.set_viewport(VIEWPORT_W, VIEWPORT_H);
    editor.view.active = true;
    editor.reflow();
    (ws, editor)
}

fn base_of(id: koru_core::NodeId) -> NodeId {
    let (high, low) = id.as_uuid().as_u64_pair();
    let mixed = high ^ low.rotate_left(17);
    NodeId(if mixed == 0 { 1 } else { mixed })
}

fn publish(access: &mut DocumentAccessibility, editor: &Editor, how: Publish) -> BodyUpdate {
    access.body(
        &editor.body.rows,
        ViewGeometry {
            origin: (0.0, 0.0),
            content_x: editor.view.content_x,
            scroll: editor.view.scroll,
            viewport_h: editor.view.viewport.1,
        },
        Some(editor.body.focus),
        how,
        base_of,
    )
}

fn press(editor: &mut Editor, ws: &mut WorkspaceDoc, key: Key, text: Option<&str>) {
    editor.key(ws, &key, text, ModifiersState::empty());
    editor.reflow();
}

// ---------------------------------------------- the real editor's own updates

/// Every update a real editing session produces is coherent, and every one of
/// them tells the client something.
#[test]
fn an_edit_a_split_a_merge_and_an_undo_all_publish_coherent_updates() {
    let (mut ws, mut editor) = document(&["The first paragraph.", "The second paragraph."]);
    let mut access = DocumentAccessibility::default();
    let whole = publish(&mut access, &editor, Publish::Whole);
    coherent(&whole).expect("the first snapshot");

    editor.body.rows[1].place_caret(3);
    editor.reflow();

    /// One thing a person does, as the function that does it.
    type Step = fn(&mut Editor, &mut WorkspaceDoc);

    let steps: [(&str, Step); 5] = [
        ("type a character", |e, w| {
            press(e, w, Key::Character("X".into()), Some("X"))
        }),
        ("split the paragraph", |e, w| {
            press(e, w, Key::Named(NamedKey::Enter), None)
        }),
        ("merge it back", |e, w| {
            press(e, w, Key::Named(NamedKey::Backspace), None)
        }),
        ("undo", |e, w| {
            e.key(w, &Key::Character("z".into()), None, koru_keys::PRIMARY);
            e.reflow();
        }),
        ("redo", |e, w| {
            e.key(
                w,
                &Key::Character("z".into()),
                None,
                koru_keys::PRIMARY_SHIFT,
            );
            e.reflow();
        }),
    ];

    for (what, step) in steps {
        step(&mut editor, &mut ws);
        let update = publish(&mut access, &editor, Publish::Delta);
        coherent(&update).unwrap_or_else(|why| panic!("{what} published {why:?}"));
        assert!(
            Effects::of(&update).says_anything(),
            "{what} told the client nothing at all"
        );
    }
}

/// A caret move republishes the paragraph's selection and nothing else, and
/// that is still an update the client is owed.
#[test]
fn a_caret_move_carries_a_selection_and_no_runs() {
    let (mut ws, mut editor) = document(&["A paragraph with several words in it."]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);

    press(&mut editor, &mut ws, Key::Named(NamedKey::ArrowRight), None);
    let update = publish(&mut access, &editor, Publish::Delta);

    coherent(&update).expect("a caret move");
    let effects = Effects::of(&update);
    assert_eq!(effects.selection.len(), 1, "the caret's paragraph");
    assert!(
        effects.text.is_empty(),
        "a caret move republished run nodes: {:?}",
        effects.text
    );
    assert!(!effects.reordered);
}

/// Deleting the focused paragraph removes it and its runs, and the surviving
/// selection names none of them.
#[test]
fn a_removed_paragraph_takes_its_runs_with_it_and_no_selection_names_one() {
    let (mut ws, mut editor) = document(&["First.", "Second.", "Third."]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);

    // Land at the start of the second paragraph and merge it into the first,
    // which is how a paragraph leaves a Koru document from the keyboard.
    editor.body.focus = 2;
    editor.body.rows[2].place_caret(0);
    editor.reflow();
    press(&mut editor, &mut ws, Key::Named(NamedKey::Backspace), None);

    let update = publish(&mut access, &editor, Publish::Delta);
    coherent(&update).expect("a merge");
    assert_eq!(update.removed.len(), 1, "one paragraph left the document");
    assert!(
        !update.removed_runs.is_empty(),
        "the departed paragraph's run ids were not reported"
    );
    assert!(update.order.is_some(), "the child list moved");
}

// -------------------------------------------------- the shapes that are wrong

fn one_run(id: NodeId) -> (NodeId, accesskit::Node) {
    let mut node = accesskit::Node::new(accesskit::Role::TextRun);
    node.set_value("x");
    node.set_character_lengths(vec![1u8]);
    (id, node)
}

fn row(id: koru_core::NodeId, runs: Vec<NodeId>, selection: Option<TextSelection>) -> RowUpdate {
    RowUpdate {
        id,
        index: 0,
        facts: crate::accessibility::projection::RowFacts::default(),
        bounds: None,
        checkbox_bounds: None,
        text: crate::accessibility::projection::RowText::Runs {
            runs: runs.into_iter().map(one_run).collect(),
            selection,
        },
        objects: Vec::new(),
        diagram: None,
    }
}

fn at(node: NodeId) -> TextPosition {
    TextPosition {
        node,
        character_index: 0,
    }
}

fn selecting(anchor: NodeId, focus: NodeId) -> Option<TextSelection> {
    Some(TextSelection {
        anchor: at(anchor),
        focus: at(focus),
    })
}

#[test]
fn a_selection_naming_a_run_this_update_removes_is_refused() {
    let survivor = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let gone = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let departed_run = NodeId(77);
    let update = BodyUpdate {
        rows: vec![row(
            survivor,
            vec![NodeId(1)],
            selecting(departed_run, NodeId(1)),
        )],
        removed: vec![gone],
        removed_runs: vec![departed_run],
        order: Some(vec![survivor]),
    };

    assert_eq!(
        coherent(&update),
        Err(Incoherent::SelectionNamesRemovedRow {
            row: survivor,
            run: departed_run
        })
    );
}

#[test]
fn a_selection_naming_a_run_nothing_published_is_refused() {
    let only = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let update = BodyUpdate {
        rows: vec![row(only, vec![NodeId(1)], selecting(NodeId(1), NodeId(9)))],
        ..BodyUpdate::default()
    };

    assert_eq!(
        coherent(&update),
        Err(Incoherent::SelectionNamesUnpublishedRun {
            row: only,
            run: NodeId(9)
        })
    );
}

#[test]
fn a_selection_reaching_into_another_paragraph_is_refused() {
    let first = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let second = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let update = BodyUpdate {
        rows: vec![
            row(first, vec![NodeId(1)], None),
            row(second, vec![NodeId(2)], selecting(NodeId(1), NodeId(2))),
        ],
        ..BodyUpdate::default()
    };

    assert_eq!(
        coherent(&update),
        Err(Incoherent::SelectionNamesUnpublishedRun {
            row: second,
            run: NodeId(1)
        })
    );
}

#[test]
fn one_run_id_under_two_paragraphs_is_refused() {
    let first = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let second = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let update = BodyUpdate {
        rows: vec![
            row(first, vec![NodeId(1)], None),
            row(second, vec![NodeId(1)], None),
        ],
        ..BodyUpdate::default()
    };

    assert_eq!(
        coherent(&update),
        Err(Incoherent::RunPublishedTwice {
            run: NodeId(1),
            first,
            second
        })
    );
}

#[test]
fn a_paragraph_that_is_both_published_and_removed_is_refused() {
    let both = koru_core::NodeId::from_uuid(uuid::Uuid::new_v4());
    let update = BodyUpdate {
        rows: vec![row(both, vec![NodeId(1)], None)],
        removed: vec![both],
        ..BodyUpdate::default()
    };

    assert_eq!(
        coherent(&update),
        Err(Incoherent::RowPublishedAndRemoved(both))
    );
}

#[test]
fn an_update_that_says_nothing_is_coherent_and_silent() {
    let update = BodyUpdate::default();
    coherent(&update).expect("an empty update is not incoherent");
    assert!(!Effects::of(&update).says_anything());
}
