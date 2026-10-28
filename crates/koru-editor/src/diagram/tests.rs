// SPDX-License-Identifier: MPL-2.0

use std::sync::Arc;

use koru_core::WorkspaceId;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_diagram::{Counts, DiagramScene, FlowchartScene, Kind as DiagramKind, SceneBody, Status};
use koru_draw::theme;
use vello::kurbo::Rect;
use winit::keyboard::{Key as InputKey, ModifiersState, NamedKey};

use super::*;
use crate::editor::{Editing, Editor};
use crate::input::action::Action;
use crate::rowlayout;
use crate::view_state::{Hover, SaveDiagramAs};

const TEXT: &str = "flowchart LR\n  A --> B\n  B --> C";

/// A document holding one paragraph, one `mermaid` code block and one
/// paragraph after it, laid out whole at a 900 px viewport. The block's row
/// is the second body row.
fn diagram_doc(text: &str) -> (WorkspaceDoc, Editor, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("D").unwrap())
        .unwrap();
    ws.insert(Parent::Node(doc), Node::paragraph("before").unwrap())
        .unwrap();
    let node = ws
        .insert(Parent::Node(doc), Node::code(text).unwrap())
        .unwrap();
    ws.set_language(node, Some(LANGUAGE)).unwrap();
    ws.insert(Parent::Node(doc), Node::paragraph("after").unwrap())
        .unwrap();
    ws.enable_undo();
    let mut e = Editor::rooted_at(&ws, doc, true);
    e.view.code_enabled = true;
    e.body.rebuild(&ws);
    e.set_viewport(900.0, 560.0);
    (ws, e, node)
}

/// A prepared drawing of a given natural size with nothing in it: the
/// editor never looks inside, only at the size.
fn prepared(width: f64, height: f64) -> Arc<PreparedDiagram> {
    Arc::new(PreparedDiagram::new(Arc::new(DiagramScene {
        kind: DiagramKind::Flowchart,
        bounds: Rect::new(0.0, 0.0, width, height),
        counts: Counts::Flowchart {
            nodes: 0,
            edges: 0,
            groups: 0,
        },
        acc_title: None,
        acc_descr: None,
        body: SceneBody::Flowchart(FlowchartScene {
            nodes: Vec::new(),
            edges: Vec::new(),
            clusters: Vec::new(),
        }),
    })))
}

fn not_parsed(line: u32) -> Status {
    Status::NotParsed {
        line,
        message: "unexpected token".into(),
        code: "merman.parse.diagram_parse".into(),
    }
}

fn row(e: &Editor, node: NodeId) -> &Paragraph {
    e.body.row(node).expect("the diagram row")
}

fn state(e: &Editor, node: NodeId) -> &DiagramState {
    &e.view.diagrams.get(&node).expect("an entry").state
}

fn focus(e: &mut Editor, node: NodeId) {
    e.body.focus_on(node);
    e.view.active = true;
}

fn press(e: &mut Editor, ws: &mut WorkspaceDoc, key: NamedKey) -> bool {
    e.key(ws, &InputKey::Named(key), None, ModifiersState::empty())
}

fn retype(ws: &mut WorkspaceDoc, e: &mut Editor, node: NodeId, text: &str) {
    ws.set_text(node, text).unwrap();
    e.body.rebuild(ws);
    e.reflow();
}

// ------------------------------------------------------------ the request seam

#[test]
fn the_text_hash_is_a_function_of_the_bytes() {
    assert_eq!(text_hash(TEXT), text_hash(TEXT));
    assert_ne!(text_hash(TEXT), text_hash("flowchart LR\n  A --> B"));
    assert_ne!(text_hash(""), text_hash(" "));
}

/// One request per distinct text, however many times the rows are laid out:
/// a second reflow asks for nothing, an edit asks once, with the new hash.
#[test]
fn a_mermaid_row_asks_once_per_distinct_text() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    let asked = e.take_diagram_requests();
    assert_eq!(asked.len(), 1, "one request for the one text");
    assert_eq!(asked[0].node, node);
    assert_eq!(asked[0].text_hash, text_hash(TEXT));
    assert_eq!(&*asked[0].text, TEXT);
    assert!(matches!(state(&e, node), DiagramState::Pending));
    assert_eq!(
        e.view.diagrams[&node].requested,
        Some(text_hash(TEXT)),
        "the hash is marked in flight"
    );

    e.reflow();
    e.reflow();
    assert!(
        e.take_diagram_requests().is_empty(),
        "laying the rows out again asks for nothing"
    );

    let edited = "flowchart LR\n  A --> B";
    retype(&mut ws, &mut e, node, edited);
    let asked = e.take_diagram_requests();
    assert_eq!(asked.len(), 1, "an edit asks once");
    assert_eq!(asked[0].text_hash, text_hash(edited));
    e.reflow();
    assert!(e.take_diagram_requests().is_empty());
}

/// A plain code block and a code block in another language ask for nothing.
#[test]
fn only_a_mermaid_code_block_is_a_diagram_row() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    ws.set_language(node, Some("rust")).unwrap();
    e.body.rebuild(&ws);
    e.view.diagram_requests.clear();
    e.view.diagrams.clear();
    e.reflow();
    assert!(e.take_diagram_requests().is_empty());
    assert!(row(&e, node).diagram.is_none());
    assert!(e.view.diagrams.is_empty());
}

/// Whitespace needs no engine to be called empty: the one answer the editor
/// gives on its own, with no request.
#[test]
fn whitespace_text_fails_as_empty_with_no_request() {
    let (mut ws, mut e, node) = diagram_doc("  \n\t\n");
    assert!(e.take_diagram_requests().is_empty(), "nothing to lay out");
    assert!(matches!(
        state(&e, node),
        DiagramState::Failed(Status::Empty)
    ));
    let shown = row(&e, node).diagram.as_ref().unwrap();
    assert!(
        shown.empty && !shown.shows_picture(),
        "the text and its hint"
    );
    assert!(
        shown.reason.is_none(),
        "the hint is the reason; no second line"
    );

    // Typing a real text asks; emptying it again answers at once.
    retype(&mut ws, &mut e, node, TEXT);
    assert_eq!(e.take_diagram_requests().len(), 1);
    retype(&mut ws, &mut e, node, "");
    assert!(e.take_diagram_requests().is_empty());
    assert!(matches!(
        state(&e, node),
        DiagramState::Failed(Status::Empty)
    ));
    assert_eq!(e.view.diagrams[&node].requested, None);
}

