// SPDX-License-Identifier: MPL-2.0

//! What the caret says about the commands.
//!
//! The toolbar's geometry tests went with the toolbar; the bubble and the two
//! menus have their own. What is left here is the half that outlived it: the
//! command vocabulary, and `State` reading a document at the caret.

use super::*;
use koru_core::WorkspaceId;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_editor::{Editing, Editor};

/// Every command names an action and an icon. A command with neither is one
/// nothing can press.
#[test]
fn every_command_has_an_icon_and_a_tooltip() {
    const ALL: [Command; 25] = [
        Command::Undo,
        Command::Redo,
        Command::H1,
        Command::H2,
        Command::H3,
        Command::H4,
        Command::List,
        Command::BulletedList,
        Command::NumberedList,
        Command::Tasks,
        Command::Callout,
        Command::Divider,
        Command::Bold,
        Command::Italic,
        Command::Underline,
        Command::Strike,
        Command::Code,
        Command::Highlight,
        Command::Link,
        Command::ClearFormatting,
        Command::Superscript,
        Command::Subscript,
        Command::AlignStart,
        Command::AlignCenter,
        Command::AlignEnd,
    ];
    for cmd in ALL {
        assert!(!cmd.tooltip().is_empty(), "{cmd:?} has no tooltip");
        // Phosphor lives in the Unicode private use area; anything outside it
        // is a codepoint somebody guessed.
        let g = cmd.icon() as u32;
        assert!((0xE000..=0xF8FF).contains(&g), "{cmd:?} has a non-PUA icon");
    }
}

fn document() -> (WorkspaceDoc, koru_core::NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("Spec").unwrap())
        .unwrap();
    ws.insert(Parent::Node(doc), Node::paragraph("hello there").unwrap())
        .unwrap();
    (ws, doc)
}

/// Marks under the caret come back as pressed buttons.
#[test]
fn a_mark_over_the_selection_reads_as_active() {
    let (mut ws, doc) = document();
    let mut e = Editor::rooted_at(&ws, doc, true);
    e.set_viewport(900.0, 560.0);
    e.body.focus = 0;
    e.place_selection_pair((0, 5));
    e.reflow();
    Editing::toggle_mark(&mut e, &mut ws, koru_core::Mark::Bold);
    e.reflow();

    let state = State::read(&ws, &e.body, e.body.armed());
    assert!(state.is_active(Command::Bold));
    assert!(!state.is_active(Command::Italic));
}

/// Undo and Redo are the only commands whose availability depends on history —
/// and the only ones with no button anywhere any more.
#[test]
fn undo_and_redo_are_the_only_commands_that_can_be_dead() {
    let state = State::default();
    assert!(!state.is_enabled(Command::Undo));
    assert!(!state.is_enabled(Command::Redo));
    for cmd in [
        Command::Bold,
        Command::Link,
        Command::ClearFormatting,
        Command::AlignCenter,
    ] {
        assert!(state.is_enabled(cmd), "{cmd:?} should never be dead");
    }
}
