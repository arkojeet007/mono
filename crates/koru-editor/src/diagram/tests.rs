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
    assert_eq!(row(&e, node).height, natural_h, "natural height, scale 1");

    // Narrow the column below the drawing's width: it scales down.
    e.set_viewport(300.0, 560.0);
    let narrow = column(&e);
    assert!(narrow < natural_w);
    let scale = narrow / natural_w;
    assert!((row(&e, node).height - natural_h * scale).abs() < 0.01);
    assert!(
        e.take_diagram_requests().is_empty(),
        "a resize is a re-scale, never a new request"
    );

    // Widen far past it: never above natural.
    e.set_viewport(2000.0, 560.0);
    assert_eq!(row(&e, node).height, natural_h);
    assert!(e.take_diagram_requests().is_empty());
}

/// A result landing changes the row's height and nothing else: no row is
/// reshaped, this one included — the heights come from the entry.
#[test]
fn a_result_landing_reshapes_nothing() {
    let (_ws, mut e, node) = diagram_doc(TEXT);
    e.take_diagram_requests();
    let before = rowlayout::shapings();
    assert!(e.diagram_result(node, text_hash(TEXT), Ok(prepared(400.0, 300.0))));
    e.reflow();
    assert_eq!(row(&e, node).height, 300.0, "the height changed");
    e.reflow();
    assert_eq!(rowlayout::shapings(), before, "and nothing was shaped");
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
    assert_eq!(row(&e, node).height, 300.0);
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
    let control = rowlayout::diagram_control_rect(shown, 900.0);
    assert_eq!(control.w, crate::table::CONTROL_SIZE);
    let panel = rowlayout::code_panel_rect(shown, 900.0, shown.y);
    assert!(control.x + control.w <= panel.x + panel.w && control.y >= panel.y);

    // Enter opens the text and the control steps aside.
    assert!(press(&mut e, &mut ws, NamedKey::Enter));
    assert_eq!(e.view.table_controls_node, None);
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