/// An answer for text the row no longer holds is dropped: the drawing shown
/// is always the current text's.
#[test]
fn a_stale_result_is_ignored() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    let stale = text_hash("something else");
    assert!(!e.diagram_result(node, stale, Ok(prepared(100.0, 100.0))));
    assert!(
        matches!(state(&e, node), DiagramState::Pending),
        "still waiting"
    );
    assert!(!e.diagram_result(NodeId::new(), text_hash(TEXT), Err(not_parsed(1))));
    // The right hash lands.
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(100.0, 100.0))));
    assert!(matches!(state(&e, node), DiagramState::Drawn(_)));
    assert_eq!(e.view.diagrams[&node].requested, None);
    // And lands only once.
    assert!(!e.diagram_result(node, text_hash(TEXT), Err(not_parsed(1))));
    assert!(matches!(state(&e, node), DiagramState::Drawn(_)));
}

/// A request superseded by an edit, whose text then comes back, is asked for
/// again rather than waited on for ever: the dropped answer is not coming.
#[test]
fn a_placeholder_whose_text_returns_asks_again() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    retype(&mut ws, &mut e, node, "flowchart LR\n  A --> B");
    assert_eq!(e.take_diagram_requests().len(), 1, "the edit asks");
    retype(&mut ws, &mut e, node, TEXT);
    let asked = e.take_diagram_requests();
    assert_eq!(asked.len(), 1, "the first text is asked for again");
    assert_eq!(asked[0].text_hash, text_hash(TEXT));
}

/// A drawn entry whose text changes and changes back owes nothing: the
/// drawing shown is already this text's, and the in-flight job for the other
/// text is forgotten so its answer cannot replace it.
#[test]
fn a_drawn_text_edited_and_restored_keeps_its_drawing_and_drops_the_job() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(100.0, 100.0))));
    let other = "flowchart LR\n  A --> B";
    retype(&mut ws, &mut e, node, other);
    assert_eq!(e.take_diagram_requests().len(), 1);
    retype(&mut ws, &mut e, node, TEXT);
    assert!(e.take_diagram_requests().is_empty(), "nothing owed");
    assert_eq!(e.view.diagrams[&node].requested, None);
    assert!(
        !e.diagram_result(node, text_hash(other), Err(not_parsed(1))),
        "the other text's answer is dropped"
    );
    assert!(matches!(state(&e, node), DiagramState::Drawn(_)));
}

/// Entries live in the view, so a node that leaves the body takes its entry
/// with it — and nothing else does: an edit, a resize and a theme-style
/// invalidation all keep it.
#[test]
fn entries_are_dropped_when_the_node_leaves_the_body() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(100.0, 100.0))));
    e.body.invalidate();
    e.set_viewport(700.0, 400.0);
    retype(&mut ws, &mut e, node, "flowchart LR\n  A --> B");
    assert!(
        e.view.diagrams.contains_key(&node),
        "survives edits and resizes"
    );

    ws.delete(node).unwrap();
    e.body.rebuild(&ws);
    e.reflow();
    assert!(!e.view.diagrams.contains_key(&node), "gone with the node");
}

// --------------------------------------------------------------- heights

/// While the first layout of a text is in flight the row keeps the height of
/// its code text: the placeholder promises the text's room and nothing more.
#[test]
fn a_pending_row_is_as_tall_as_its_text() {
    let (_ws, e, node) = diagram_doc(TEXT);
    let row = row(&e, node);
    assert!(
        row.diagram_shows_picture(),
        "closed and pending is a picture"
    );
    assert!(!row.dirty, "the text was shaped like any code row");
    assert_eq!(row.height, row.layout.height());
}

/// Drawn, the row is the drawing's natural height scaled to fit the column —
/// scaled down when wider, never up — and a resize is a re-scale, not a
/// request.
#[test]
fn a_drawn_row_takes_the_natural_height_scaled_to_the_column() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    let (natural_w, natural_h) = (400.0_f32, 300.0_f32);
    assert!(e.diagram_result(
        node,
        text_hash(TEXT),
        Ok(prepared(natural_w as f64, natural_h as f64))
    ));
    e.reflow();
    let column = |e: &Editor| {
        let row = row(e, node);
        e.view.viewport.0 - theme::MARGIN_X - theme::AFFORDANCE_W - row.text_x()
    };
    assert!(
        column(&e) > natural_w,
        "the fixture's column is wider than the drawing"
    );
    assert_eq!(
        row(&e, node).height,
        natural_h + rowlayout::DIAGRAM_TOP_INSET,
        "natural height, scale 1, under the inset"
    );

    // Narrow the column below the drawing's width: it scales down.
    e.set_viewport(300.0, 560.0);
    let narrow = column(&e);
    assert!(narrow < natural_w);
    let scale = narrow / natural_w;
    assert!(
        (row(&e, node).height - (natural_h * scale + rowlayout::DIAGRAM_TOP_INSET)).abs() < 0.01
    );
    assert!(
        e.take_diagram_requests().is_empty(),
        "a resize is a re-scale, never a new request"
    );

    // Widen far past it: never above natural.
    e.set_viewport(2000.0, 560.0);
    assert_eq!(
        row(&e, node).height,
        natural_h + rowlayout::DIAGRAM_TOP_INSET
    );
    assert!(e.take_diagram_requests().is_empty());
}

/// A diagram block keeps a figure's air from the rows around it — above
/// and below alike — outside the panel's own padding. It had a code block's
/// two pixels, which put a bordered picture against the descenders of the
/// paragraph above it.
#[test]
fn a_diagram_block_keeps_a_figures_air_from_its_neighbours() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.reflow();
    let index = e.body.index_of(node).unwrap();
    let (before, shown, after) = (
        &e.body.rows[index - 1],
        &e.body.rows[index],
        &e.body.rows[index + 1],
    );
    let panel = rowlayout::code_panel_rect(shown, 900.0, shown.y);
    let above = panel.y - (before.y + before.height);
    let below = after.y - (panel.y + panel.h);
    assert!(
        (above - (theme::TASK_GAP + theme::DIAGRAM_AIR)).abs() < 0.01,
        "above: {above}"
    );
    assert!((below - above).abs() < 0.01, "the same below: {below}");
    assert!(above > theme::TASK_GAP, "more than a code block's");
}

/// A result landing changes the row's height and nothing else: no row is
/// reshaped, this one included — the heights come from the entry.
#[test]
fn a_result_landing_reshapes_nothing() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    // Each row's own shaping stamp, not the process-wide counter: other
    // tests shape rows in parallel, and only this editor's rows matter.
    let stamps = |e: &Editor| e.body.rows.iter().map(|r| r.shaped).collect::<Vec<_>>();
    let before = stamps(&e);
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.reflow();
    assert_eq!(
        row(&e, node).height,
        300.0 + rowlayout::DIAGRAM_TOP_INSET,
        "the height changed"
    );
    e.reflow();
    assert_eq!(stamps(&e), before, "and no row of this editor was reshaped");
}

/// Closed and failed, the row is the text plus one reason line, like a
/// table's error line.
#[test]
fn a_failed_closed_row_is_the_text_plus_one_reason_line() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Err(not_parsed(2))));
    e.reflow();
    let row = row(&e, node);
    assert!(!row.diagram_shows_picture(), "the text shows");
    let line = theme::role::hint().metrics.line_height;
    assert_eq!(
        row.height,
        row.layout.height() + rowlayout::DIAGRAM_LINE_GAP + line
    );
}

// ------------------------------------------------------ opening the text

/// Enter on the selected drawing opens the text with the caret on line 1;
/// Escape closes it and the drawing is a picture again, still selected.
#[test]
fn enter_opens_the_text_and_esc_closes_it() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(row(&e, node).diagram_shows_picture());
    let rows_before = e.body.rows.len();

    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.diagram_text_open, Some(node), "the text is open");
    assert_eq!(e.body.rows.len(), rows_before, "no paragraph was inserted");
    let shown = row(&e, node);
    assert!(!shown.diagram_shows_picture(), "the text shows");
    assert_eq!(shown.caret(), 0, "caret on line 1");
    let state = shown.diagram.as_ref().unwrap();
    assert!(state.open && state.drawing.is_some() && !state.last_good);
    let layout = rowlayout::diagram_layout(shown, state, shown.layout.height(), 900.0);
    assert!(
        layout.drawing.is_some_and(|d| d.y > shown.layout.height()),
        "the drawing sits below the text"
    );
    assert!(layout.reason_y.is_none() && layout.label_y.is_none());

    assert!(press(&mut e, &mut ws, NamedKey::Escape));
    assert_eq!(e.view.diagram_text_open, None, "closed");
    assert!(row(&e, node).diagram_shows_picture(), "a picture again");
    assert_eq!(e.body.focused_id(), Some(node), "still selected");
    assert_eq!(row(&e, node).height, 300.0 + rowlayout::DIAGRAM_TOP_INSET);
}

/// The *Done* check at the block's corner closes the open text the way
/// Escape does — for a hand on the mouse. It is there only while the text
/// is open: a drawn block's corner has the four controls instead.
#[test]
fn the_done_check_closes_the_text_like_escape() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    let index = e.body.index_of(node).unwrap();
    let content_x = e.view.content_x;
    let centre = |e: &Editor| {
        let rect = rowlayout::diagram_done_rect(row(e, node), 900.0);
        (
            content_x + rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5 - e.view.scroll,
        )
    };
    // Closed, the corner is the controls', not Done's.
    let (x, y) = centre(&e);
    e.mouse_move(x, y);
    assert_ne!(e.view.hover, Some(Hover::DiagramDone(index)));

    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.diagram_text_open, Some(node));
    let (x, y) = centre(&e);
    e.mouse_move(x, y);
    assert_eq!(e.view.hover, Some(Hover::DiagramDone(index)));
    assert_eq!(e.view.cursor_icon(), koru_ui::cursor::PRESS);
    let done = rowlayout::diagram_done_rect(row(&e, node), 900.0);
    let copy = rowlayout::copy_chip_rect(row(&e, node), 900.0);
    assert!(done.x + done.w <= copy.x, "left of the copy icon");
    assert_eq!(done.y, copy.y, "in the chip band");

    assert!(e.mouse_down(&mut ws, x, y, false));
    assert_eq!(e.view.diagram_text_open, None, "closed");
    assert!(row(&e, node).diagram_shows_picture(), "a picture again");
    assert_eq!(e.body.focused_id(), Some(node), "still selected");
    assert_eq!(row(&e, node).height, 300.0 + rowlayout::DIAGRAM_TOP_INSET);
}

/// Ctrl+Enter leaves to a paragraph below and closes the text; so does the
/// two-trailing-blank-lines exit.
#[test]
fn ctrl_enter_leaves_to_a_paragraph_below_and_closes() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.diagram_text_open, Some(node));

    assert!(e.act(&mut ws, Action::ToggleDone));
    assert_eq!(e.view.diagram_text_open, None, "closed on leaving");
    assert_eq!(
        e.body.focused().unwrap().text,
        "after",
        "the caret moved into the following paragraph"
    );
    e.reflow();
    assert!(row(&e, node).diagram_shows_picture(), "a picture again");

    // Two trailing blank lines, Enter at the end: the same exit.
    assert!(e.open_diagram_text(node));
    retype(&mut ws, &mut e, node, &format!("{TEXT}\n\n"));
    e.place_caret(usize::MAX);
    e.reflow();
    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.diagram_text_open, None);
    assert_eq!(
        ws.read(node).unwrap().content.as_text(),
        Some(TEXT),
        "trimmed"
    );
    assert_eq!(e.body.focused().unwrap().text, "after");
}

/// A click elsewhere closes the text: whether it is open is a reading
/// position, and the reading position moved.
#[test]
fn focus_leaving_the_row_closes_the_text() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.diagram_text_open, Some(node));
    e.body.focus = 0;
    e.reflow();
    assert_eq!(e.view.diagram_text_open, None);
}

/// An edit while the text is open keeps the drawing showing until the answer
/// lands; a failing answer keeps the last good drawing, labelled, under the
/// reason line.
#[test]
fn an_edit_while_open_keeps_the_drawing_until_the_result() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(e.open_diagram_text(node));

    let broken = "flowchart LR\n  A --> B\n  B -> C";
    retype(&mut ws, &mut e, node, broken);
    assert!(
        matches!(state(&e, node), DiagramState::Drawn(_)),
        "kept while in flight"
    );
    let shown = row(&e, node).diagram.as_ref().unwrap();
    assert!(shown.drawing.is_some() && !shown.last_good && shown.reason.is_none());
    assert_eq!(e.take_diagram_requests()[0].text_hash, text_hash(broken));

    assert!(e.diagram_result(node, text_hash(broken), Err(not_parsed(3))));
    e.reflow();
    assert!(matches!(state(&e, node), DiagramState::Failed(_)));
    assert!(e.view.diagrams[&node].last_good.is_some(), "last good kept");
    let shown = row(&e, node);
    let state = shown.diagram.as_ref().unwrap();
    assert!(state.open && state.last_good, "the label shows");
    assert!(state.drawing.is_some(), "the last good drawing shows");
    assert!(matches!(
        state.reason,
        Some(Status::NotParsed { line: 3, .. })
    ));
    let layout = rowlayout::diagram_layout(shown, state, shown.layout.height(), 900.0);
    let (reason_y, label_y, drawing) = (
        layout.reason_y.unwrap(),
        layout.label_y.unwrap(),
        layout.drawing.unwrap(),
    );
    assert!(shown.layout.height() <= reason_y && reason_y < label_y && label_y < drawing.y);
    assert_eq!(shown.height, drawing.y + drawing.h);
}

/// Closing while failed shows text and reason and no drawing; reopening shows
/// the last good drawing again, for this view, this session.
#[test]
fn closing_while_failed_shows_text_and_reason_and_no_drawing() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(e.open_diagram_text(node));
    let broken = "flowchart LR\n  A -> B";
    retype(&mut ws, &mut e, node, broken);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(broken), Err(not_parsed(2))));
    e.reflow();

    assert!(press(&mut e, &mut ws, NamedKey::Escape));
    assert_eq!(e.view.diagram_text_open, None);
    let shown = row(&e, node);
    let state = shown.diagram.as_ref().unwrap();
    assert!(!state.shows_picture(), "the text shows");
    assert!(state.reason.is_some(), "with its reason");
    assert!(
        state.drawing.is_none(),
        "and no drawing — never a stale one"
    );
    assert!(
        e.view.diagrams[&node].last_good.is_some(),
        "but the entry keeps it"
    );

    assert!(e.open_diagram_text(node));
    let state = row(&e, node).diagram.as_ref().unwrap();
    assert!(
        state.drawing.is_some() && state.last_good,
        "reopening shows it again"
    );
}

/// *Edit text* on a block that does not parse puts the caret on the line the
/// status names; on a block that draws, on line 1.
#[test]
fn edit_text_lands_the_caret_on_the_named_line() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Err(not_parsed(3))));
    e.reflow();
    assert!(e.open_diagram_text(node));
    assert_eq!(e.body.focused_id(), Some(node));
    assert_eq!(
        row(&e, node).caret(),
        TEXT.find("  B --> C").unwrap(),
        "line 3 of the text"
    );
    assert_eq!(e.view.diagram_text_open, Some(node));

    assert_eq!(line_start(TEXT, 1), 0);
    assert_eq!(line_start(TEXT, 2), TEXT.find("  A --> B").unwrap());
    assert_eq!(line_start(TEXT, 0), 0, "a zero line is line 1");
    assert_eq!(
        line_start(TEXT, 99),
        TEXT.len(),
        "a line past the end is the end"
    );
    assert_eq!(line_start("", 7), 0);

    // Not a diagram row: refused.
    let before = e.body.rows[0].id;
    assert!(!e.open_diagram_text(before));
}

// ---------------------------------------------------------- the control

/// Arrowing onto a drawn diagram shows its *Edit text* control at once,
/// through the clock hover uses; arrowing off lets it go.
#[test]
fn keyboard_selection_reveals_the_edit_control() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.body.focus = 0;
    e.view.active = true;
    e.reflow();
    assert_eq!(e.view.table_controls_node, None);

    // From the paragraph above (row 0) onto the drawing (row 1).
    assert!(press(&mut e, &mut ws, NamedKey::ArrowDown));
    assert_eq!(e.body.focused_id(), Some(node), "the drawing is selected");
    assert_eq!(e.view.table_controls_node, Some(node), "its control is on");
    assert_eq!(
        e.view.table_controls.label(),
        Some(koru_i18n::t(koru_i18n::Lang::En, Key::DiagramEditText)),
        "and it is the one control, by name"
    );
    assert_eq!(
        e.view.table_controls.visibility(std::time::Instant::now()),
        1.0,
        "shown at once: the keyboard has already said what it wants"
    );

    // The control is where the painter puts it, inside the panel's corner.
    let shown = row(&e, node);
    let control = rowlayout::diagram_control_rect(shown, 900.0, Control::Edit);
    assert_eq!(control.w, crate::table::CONTROL_SIZE);
    let panel = rowlayout::code_panel_rect(shown, 900.0, shown.y);
    assert!(control.x + control.w <= panel.x + panel.w && control.y >= panel.y);

    // Enter opens the text and the control steps aside.
    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.table_controls_node, None);
}

/// The four controls — *Open*, *Copy image*, *Save as…*, *Edit text* —
/// sit in a row inside the panel's corner, left to right, the pencil at
/// the corner it always had. Each is its own hover, and each click asks
/// for its own thing: the viewer; the clipboard, through the document app,
/// with the check and the toast every copy shows; the save dialog; or the
/// text. The asks carry the block's id and are taken once.
#[test]
fn the_controls_sit_in_the_corner_and_each_asks_for_its_own_thing() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.body.focus = 0;
    e.view.active = true;
    e.reflow();
    assert!(press(&mut e, &mut ws, NamedKey::ArrowDown));
    assert_eq!(
        e.view.table_controls_node,
        Some(node),
        "the controls are on"
    );

    let shown = row(&e, node);
    let rects = Control::ALL.map(|control| rowlayout::diagram_control_rect(shown, 900.0, control));
    let panel = rowlayout::code_panel_rect(shown, 900.0, shown.y);
    for rect in &rects {
        assert_eq!(rect.w, crate::table::CONTROL_SIZE);
        assert_eq!(rect.y, rects[0].y, "one row");
        assert!(
            rect.x >= panel.x && rect.x + rect.w <= panel.x + panel.w && rect.y >= panel.y,
            "inside the panel"
        );
    }
    for pair in rects.windows(2) {
        assert!(pair[0].x + pair[0].w <= pair[1].x, "left to right, apart");
    }
    // All four sit on one bar, inside the panel's corner.
    let bar = rowlayout::diagram_controls_bar(shown, 900.0);
    for rect in &rects {
        assert!(
            rect.x >= bar.x
                && rect.x + rect.w <= bar.x + bar.w
                && rect.y >= bar.y
                && rect.y + rect.h <= bar.y + bar.h,
            "on the bar"
        );
    }
    assert!(
        bar.x > panel.x && bar.x + bar.w < panel.x + panel.w && bar.y > panel.y,
        "the bar is inside the panel, off its edges"
    );

    // The pointer in window x is the content column plus the rect.
    let content_x = e.view.content_x;
    let scroll = e.view.scroll;
    let centre = |rect: koru_draw::geometry::Rect| {
        (
            content_x + rect.x + rect.w * 0.5,
            rect.y + rect.h * 0.5 - scroll,
        )
    };
    let index = e.body.index_of(node).unwrap();
    for (control, rect) in Control::ALL.into_iter().zip(rects) {
        let (x, y) = centre(rect);
        e.mouse_move(x, y);
        assert_eq!(
            e.view.hover,
            Some(Hover::DiagramControl {
                row: index,
                control
            })
        );
        assert_eq!(e.view.cursor_icon(), koru_ui::cursor::PRESS);
    }

    // Open asks for the viewer, for this block, and opens nothing here.
    let (x, y) = centre(rects[0]);
    e.mouse_move(x, y);
    assert!(e.mouse_down(&mut ws, x, y, false));
    assert_eq!(e.view.wants_open_diagram.take(), Some(node));
    assert_eq!(e.body.focused_id(), Some(node));
    assert_eq!(
        e.view.diagram_text_open, None,
        "the text stays closed: the viewer is a picture, not an edit"
    );

    // Copy asks for the picture, confirms with the check, raises the toast.
    let (x, y) = centre(rects[1]);
    e.mouse_move(x, y);
    assert!(e.mouse_down(&mut ws, x, y, false));
    assert_eq!(e.view.wants_copy_diagram.take(), Some(node));
    assert_eq!(e.view.copy.glyph(), koru_draw::icons::CHECK);
    assert_eq!(
        e.view.wants_toast.take().as_deref(),
        Some(koru_ui::CopyAffordance::TOAST)
    );
    assert_eq!(e.view.diagram_text_open, None);

    // Save asks for the dialog on this block.
    let (x, y) = centre(rects[2]);
    e.mouse_move(x, y);
    assert!(e.mouse_down(&mut ws, x, y, false));
    assert_eq!(
        e.view.wants_save_diagram_as.take().map(|ask| ask.node),
        Some(node)
    );

    // The pencil still opens the text.
    let (x, y) = centre(rects[3]);
    e.mouse_move(x, y);
    assert!(e.mouse_down(&mut ws, x, y, false));
    assert_eq!(e.view.diagram_text_open, Some(node));
    assert_eq!(e.view.wants_open_diagram, None);
    assert_eq!(e.view.wants_copy_diagram, None);
}

/// A drawn block is a block like any other: the margin offers the `+` beside
/// the grip, so a row can be added above or below it. It had the grip alone.
#[test]
fn a_drawn_block_offers_the_plus_beside_its_grip() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.view.insert_button = true;
    e.reflow();
    let index = e.body.index_of(node).unwrap();
    let shown = row(&e, node);
    assert!(shown.diagram_shows_picture());
    let (plus, grip) = rowlayout::gutter_affordances(shown, shown.y);
    assert!(plus.x + plus.w <= grip.x, "the + is left of the grip");
    let content_x = e.view.content_x;
    let scroll = e.view.scroll;
    e.mouse_move(
        content_x + plus.x + plus.w * 0.5,
        plus.y + plus.h * 0.5 - scroll,
    );
    assert_eq!(e.view.hover, Some(Hover::Plus(index)));
    e.mouse_move(
        content_x + grip.x + grip.w * 0.5,
        grip.y + grip.h * 0.5 - scroll,
    );
    assert_eq!(e.view.hover, Some(Hover::Grip(index)));
}

/// A double click on the drawing asks for the viewer too; one on the
/// panel beside the drawing, or on a block whose text is showing, does
/// not.
#[test]
fn a_double_click_on_the_drawing_asks_for_the_viewer() {
    let (ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.view.active = true;
    e.reflow();
    let shown = row(&e, node);
    let state = shown.diagram.as_ref().expect("a diagram row");
    let drawing = rowlayout::diagram_layout(shown, state, shown.height, 900.0)
        .drawing
        .expect("a drawing");
    let content_x = e.view.content_x;
    let (x, y) = (
        content_x + drawing.x + drawing.w * 0.5,
        shown.y + drawing.y + drawing.h * 0.5,
    );
    let right = rowlayout::code_panel_rect(shown, 900.0, shown.y);
    assert!(e.mouse_double_click(&ws, x, y));
    assert_eq!(e.view.wants_open_diagram.take(), Some(node));

    // Off the drawing, on the panel's right-hand air: nothing.
    let (px, py) = (content_x + right.x + right.w - 2.0, y);
    if px > content_x + drawing.x + drawing.w {
        assert!(!e.mouse_double_click(&ws, px, py));
        assert_eq!(e.view.wants_open_diagram, None);
    }

    // Open the text: the block shows its text now, so a double click
    // selects a word there as in any code block.
    assert!(e.open_diagram_text(node));
    e.reflow();
    assert!(!row(&e, node).diagram_shows_picture());
    e.mouse_double_click(&ws, x, y);
    assert_eq!(e.view.wants_open_diagram, None);
}

// ----------------------------------------------------------------- copy

/// Copy on the selected drawing copies the whole code block with its
/// language, so it arrives as a diagram wherever it is pasted in Koru.
#[test]
fn copying_a_selected_drawing_carries_the_language() {
    let (ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    focus(&mut e, node);
    e.reflow();
    assert!(row(&e, node).diagram_shows_picture());
    crate::clipboard::clear();
    assert!(crate::clipboard::copy(&mut e, &ws), "something was copied");
    match crate::clipboard::take() {
        crate::clipboard::Taken::Rows(rows, _) => {
            assert_eq!(rows.len(), 1, "the one block");
            assert_eq!(rows[0].kind, Kind::Code);
            assert_eq!(rows[0].language.as_deref(), Some(LANGUAGE));
            assert_eq!(rows[0].to_text(&|_| String::new()), TEXT);
        }
        _ => panic!("expected the block as rows"),
    }
}

// ---------------------------------------------------------------- words

#[test]
fn the_chip_label_for_mermaid_is_localized() {
    assert_eq!(
        chip_label(Some(LANGUAGE)),
        koru_i18n::t(koru_i18n::current(), Key::DiagramChipMermaid)
    );
    assert_eq!(
        chip_label(Some("rust")),
        "rust",
        "every other token is itself"
    );
    assert_eq!(chip_label(Some("Mermaid")), "Mermaid", "the token is exact");
    assert_eq!(chip_label(None), "Auto");
}

/// Every status has its line, in Koru's words — none for a text that draws,
/// and the engine's English never reaches the page.
#[test]
fn reason_text_for_every_status() {
    let en = Lang::En;
    assert_eq!(
        reason_text(
            en,
            &Status::Draws(Counts::Flowchart {
                nodes: 1,
                edges: 0,
                groups: 0
            })
        ),
        None
    );
    assert_eq!(
        reason_text(en, &not_parsed(7)).as_deref(),
        Some("Line 7: this line could not be read")
    );
    assert!(
        !reason_text(en, &not_parsed(7))
            .unwrap()
            .contains("unexpected token"),
        "the engine's words stay off the page"
    );
    assert_eq!(
        reason_text(
            en,
            &Status::KindNotDrawn {
                kind: Some("pie".into())
            }
        )
        .as_deref(),
        Some("Koru does not draw this kind of diagram yet")
    );
    assert_eq!(
        reason_text(en, &Status::KindNotDrawn { kind: None }).as_deref(),
        Some("Koru does not draw this kind of diagram yet")
    );
    let too_large = |kind, count| Status::TooLarge { kind, count };
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Flowchart, 1)).as_deref(),
        Some("Too large to draw: 1 box")
    );
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Flowchart, 1200)).as_deref(),
        Some("Too large to draw: 1,200 boxes")
    );
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Sequence, 1)).as_deref(),
        Some("Too large to draw: 1 message")
    );
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Sequence, 600)).as_deref(),
        Some("Too large to draw: 600 messages")
    );
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Flowchart, 0)).as_deref(),
        Some("Too large to draw"),
        "uncounted: the engine refused the text before counting"
    );
    assert_eq!(
        reason_text(en, &too_large(DiagramKind::Other("gantt".into()), 3)).as_deref(),
        Some("Too large to draw")
    );
    assert_eq!(
        reason_text(en, &Status::Empty).as_deref(),
        Some("Describe a flowchart or a sequence diagram here")
    );
    // Every language answers every status with something.
    for lang in Lang::ALL {
        for status in [
            not_parsed(2),
            Status::KindNotDrawn { kind: None },
            too_large(DiagramKind::Flowchart, 900),
            too_large(DiagramKind::Sequence, 2),
            too_large(DiagramKind::Sequence, 0),
            Status::Empty,
        ] {
            assert!(
                reason_text(lang, &status).is_some_and(|text| !text.trim().is_empty()),
                "{lang:?} says nothing for {status:?}"
            );
        }
    }
}

// ------------------------------------------------------------- the narration

/// The deterministic measurer the engine's own tests lay the showcases out
/// with — half the size per character, no font — so the scene is the same on
/// every machine. The narration reads labels and counts, never a width.
pub(crate) struct Stub;

impl koru_diagram::TextMeasure for Stub {
    fn measure(&self, text: &str, px: f32) -> koru_diagram::TextMetrics {
        koru_diagram::TextMetrics {
            width: text.chars().count() as f32 * 0.5 * px,
            height: 1.2 * px,
            ascent: 0.9 * px,
        }
    }

    fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<koru_diagram::MeasuredLine> {
        let mut lines: Vec<koru_diagram::MeasuredLine> = Vec::new();
        let mut current = String::new();
        for word in text.split(' ').filter(|w| !w.is_empty()) {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if current.is_empty() || self.measure(&candidate, px).width <= max_width {
                current = candidate;
            } else {
                lines.push(koru_diagram::MeasuredLine {
                    width: self.measure(&current, px).width,
                    text: std::mem::replace(&mut current, word.to_owned()),
                });
            }
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(koru_diagram::MeasuredLine {
                width: self.measure(&current, px).width,
                text: current,
            });
        }
        lines
    }
}

/// The two showcases the engine's tests render: ten boxes, twelve arrows and
/// two groups behind a title and a description; four participants and twelve
/// messages behind the same.
pub(crate) const FLOWCHART_SHOWCASE: &str =
    include_str!("../../../koru-diagram/tests/fixtures/flowchart-showcase.mmd");
pub(crate) const SEQUENCE_SHOWCASE: &str =
    include_str!("../../../koru-diagram/tests/fixtures/sequence-showcase.mmd");

/// `text` laid out through the stub and shaped, as the host hands it back.
pub(crate) fn laid_out(text: &str) -> Arc<PreparedDiagram> {
    let measure: Arc<dyn koru_diagram::TextMeasure> = Arc::new(Stub);
    let scene = koru_diagram::layout(
        text,
        &measure,
        &koru_diagram::Limits::default(),
        &koru_diagram::Cancel::new(),
    )
    .expect("the text draws");
    Arc::new(PreparedDiagram::new(Arc::new(scene)))
}

/// A closed row showing `drawing`.
fn drawn(drawing: Arc<PreparedDiagram>) -> DiagramRowState {
    DiagramRowState {
        open: false,
        pending: false,
        empty: false,
        reason: None,
        drawing: Some(drawing),
        last_good: false,
        shown: 1,
    }
}

/// A closed row with nothing to show but `reason` and its text.
fn failed(reason: Status) -> DiagramRowState {
    let empty = matches!(reason, Status::Empty);
    DiagramRowState {
        open: false,
        pending: false,
        empty,
        reason: (!empty).then_some(reason),
        drawing: None,
        last_good: false,
        shown: 2,
    }
}

const FLOWCHART_NARRATION: &str = concat!(
    "Checkout flow. ",
    "How an order moves from the cart to a confirmed payment, with retries and a fallback provider. ",
    "Diagram, flowchart, 10 boxes, 12 arrows. ",
    "Cart, arrow to Valid order? ",
    "Valid order?, arrow \"no\", to Fix the order. ",
    "Fix the order, arrow to Cart. ",
    "Valid order?, arrow \"yes\", to Charge card. ",
    "Charge card, arrow to Payment gateway. ",
    "Payment gateway, arrow \"declined\", to Retry. ",
    "Retry, arrow to Charge card. ",
    "Payment gateway, arrow \"timeout\", to Second provider (EU). ",
    "Payment gateway, arrow \"approved\", to Order confirmed. ",
    "Second provider (EU), arrow to Order confirmed. ",
    "Order confirmed, arrow to Email & push — café ☕. ",
    "Payment service, arrow to Audit log",
);

const SEQUENCE_NARRATION: &str = concat!(
    "Sign-in with a passkey. ",
    "The app asks the server for a challenge, the device signs it, and the server issues a session. ",
    "Diagram, sequence, 4 participants: Person, Koru app, Server, Session store; 12 messages. ",
    "Person, message \"Sign in\", to Koru app. ",
    "Koru app, message \"Request challenge\", to Server. ",
    "Server, message \"Challenge\", to Koru app. ",
    "Koru app, message \"Ask for passkey\", to Person. ",
    "Person, message \"Touch sensor\", to Koru app. ",
    "Koru app, message \"Signed challenge\", to Server. ",
    "Server, message \"Create session\", to Session store. ",
    "Session store, message \"Session id\", to Server. ",
    "Server, message \"Session token\", to Koru app. ",
    "Server, message \"Rejected\", to Koru app. ",
    "Koru app, message \"Register device\", to Server. ",
    "Koru app, message \"Signed in\", to Person",
);

/// Title and description first, the counts, then the twelve arrows in source
/// order by the labels a person sees — the box ids `Validate` and `Fix` are
/// never spoken, an arrow from a group names the group's title, and a box
/// called *Valid order?* ends its sentence itself.
#[test]
fn a_drawn_flowchart_is_read_title_first_then_counts_then_every_arrow() {
    let text = narration_text(Lang::En, &drawn(laid_out(FLOWCHART_SHOWCASE)))
        .expect("a closed drawing is narrated");
    assert_eq!(text, FLOWCHART_NARRATION);
}

/// The participants by name, in the order they stand, then the twelve
/// messages in order — `P` is *Person* and `DB` is *Session store*.
#[test]
fn a_drawn_sequence_is_read_with_its_participants_by_name() {
    let text = narration_text(Lang::En, &drawn(laid_out(SEQUENCE_SHOWCASE)))
        .expect("a closed drawing is narrated");
    assert_eq!(text, SEQUENCE_NARRATION);
}

#[test]
fn a_block_that_is_not_drawn_says_so_first_and_an_open_one_says_nothing() {
    assert_eq!(
        narration_text(Lang::En, &failed(not_parsed(7))).as_deref(),
        Some("Diagram, not drawn: Line 7: this line could not be read")
    );
    assert_eq!(
        narration_text(Lang::En, &failed(Status::Empty)).as_deref(),
        Some("Diagram, not drawn: Describe a flowchart or a sequence diagram here"),
        "an emptied block names its hint"
    );
    let pending = DiagramRowState {
        open: false,
        pending: true,
        empty: false,
        reason: None,
        drawing: None,
        last_good: false,
        shown: 3,
    };
    assert_eq!(
        narration_text(Lang::En, &pending).as_deref(),
        Some("Diagram, drawing")
    );
    // Open, the text is what the person hears, whatever the state — a
    // retained drawing under it included.
    let mut open = drawn(laid_out(FLOWCHART_SHOWCASE));
    open.open = true;
    open.last_good = true;
    assert_eq!(narration_text(Lang::En, &open), None);
    let mut open = failed(not_parsed(7));
    open.open = true;
    assert_eq!(narration_text(Lang::En, &open), None);
}

/// Exactly one takes the One row; nothing and many take the Many row.
#[test]
fn the_count_fragments_agree_with_their_number() {
    assert_eq!(
        narration_text(Lang::En, &drawn(laid_out("flowchart LR\n  A --> B"))).as_deref(),
        Some("Diagram, flowchart, 2 boxes, 1 arrow. A, arrow to B")
    );
    assert_eq!(
        narration_text(Lang::En, &drawn(laid_out("flowchart LR\n  A"))).as_deref(),
        Some("Diagram, flowchart, 1 box, 0 arrows")
    );
    assert_eq!(
        narration_text(
            Lang::En,
            &drawn(laid_out("sequenceDiagram\n  participant A\n  A->>A: hi"))
        )
        .as_deref(),
        Some("Diagram, sequence, 1 participant: A; 1 message. A, message \"hi\", to A")
    );
}

/// Every language says something about every state, with the counts as
/// digits in it.
#[test]
fn every_language_narrates_every_state() {
    let flowchart = drawn(laid_out(FLOWCHART_SHOWCASE));
    let sequence = drawn(laid_out(SEQUENCE_SHOWCASE));
    let pending = DiagramRowState {
        open: false,
        pending: true,
        empty: false,
        reason: None,
        drawing: None,
        last_good: false,
        shown: 4,
    };
    for lang in Lang::ALL {
        // The counts in the reader's own numerals — Arabic-Indic for Arabic.
        let (ten, twelve, four) = (
            number::decimal(lang, 10),
            number::decimal(lang, 12),
            number::decimal(lang, 4),
        );
        let text = narration_text(lang, &flowchart).expect("a drawn flowchart");
        assert!(
            text.contains(&ten) && text.contains(&twelve) && text.contains("Checkout flow"),
            "{lang:?}: {text}"
        );
        let text = narration_text(lang, &sequence).expect("a drawn sequence");
        assert!(
            text.contains(&four) && text.contains(&twelve) && text.contains("Session store"),
            "{lang:?}: {text}"
        );
        for state in [&pending, &failed(not_parsed(7)), &failed(Status::Empty)] {
            assert!(
                narration_text(lang, state).is_some_and(|text| !text.trim().is_empty()),
                "{lang:?} says nothing for {state:?}"
            );
        }
    }
}

// ----------------------------------------------------------- the startup row

/// The note the startup table prints beside its `diagrams` row: the view's
/// entries, counted by state. Nothing about the switch itself is testable
/// here — it is read from the environment once per process.
#[test]
fn the_startup_row_counts_the_views_diagrams_by_state() {
    assert_eq!(
        startup_note(&HashMap::new()),
        "0 rows, 0 pending, 0 drawn, 0 failed"
    );
    let (_ws, mut e, node) = diagram_doc(TEXT);
    assert_eq!(
        startup_note(&e.view.diagrams),
        "1 rows, 1 pending, 0 drawn, 0 failed"
    );
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(100.0, 50.0))));
    assert_eq!(
        startup_note(&e.view.diagrams),
        "1 rows, 0 pending, 1 drawn, 0 failed"
    );
}

// ---------------------------------------------------------- save as… by key

/// The shortcut applies only where the menu offers the row: on a diagram
/// showing its drawing. Everywhere else the handler refuses and the chord
/// falls through, so it can never mean two things.
#[test]
fn the_save_chord_does_nothing_on_a_paragraph() {
    let (mut ws, mut e, _node) = diagram_doc(TEXT);
    e.body.focus = 0;
    e.view.active = true;
    assert!(!e.act(&mut ws, Action::SaveDiagramAs));
    assert_eq!(e.view.wants_save_diagram_as, None);
}

#[test]
fn the_save_chord_asks_for_a_drawn_diagram() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    let asked = e.take_diagram_requests();
    assert!(e.diagram_result(node, asked[0].text_hash, Ok(prepared(300.0, 200.0))));
    e.reflow();
    focus(&mut e, node);
    assert!(e.act(&mut ws, Action::SaveDiagramAs));
    assert_eq!(e.view.wants_save_diagram_as, Some(SaveDiagramAs { node }));
}

#[test]
fn the_save_chord_does_nothing_while_the_text_is_open_or_before_a_drawing() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    focus(&mut e, node);
    // Asked, not answered: a placeholder has no picture to save.
    assert!(!e.act(&mut ws, Action::SaveDiagramAs));
    let asked = e.take_diagram_requests();
    e.diagram_result(node, asked[0].text_hash, Ok(prepared(300.0, 200.0)));
    e.reflow();
    // Drawn, but the text is open: the row is text now, not a picture.
    assert!(e.open_diagram_text(node));
    assert!(!e.act(&mut ws, Action::SaveDiagramAs));
    assert_eq!(e.view.wants_save_diagram_as, None);
    // Closed again: the picture is back, and so is the chord.
    assert!(e.close_diagram_text());
    assert!(e.act(&mut ws, Action::SaveDiagramAs));
    assert_eq!(e.view.wants_save_diagram_as, Some(SaveDiagramAs { node }));
}

#[test]
fn the_save_chord_does_nothing_on_a_diagram_that_does_not_draw() {
    let (mut ws, mut e, node) = diagram_doc(TEXT);
    let asked = e.take_diagram_requests();
    e.diagram_result(node, asked[0].text_hash, Err(not_parsed(2)));
    e.reflow();
    focus(&mut e, node);
    assert!(!e.act(&mut ws, Action::SaveDiagramAs));
    assert_eq!(e.view.wants_save_diagram_as, None);
}

// ---------------------------------------------------- starting a diagram

/// A document of paragraphs, focused on the first, with code blocks offered.
fn prose_doc(lines: &[&str]) -> (WorkspaceDoc, Editor) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(Parent::Root, Node::document("D").unwrap())
        .unwrap();
    for line in lines {
        ws.insert(Parent::Node(doc), Node::paragraph(*line).unwrap())
            .unwrap();
    }
    ws.enable_undo();
    let mut e = Editor::rooted_at(&ws, doc, true);
    e.view.code_enabled = true;
    e.view.active = true;
    e.body.rebuild(&ws);
    e.set_viewport(900.0, 560.0);
    (ws, e)
}

fn type_in(e: &mut Editor, ws: &mut WorkspaceDoc, s: &str) {
    for c in s.chars() {
        let t = c.to_string();
        e.key(
            ws,
            &InputKey::Character(t.as_str().into()),
            Some(&t),
            ModifiersState::empty(),
        );
    }
}

fn slash_diagram(e: &mut Editor, ws: &mut WorkspaceDoc, typed: &str) {
    type_in(e, ws, typed);
    press(e, ws, NamedKey::Enter);
}

/// What a freshly started diagram must be: a `mermaid` code block holding the
/// example, its text open, the whole example selected, not a picture.
fn assert_started(e: &Editor, ws: &WorkspaceDoc, node: NodeId) {
    let read = ws.read(node).unwrap();
    let example = example(koru_i18n::current());
    assert_eq!(read.kind, Kind::Code);
    assert_eq!(read.meta.language.as_deref(), Some(LANGUAGE));
    assert_eq!(read.content.as_text(), Some(example.as_str()));
    assert_eq!(e.view.diagram_text_open, Some(node), "the text is open");
    assert_eq!(e.body.focused().map(|r| r.id), Some(node));
    assert_eq!(
        row(e, node).range(),
        0..example.len(),
        "the whole example is selected, so typing replaces it"
    );
}

/// The example draws in every language: the labels are translated and the
/// Mermaid around them is not, so no translation can stop it drawing.
#[test]
fn the_example_draws_in_every_language() {
    for lang in koru_i18n::Lang::ALL {
        let text = example(lang);
        assert_eq!(text.lines().count(), 3, "{lang:?}: three lines");
        assert_eq!(
            koru_diagram::inspect(&text, &koru_diagram::Limits::default()),
            Status::Draws(Counts::Flowchart {
                nodes: 3,
                edges: 2,
                groups: 0
            }),
            "{lang:?}: {text}"
        );
    }
}

/// `/diagram` on an empty line makes that line the diagram.
#[test]
fn slash_diagram_on_an_empty_line_starts_a_diagram_there() {
    let (mut ws, mut e) = prose_doc(&["", "after"]);
    let node = e.body.focused().unwrap().id;
    slash_diagram(&mut e, &mut ws, "/diagram");
    assert_started(&e, &ws, node);
    assert_eq!(e.body.rows.len(), 2, "no block was added");
}

/// Typing *mermaid* in the menu finds the same row.
#[test]
fn slash_mermaid_finds_the_diagram_row() {
    let (mut ws, mut e) = prose_doc(&[""]);
    let node = e.body.focused().unwrap().id;
    slash_diagram(&mut e, &mut ws, "/mermaid");
    assert_started(&e, &ws, node);
}

/// On a line with words the words stay, and the diagram is the block below.
#[test]
fn slash_diagram_on_a_line_with_words_keeps_them_and_starts_below() {
    let (mut ws, mut e) = prose_doc(&["Checkout"]);
    let line = e.body.focused().unwrap().id;
    let end = e.body.focused().unwrap().text.len();
    e.place_caret(end);
    e.reflow();
    slash_diagram(&mut e, &mut ws, " /diagram");
    let read = ws.read(line).unwrap();
    assert_eq!(read.kind, Kind::Paragraph, "the line is still prose");
    assert_eq!(read.content.as_text(), Some("Checkout "));
    assert_eq!(e.body.rows.len(), 2);
    let below = e.body.rows[1].id;
    assert_started(&e, &ws, below);
}

/// The margin `+` reaches the same command: the menu makes its new block
/// only once *Diagram* is chosen, and that block is the diagram.
#[test]
fn the_margin_plus_starts_a_diagram_in_a_new_block() {
    let (mut ws, mut e) = prose_doc(&["Checkout"]);
    let line = e.body.focused().unwrap().id;
    let trigger = crate::trigger::Trigger {
        kind: crate::trigger::TriggerKind::Slash,
        start: 0,
        node: line,
        anchor: crate::trigger::Anchor::NewBlock { above: false },
        stable: None,
    };
    e.choose_command(
        &mut ws,
        Some(trigger),
        crate::components::command_menu::Command::Diagram,
    );
    assert_eq!(ws.read(line).unwrap().content.as_text(), Some("Checkout"));
    assert_eq!(e.body.rows.len(), 2);
    let below = e.body.rows[1].id;
    assert_started(&e, &ws, below);
}

/// The example arrives selected: the first keystroke replaces it.
#[test]
fn typing_replaces_the_selected_example() {
    let (mut ws, mut e) = prose_doc(&[""]);
    let node = e.body.focused().unwrap().id;
    slash_diagram(&mut e, &mut ws, "/diagram");
    type_in(&mut e, &mut ws, "s");
    assert_eq!(ws.read(node).unwrap().content.as_text(), Some("s"));
}

/// One Ctrl+Z takes the started diagram away whole and gives back the line.
#[test]
fn one_undo_takes_a_started_diagram_away() {
    let (mut ws, mut e) = prose_doc(&[""]);
    let node = e.body.focused().unwrap().id;
    slash_diagram(&mut e, &mut ws, "/diagram");
    e.key(
        &mut ws,
        &InputKey::Character("z".into()),
        None,
        koru_keys::PRIMARY,
    );
    let read = ws.read(node).unwrap();
    assert_ne!(read.kind, Kind::Code, "no longer a code block");
    assert_eq!(read.meta.language, None);
    assert!(
        !read.content.as_text().unwrap_or("").contains("flowchart"),
        "the example is gone: {:?}",
        read.content.as_text()
    );
}

/// Where code blocks are not offered, neither is *Diagram*.
#[test]
fn the_diagram_row_is_offered_only_where_code_is() {
    use crate::components::command_menu::{Allows, Command, matching};
    let with = Allows {
        image: true,
        image_offline: false,
        divider: true,
        quote: true,
        headings: true,
        code: true,
        table: true,
        tasks: true,
        blocks: true,
        pills: true,
        attrs: true,
        task_capable: true,
        task: false,
        voice: false,
    };
    let without = Allows {
        code: false,
        ..with
    };
    assert!(matching("diag", &with).contains(&Command::Diagram));
    assert!(matching("mer", &with).contains(&Command::Diagram));
    assert!(!matching("diag", &without).contains(&Command::Diagram));
}
