// SPDX-License-Identifier: MPL-2.0

//! **The incremental story, against a real document and a real layout.**
//!
//! Every assertion here reflows for real through parley and drives the real
//! [`Editor`], because the subject is *how much work a publish costs* and a
//! stub layout costs nothing whatever the code does.
//!
//! The long fixture is the roadmap's candidate size — **1,000 paragraphs** —
//! and the numbers it produces are the ones Plan 1b.2 is judged on. They are
//! printed as well as asserted (`-- --nocapture`), because the assertion says
//! "not O(document)" and a reader wants the actual figure.

use std::sync::Arc;

use koru_core::WorkspaceId;
use koru_core::node::Node as CoreNode;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_draw::diagram::PreparedDiagram;
use winit::keyboard::{ModifiersState, NamedKey};

use super::*;
use crate::accessibility::MAX_WORD_STARTS_INDEX;
use crate::diagram::tests::{FLOWCHART_SHOWCASE, laid_out};
use crate::input::action::Action;
use crate::{Editing, Editor};

const VIEWPORT_W: f32 = 760.0;
const VIEWPORT_H: f32 = 620.0;

/// The roadmap's long fixture.
const LONG: usize = 1_000;

#[test]
fn image_and_divider_geometry_stamps_do_not_export_placeholder_text_layouts() {
    let image = CoreNode::image(
        koru_core::BlobHash("synthetic".into()),
        "image/png",
        Some(10),
        Some(10),
    )
    .unwrap();
    let mut divider = CoreNode::paragraph("").unwrap();
    divider.kind = koru_core::node::Kind::Divider;
    for node in [image, divider] {
        let mut row = Paragraph::from_node(&node, 0, Vec::new(), HashMap::new(), None, 0);
        // The exact state build_row leaves for non-text rows; no fonts, image
        // bytes, files, or store are needed to reproduce the original panic.
        row.dirty = false;
        row.shaped = 1;
        row.height = 100.0;
        let mut doc = DocumentAccessibility::default();
        let projected = doc.paragraph(&row, base_of(row.id), (0.0, 0.0), true);
        assert!(projected.runs.is_empty());
        assert!(projected.selection.is_none());
        let update = doc.body(
            &[row],
            ViewGeometry {
                origin: (0.0, 0.0),
                content_x: 0.0,
                scroll: 0.0,
                viewport_h: VIEWPORT_H,
            },
            Some(0),
            Publish::Whole,
            base_of,
        );
        assert!(matches!(update.rows[0].text, RowText::Absent));
        assert_eq!(crate::accessibility::events::coherent(&update), Ok(()));
        assert_eq!(doc.counters().shapings, 0);
    }
}

#[test]
fn navigable_delegate_pills_are_links() {
    for kind in [PillKind::Run, PillKind::Schedule, PillKind::SchedulePaused] {
        let object = InlineObject {
            attr: koru_core::AttrId::new(),
            kind,
            label: "Delegated work".into(),
            at: 0,
            bounds: None,
        };
        assert_eq!(object.role(), Role::Link);
        assert_eq!(object.value(), None);
    }
}

/// A document of `texts`, opened in a real editor at a real viewport.
fn document(texts: &[String]) -> (WorkspaceDoc, Editor) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "accessibility fixture")
        .expect("an in-memory workspace");
    let doc = ws
        .insert(
            Parent::Root,
            CoreNode::document("Fixture").expect("a document"),
        )
        .expect("the document node");
    for text in texts {
        ws.insert(
            Parent::Node(doc),
            CoreNode::paragraph(text).expect("a paragraph"),
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

fn paragraphs(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("Paragraph {i} of the long fixture, with enough words in it to be a real line of prose."))
        .collect()
}

/// Base ids derived from the row's own uuid, the way an occurrence's would be.
fn base_of(id: koru_core::NodeId) -> NodeId {
    let (high, low) = id.as_uuid().as_u64_pair();
    let mixed = high ^ low.rotate_left(17);
    NodeId(if mixed == 0 { 1 } else { mixed })
}

fn view_of(editor: &Editor) -> ViewGeometry {
    ViewGeometry {
        origin: (0.0, 0.0),
        content_x: editor.view.content_x,
        scroll: editor.view.scroll,
        viewport_h: editor.view.viewport.1,
    }
}

fn publish(
    doc: &mut DocumentAccessibility,
    editor: &Editor,
    publish: Publish,
) -> (BodyUpdate, Counters) {
    let before = doc.counters();
    let update = doc.body(
        &editor.body.rows,
        view_of(editor),
        Some(editor.body.focus),
        publish,
        base_of,
    );
    let after = doc.counters();
    (
        update,
        Counters {
            paragraphs: after.paragraphs - before.paragraphs,
            runs: after.runs - before.runs,
            stale_requests: after.stale_requests - before.stale_requests,
            shapings: after.shapings - before.shapings,
            reused: after.reused - before.reused,
            translated: after.translated - before.translated,
            published: after.published - before.published,
            skipped: after.skipped - before.skipped,
            requests_performed: after.requests_performed - before.requests_performed,
            requests_refused: after.requests_refused - before.requests_refused,
        },
    )
}

fn with_text(update: &BodyUpdate) -> usize {
    update
        .rows
        .iter()
        .filter(|row| matches!(row.text, RowText::Runs { .. }))
        .count()
}

fn stubs(update: &BodyUpdate) -> usize {
    update
        .rows
        .iter()
        .filter(|row| matches!(row.text, RowText::Absent))
        .count()
}

// ------------------------------------------------- the numbers that decide

/// **The whole plan, as five numbers.**
///
/// Activation may be O(the document). Nothing after it may be. Printed as well
/// as asserted, because "not O(document)" is the assertion and the figure is
/// what a reader came for.
#[test]
fn a_thousand_paragraphs_cost_the_document_once_and_never_again() {
    let (mut ws, mut editor) = document(&paragraphs(LONG));
    let mut access = DocumentAccessibility::default();
    assert_eq!(editor.body.rows.len(), LONG);

    // ---- activation
    let (first, activation) = publish(&mut access, &editor, Publish::Whole);
    assert_eq!(first.rows.len(), LONG, "every paragraph is discoverable");
    assert_eq!(first.order.as_ref().map(Vec::len), Some(LONG));
    let banded = with_text(&first);
    assert!(
        banded < LONG / 4,
        "the whole document was laid out to publish it: {banded} of {LONG} rows carry text"
    );
    assert_eq!(stubs(&first), LONG - banded);
    assert_eq!(activation.shapings, banded as u64);

    // ---- one keystroke
    editor.body.focus = 0;
    editor.body.rows[0].place_caret(0);
    editor.reflow();
    let _ = publish(&mut access, &editor, Publish::Delta);
    editor.insert_text(&mut ws, "Z");
    editor.reflow();
    let (typed, keystroke) = publish(&mut access, &editor, Publish::Delta);

    // ---- a caret move
    assert!(editor.act(
        &mut ws,
        Action::MoveRight {
            by_word: false,
            extend: false,
        }
    ));
    editor.reflow();
    let (moved, caret) = publish(&mut access, &editor, Publish::Delta);

    // ---- an idle frame: nothing at all happened
    let (idle, nothing) = publish(&mut access, &editor, Publish::Delta);

    // ---- a scroll of one viewport
    editor.view.scroll = VIEWPORT_H;
    editor.reflow();
    let (scrolled, scroll) = publish(&mut access, &editor, Publish::Delta);

    println!(
        "1,000 paragraphs — activation: {} rows published ({} with text), {} shaped\n\
         keystroke:  {} rows published, {} shaped\n\
         caret move: {} rows published, {} shaped\n\
         idle frame: {} rows published, {} shaped\n\
         scroll:     {} rows published, {} shaped, {} moved",
        first.rows.len(),
        banded,
        activation.shapings,
        typed.rows.len(),
        keystroke.shapings,
        moved.rows.len(),
        caret.shapings,
        idle.rows.len(),
        nothing.shapings,
        scrolled.rows.len(),
        scroll.shapings,
        scroll.translated,
    );

    // **A keystroke touches the paragraph it was typed in.**
    assert_eq!(
        typed.rows.len(),
        1,
        "a keystroke republished more than the paragraph it changed"
    );
    assert_eq!(keystroke.shapings, 1);
    assert!(
        typed.order.is_none(),
        "an edit inside a paragraph is not a reorder"
    );

    // **A caret move shapes nothing and copies no text.** The paragraph is
    // republished — its `text_selection` changed — but its runs are named, not
    // rebuilt.
    assert_eq!(caret.shapings, 0, "a caret move reshaped a paragraph");
    assert!(
        moved.rows.len() <= 2,
        "a caret move touched {} rows",
        moved.rows.len()
    );
    assert!(
        moved
            .rows
            .iter()
            .all(|row| matches!(row.text, RowText::Retained { .. })),
        "a caret move republished text runs"
    );

    // **An idle frame is nothing at all.**
    assert!(
        idle.is_empty(),
        "an idle frame published {} rows",
        idle.rows.len()
    );
    assert_eq!(nothing.shapings, 0);
    assert_eq!(nothing.published, 0);
    assert_eq!(nothing.skipped, LONG as u64);

    // **A scroll is proportional to the viewport, not to the document.**
    assert!(
        scrolled.rows.len() < LONG / 4,
        "a scroll republished {} of {LONG} rows",
        scrolled.rows.len()
    );
    assert!(scroll.shapings <= scrolled.rows.len() as u64);
}

/// The counter that proves the *editor* was not asked to lay anything out.
///
/// Every row's shaping stamp is read before and after, and a stamp only ever
/// changes inside `rowlayout::build_row` — so a projection that reflowed would
/// be visible here whatever it claimed. The type system already forbids it
/// (`body` takes `&[Paragraph]`); this is the runtime statement of the same
/// thing, which is what survives a later refactor that widens a signature.
///
/// Deliberately *per editor* rather than reading the process-wide
/// [`rowlayout::shapings`](crate::rowlayout::shapings) counter: the suite runs
/// on many threads and every other test in it shapes rows of its own.
#[test]
fn projecting_never_lays_anything_out() {
    let (_ws, editor) = document(&paragraphs(200));
    let mut access = DocumentAccessibility::default();
    let stamps: Vec<(bool, u64)> = editor
        .body
        .rows
        .iter()
        .map(|row| (row.dirty, row.shaped_at()))
        .collect();
    for _ in 0..25 {
        publish(&mut access, &editor, Publish::Whole);
        publish(&mut access, &editor, Publish::Delta);
    }
    let after: Vec<(bool, u64)> = editor
        .body
        .rows
        .iter()
        .map(|row| (row.dirty, row.shaped_at()))
        .collect();
    assert_eq!(stamps, after, "a semantic query shaped a row");
}

// --------------------------------------------------- off screen and back

/// **Offscreen paragraphs remain discoverable**, and a reveal is the view's
/// existing scroll rather than a document walk.
#[test]
fn an_offscreen_paragraph_is_discoverable_and_reveals_its_text_when_scrolled_to() {
    let (_ws, mut editor) = document(&paragraphs(400));
    let mut access = DocumentAccessibility::default();
    let (first, _) = publish(&mut access, &editor, Publish::Whole);

    let far = editor.body.rows[380].id;
    let stub = first
        .rows
        .iter()
        .find(|row| row.id == far)
        .expect("every paragraph is in the tree");
    assert!(
        matches!(stub.text, RowText::Absent),
        "a paragraph 380 rows down was laid out to publish it"
    );
    assert_eq!(
        stub.bounds, None,
        "a row with no layout published a rectangle"
    );

    // The reveal: the same scroll a focus request performs. Nothing here walks
    // the document; `reflow` shapes the band it lands in and no more.
    editor.view.scroll = editor.body.rows[380].y - VIEWPORT_H / 2.0;
    editor.reflow();
    let (after, _) = publish(&mut access, &editor, Publish::Delta);
    let revealed = after
        .rows
        .iter()
        .find(|row| row.id == far)
        .expect("the revealed row was republished");
    match &revealed.text {
        RowText::Runs { runs, .. } => assert!(!runs.is_empty()),
        other => panic!("a revealed paragraph published no text: {other:?}"),
    }
    assert!(
        revealed.bounds.is_some(),
        "a revealed paragraph has no rectangle"
    );
}

/// A shaped row that scrolls far away keeps its text and loses its rectangle.
/// That is what stops a scroll from republishing every paragraph the session
/// has ever visited.
#[test]
fn a_row_that_scrolls_away_keeps_its_text_and_drops_its_geometry() {
    let (_ws, mut editor) = document(&paragraphs(400));
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);
    let first = editor.body.rows[0].id;

    editor.view.scroll = 6_000.0;
    editor.reflow();
    let (after, _) = publish(&mut access, &editor, Publish::Delta);
    let row = after
        .rows
        .iter()
        .find(|row| row.id == first)
        .expect("the row that left the viewport was republished once");
    assert_eq!(row.bounds, None);
    match &row.text {
        RowText::Runs { runs, .. } => {
            assert!(
                !runs.is_empty(),
                "the text was dropped along with the geometry"
            );
            assert!(
                runs.iter().all(|(_, node)| node.bounds().is_none()),
                "an off-screen run kept a rectangle and will move on every scroll"
            );
        }
        other => panic!("an off-screen paragraph stopped being readable: {other:?}"),
    }

    // And it stays quiet from then on: the second scroll does not touch it.
    editor.view.scroll = 6_400.0;
    editor.reflow();
    let (again, _) = publish(&mut access, &editor, Publish::Delta);
    assert!(
        !again.rows.iter().any(|row| row.id == first),
        "an off-screen paragraph was republished by a scroll"
    );
}

// ----------------------------------------------------- structure changes

/// Splitting a paragraph is an insert: the order changes, the new row is
/// published, and the rows that did not change are left alone.
#[test]
fn a_split_reports_a_new_order_and_republishes_only_what_moved() {
    let (mut ws, mut editor) = document(&["one two".to_string(), "three".to_string()]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);

    editor.body.focus = 0;
    editor.body.rows[0].place_caret(3);
    editor.reflow();
    let _ = publish(&mut access, &editor, Publish::Delta);
    let survivor = editor.body.rows[0].id;
    // The run ids as the client holds them *before* anything happens. A split
    // and its undo have to give the same paragraph back, ids included.
    let ids_before = access.published_run_ids(survivor);
    assert!(editor.act(&mut ws, Action::SplitLine));
    editor.reflow();

    let (split, _) = publish(&mut access, &editor, Publish::Delta);
    assert_eq!(editor.body.rows.len(), 3);
    assert_eq!(
        split.order.as_ref().map(Vec::len),
        Some(3),
        "a split did not report a new child order"
    );
    assert!(split.removed.is_empty());

    // ...and the undo puts the paragraph back, ids included.
    assert!(editor.act(&mut ws, Action::Undo));
    editor.reflow();
    let (undone, _) = publish(&mut access, &editor, Publish::Delta);
    assert_eq!(
        editor.body.rows.len(),
        2,
        "undo did not put the paragraph back"
    );
    assert_eq!(
        undone.removed.len(),
        1,
        "the split row was not reported as gone"
    );
    assert_eq!(
        access.published_run_ids(survivor),
        ids_before,
        "an undo moved a surviving paragraph's run ids, so every range a screen \
         reader held went stale"
    );
}

/// A row that leaves the document is reported once and then forgotten, so the
/// map does not grow for the life of the session.
#[test]
fn a_deleted_row_is_reported_and_then_forgotten() {
    let (mut ws, mut editor) = document(&["one".to_string(), "two".to_string()]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);
    assert_eq!(access.tracked_rows(), 2);

    let gone = editor.body.rows[1].id;
    ws.delete(gone).expect("the row is deletable");
    editor.body.rebuild(&ws);
    editor.reflow();

    let (update, _) = publish(&mut access, &editor, Publish::Delta);
    assert_eq!(update.removed, vec![gone]);
    assert_eq!(update.order.as_ref().map(Vec::len), Some(1));
    assert_eq!(access.tracked_rows(), 1);

    let (again, _) = publish(&mut access, &editor, Publish::Delta);
    assert!(again.removed.is_empty(), "a row was reported gone twice");
}

// ------------------------------------------------------ activation again

/// A client that attaches, leaves and attaches again is owed the whole
/// document — and must not pay for shaping it twice.
#[test]
fn a_second_activation_republishes_everything_without_reshaping_anything() {
    let (_ws, editor) = document(&paragraphs(120));
    let mut access = DocumentAccessibility::default();
    let (_, first) = publish(&mut access, &editor, Publish::Whole);
    assert!(first.shapings > 0);

    access.forget_published();
    let (second_update, second) = publish(&mut access, &editor, Publish::Whole);
    assert_eq!(
        second_update.rows.len(),
        120,
        "the second client got a partial tree"
    );
    assert_eq!(
        second.shapings, 0,
        "a re-activation shaped the document again"
    );
}

// ------------------------------------------------------------- unicode

/// RTL and mixed-direction content: the runs still cover the row's bytes
/// exactly, and reading order is parley's bidi order rather than byte order.
#[test]
fn rtl_and_mixed_direction_paragraphs_publish_their_exact_bytes() {
    let mixed = [
        // Pure RTL.
        "שלום עולם".to_string(),
        // Arabic with an embedded Latin word and a number — three directions in
        // one line, which is where a byte-ordered projection goes wrong.
        "مرحبا Koru 2026 بالعالم".to_string(),
        // An RTL run inside an LTR sentence.
        "The word שלום means peace.".to_string(),
    ];
    let (_ws, editor) = document(&mixed);
    let mut access = DocumentAccessibility::default();
    let (update, _) = publish(&mut access, &editor, Publish::Whole);

    for (index, row) in editor.body.rows.iter().enumerate() {
        let published = update
            .rows
            .iter()
            .find(|published| published.id == row.id)
            .expect("every row is published");
        let RowText::Runs { runs, .. } = &published.text else {
            panic!("a laid-out paragraph published no runs");
        };
        let covered: usize = runs
            .iter()
            .map(|(_, node)| {
                node.character_lengths()
                    .iter()
                    .copied()
                    .map(usize::from)
                    .sum::<usize>()
            })
            .sum();
        assert_eq!(
            covered,
            row.text.len(),
            "row {index} ({:?}) is not covered exactly by its runs",
            row.text
        );
        // Every run states a direction, on itself or inherited: without one
        // anywhere on the chain `bounding_boxes()` is empty
        // (`guide/contributing/deps/ACCESSKIT.md`).
        assert!(
            runs.iter().any(|(_, node)| node.text_direction().is_some()),
            "row {index} published no text direction at all"
        );
    }
}

/// A long unbroken run is where `accesskit::Node::word_starts` being a
/// `Box<[u8]>` bites. parley splits at 256 characters; this is the assertion
/// that Koru inherits the split rather than the wrap-around.
#[test]
fn a_long_unbroken_run_is_split_below_the_word_start_cap() {
    let long = "x".repeat(2_000);
    let (_ws, editor) = document(std::slice::from_ref(&long));
    let mut access = DocumentAccessibility::default();
    let (update, _) = publish(&mut access, &editor, Publish::Whole);
    let RowText::Runs { runs, .. } = &update.rows[0].text else {
        panic!("the long paragraph published no runs");
    };
    for (_, node) in runs {
        assert!(
            node.character_lengths().len() <= MAX_WORD_STARTS_INDEX + 1,
            "a run of {} characters is past the cap `word_starts` can express",
            node.character_lengths().len()
        );
        for start in node.word_starts().iter() {
            assert!(usize::from(*start) < node.character_lengths().len());
        }
    }
    let covered: usize = runs
        .iter()
        .map(|(_, node)| {
            node.character_lengths()
                .iter()
                .copied()
                .map(usize::from)
                .sum::<usize>()
        })
        .sum();
    assert_eq!(covered, long.len());
    println!(
        "a {}-character unbroken run became {} runs of {:?} characters",
        long.chars().count(),
        runs.len(),
        runs.iter()
            .map(|(_, node)| node.character_lengths().len())
            .collect::<Vec<_>>()
    );
}

/// An empty paragraph is still a text container with exactly one empty run —
/// `accesskit_consumer` refuses a Text pattern to a container with none — and
/// that survives the incremental path as well as the single-row one.
#[test]
fn a_blank_paragraph_survives_the_incremental_path() {
    let (_ws, editor) = document(&[String::new(), "after".to_string()]);
    let mut access = DocumentAccessibility::default();
    let (update, _) = publish(&mut access, &editor, Publish::Whole);
    let RowText::Runs { runs, .. } = &update.rows[0].text else {
        panic!("an empty paragraph published no runs");
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].1.value(), Some(""));
}

/// **Every same-line link must name a run this row publishes.**
///
/// `accesskit_consumer` follows `next_on_line`/`previous_on_line` with an
/// `unwrap()`, so a dangling link is not a wrong answer: it aborts the process
/// from inside a UI Automation callback, with no Rust backtrace and no message
/// a person could act on. This is the assertion that keeps the strut run's
/// stable id from costing that — it fails on the version of this module that
/// renamed the run without relinking its neighbour.
#[test]
fn no_run_links_to_a_run_that_was_never_published() {
    // Wrapped, so the paragraph really has several runs and several lines; then
    // edited, so the ids are reassigned at least once.
    let wide = "This paragraph is deliberately long enough to wrap onto several \
                visual lines at this width, which is where the same-line links \
                are used at all."
        .to_string();
    let (mut ws, mut editor) = document(&[wide, "second".to_string()]);
    let mut access = DocumentAccessibility::default();

    let check = |update: &BodyUpdate| {
        for row in &update.rows {
            let RowText::Runs { runs, .. } = &row.text else {
                continue;
            };
            let ids: std::collections::HashSet<NodeId> = runs.iter().map(|(id, _)| *id).collect();
            for (id, node) in runs {
                for link in [node.next_on_line(), node.previous_on_line()]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        ids.contains(&link),
                        "run #{} links to #{}, which this row never published",
                        id.0,
                        link.0
                    );
                }
            }
        }
    };

    check(&publish(&mut access, &editor, Publish::Whole).0);
    editor.body.focus = 0;
    editor.body.rows[0].place_caret(0);
    editor.reflow();
    for _ in 0..4 {
        editor.insert_text(&mut ws, "Q");
        editor.reflow();
        check(&publish(&mut access, &editor, Publish::Delta).0);
    }
}

/// The end of a paragraph is a place a screen reader keeps a range, and the
/// strut run is what an end-of-text position names. Its id must not move when
/// the text before it does.
#[test]
fn the_end_of_a_paragraph_keeps_its_run_id_across_an_edit() {
    let (mut ws, mut editor) = document(&["one two".to_string()]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);
    let row = editor.body.rows[0].id;
    let before = *access
        .published_run_ids(row)
        .last()
        .expect("a paragraph publishes at least one run");

    editor.body.focus = 0;
    editor.body.rows[0].place_caret(0);
    editor.reflow();
    editor.insert_text(&mut ws, "Z");
    editor.reflow();
    publish(&mut access, &editor, Publish::Delta);

    assert_eq!(
        *access
            .published_run_ids(row)
            .last()
            .expect("still at least one run"),
        before,
        "typing moved the run an end-of-text range names, so every such range a \
         screen reader held went stale"
    );
}

// ------------------------------------------------------- inline objects

/// **A pill must not be silently swallowed.**
///
/// `accesskit_windows` implements neither `RangeFromChild` nor
/// `ITextRangeProvider::GetChildren`, so an object inside a text range is
/// invisible to a client at every layer below this one — a Koru pill reaches
/// the text as `koru_core::text::ANCHOR`, one default-ignorable `U+2060`. §5
/// forbids both halves of that: swallowed silently, and spoken as a raw storage
/// anchor. So the object is published beside the runs, with the pill's own kind
/// and its own label, and nothing invented.
#[test]
fn a_pill_is_published_beside_the_runs_with_its_own_label_and_kind() {
    use koru_core::node::{Attr, AttrValue};

    let (mut ws, mut editor) = document(&["Due ".to_string()]);
    let row_id = editor.body.rows[0].id;
    let attr = Attr::new(
        "due",
        AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-02").unwrap()),
    );
    ws.insert_pill(row_id, 4, &attr).expect("a pill");
    editor.body.rebuild(&ws);
    editor.reflow();

    let mut access = DocumentAccessibility::default();
    let (update, _) = publish(&mut access, &editor, Publish::Whole);
    let row = &update.rows[0];
    assert_eq!(row.objects.len(), 1, "the pill was swallowed");
    let object = &row.objects[0];
    assert_eq!(object.attr, attr.id);
    assert_eq!(object.kind, koru_draw::pill::PillKind::Date);
    assert_eq!(
        object.label, "Aug 2",
        "the pill's own caption, not an invention"
    );
    assert_eq!(object.at, 4, "the anchor's byte offset in the row's text");
    let bounds = object.bounds.expect("a laid-out pill has a rectangle");
    assert!(
        bounds.w > 1.0 && bounds.h > 1.0,
        "a pill with no extent: {bounds:?}"
    );

    // The anchor is still in the text and still covered by the runs — the
    // object is published *beside* them, not instead of a byte.
    let RowText::Runs { runs, .. } = &row.text else {
        panic!("no runs");
    };
    let covered: usize = runs
        .iter()
        .map(|(_, node)| {
            node.character_lengths()
                .iter()
                .copied()
                .map(usize::from)
                .sum::<usize>()
        })
        .sum();
    assert_eq!(covered, editor.body.rows[0].text.len());
}

/// **A simple text edit must not destroy the mixed content.** §5 names this
/// explicitly, and it is the case a caret-offset bug quietly breaks: the anchor
/// moves, and a projection that remembered a byte offset instead of the model's
/// own identity loses the pill.
#[test]
fn typing_before_a_pill_moves_it_and_does_not_lose_it() {
    use koru_core::node::{Attr, AttrValue};

    let (mut ws, mut editor) = document(&["Due ".to_string()]);
    let row_id = editor.body.rows[0].id;
    let attr = Attr::new(
        "due",
        AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-02").unwrap()),
    );
    ws.insert_pill(row_id, 4, &attr).expect("a pill");
    editor.body.rebuild(&ws);
    editor.reflow();

    let mut access = DocumentAccessibility::default();
    let (before, _) = publish(&mut access, &editor, Publish::Whole);
    let was = before.rows[0].objects[0].clone();

    editor.body.focus = 0;
    editor.body.rows[0].place_caret(0);
    editor.reflow();
    editor.insert_text(&mut ws, "Very ");
    editor.reflow();

    let (after, _) = publish(&mut access, &editor, Publish::Delta);
    let row = after
        .rows
        .iter()
        .find(|row| row.id == row_id)
        .expect("the edited row was republished");
    assert_eq!(row.objects.len(), 1, "a keystroke destroyed the pill");
    let now = &row.objects[0];
    assert_eq!(now.attr, was.attr, "the pill changed identity");
    assert_eq!(now.label, was.label, "the pill's caption was rewritten");
    assert_eq!(
        now.at,
        was.at + "Very ".len(),
        "the anchor did not move with the text before it"
    );
}

/// Merging two paragraphs — Backspace at the start of one — removes a row and
/// changes the order, and the paragraph that survives keeps the run ids a
/// screen reader is holding ranges against.
#[test]
fn a_merge_removes_one_row_and_leaves_the_survivor_addressable() {
    let (mut ws, mut editor) = document(&["one".to_string(), "two".to_string()]);
    let mut access = DocumentAccessibility::default();
    publish(&mut access, &editor, Publish::Whole);

    let survivor = editor.body.rows[0].id;
    let swallowed = editor.body.rows[1].id;
    let ids_before = access.published_run_ids(survivor);

    editor.body.focus = 1;
    editor.body.rows[1].place_caret(0);
    editor.reflow();
    let _ = publish(&mut access, &editor, Publish::Delta);
    assert!(editor.act(&mut ws, Action::DeleteBack { by_word: false }));
    editor.reflow();

    let (merged, _) = publish(&mut access, &editor, Publish::Delta);
    assert_eq!(editor.body.rows.len(), 1, "the two rows did not merge");
    assert_eq!(editor.body.rows[0].text, "onetwo");
    assert_eq!(merged.removed, vec![swallowed]);
    assert_eq!(merged.order.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        access.published_run_ids(survivor)[0],
        ids_before[0],
        "the surviving paragraph's first run changed id, so every range a \
         screen reader held against it went stale"
    );
    assert_eq!(access.tracked_rows(), 1, "the merged row was not forgotten");
}

#[test]
fn pane_origin_moves_rows_runs_pills_and_checkboxes_without_shaping() {
    use koru_core::node::{Attr, AttrValue, Kind};
    let (mut ws, mut editor) = document(&paragraphs(100));
    let id = editor.body.rows[0].id;
    ws.set_kind(id, Kind::Task).unwrap();
    ws.insert_pill(
        id,
        4,
        &Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-02").unwrap()),
        ),
    )
    .unwrap();
    editor.body.rebuild(&ws);
    editor.reflow();
    let mut access = DocumentAccessibility::default();
    let (first, _) = publish(&mut access, &editor, Publish::Whole);
    let original = &first.rows[0];
    let shapings = access.counters().shapings;
    let absent = first
        .rows
        .iter()
        .filter(|row| matches!(row.text, RowText::Absent))
        .count();
    assert!(absent > 0);
    for origin in [(260.0, 42.0), (700.0, 370.0)] {
        let mut view = view_of(&editor);
        view.origin = origin;
        let update = access.body(&editor.body.rows, view, Some(0), Publish::Delta, base_of);
        let row = update.rows.iter().find(|row| row.id == id).unwrap();
        let moved = |bounds: Rect| geometry::shift(bounds, origin.0, origin.1);
        assert_eq!(row.bounds, original.bounds.map(moved));
        assert_eq!(row.checkbox_bounds, original.checkbox_bounds.map(moved));
        assert!(row.checkbox_bounds.is_some());
        assert_eq!(row.objects.len(), 1);
        assert_eq!(row.objects[0].bounds, original.objects[0].bounds.map(moved));
        let (RowText::Runs { runs: before, .. }, RowText::Runs { runs: after, .. }) =
            (&original.text, &row.text)
        else {
            panic!("moved runs must be republished")
        };
        assert_eq!(before.len(), after.len());
        for ((before_id, before), (after_id, after)) in before.iter().zip(after) {
            assert_eq!(before_id, after_id);
            let a = before.bounds().unwrap();
            let b = after.bounds().unwrap();
            assert!((b.x0 - a.x0 - f64::from(origin.0)).abs() < 0.001);
            assert!((b.y0 - a.y0 - f64::from(origin.1)).abs() < 0.001);
            assert_eq!(before.character_positions(), after.character_positions());
        }
        assert_eq!(access.counters().shapings, shapings);
        assert!(
            update
                .rows
                .iter()
                .all(|row| !matches!(row.text, RowText::Absent)),
            "pane movement must not expose unshaped rows"
        );
    }
}

#[test]
fn content_clip_matches_page_chrome_and_other_mode_columns() {
    use koru_draw::theme::ViewMode;
    let (_, mut editor) = document(&["clip".into()]);
    editor.view.chrome_h = 48.0;
    editor.view.status_h = 24.0;
    editor.view.view_mode = ViewMode::Page;
    assert_eq!(
        editor.view.content_clip(),
        Rect::new(0.0, 48.0, VIEWPORT_W, VIEWPORT_H - 72.0)
    );
    for mode in [ViewMode::Full, ViewMode::Narrow] {
        editor.view.view_mode = mode;
        editor.view.content_x = mode.content_x(VIEWPORT_W);
        assert_eq!(
            editor.view.content_clip(),
            Rect::new(
                mode.content_x(VIEWPORT_W),
                0.0,
                mode.effective_w(VIEWPORT_W),
                VIEWPORT_H
            )
        );
    }
}

// ================================================================ diagrams

const DIAGRAM_TEXT: &str = "flowchart LR\n  A --> B\n  B --> C";

/// A document of a paragraph, a `mermaid` block of `text` and a paragraph
/// after it, opened in a real editor at a real viewport, with the host's
/// `answer` for the block landed through the editor's own seam — or not
/// landed, which leaves the block waiting.
fn diagram_document(
    text: &str,
    answer: Option<Result<Arc<PreparedDiagram>, diagram::Status>>,
) -> (WorkspaceDoc, Editor, KoruNodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "diagram fixture")
        .expect("an in-memory workspace");
    let doc = ws
        .insert(
            Parent::Root,
            CoreNode::document("Fixture").expect("a document"),
        )
        .expect("the document node");
    ws.insert(Parent::Node(doc), CoreNode::paragraph("before").unwrap())
        .unwrap();
    let node = ws
        .insert(Parent::Node(doc), CoreNode::code(text).unwrap())
        .unwrap();
    ws.set_language(node, Some(diagram::LANGUAGE)).unwrap();
    ws.insert(Parent::Node(doc), CoreNode::paragraph("after").unwrap())
        .unwrap();
    ws.enable_undo();
    let mut editor = Editor::rooted_at(&ws, doc, true);
    editor.view.code_enabled = true;
    editor.body.rebuild(&ws);
    editor.set_viewport(VIEWPORT_W, VIEWPORT_H);
    editor.view.active = true;
    editor.reflow();
    if let Some(answer) = answer {
        assert!(
            editor.diagram_result(node, diagram::text_hash(text), answer),
            "the answer is for the text in flight"
        );
        editor.reflow();
    }
    (ws, editor, node)
}

fn diagram_row(update: &BodyUpdate, node: KoruNodeId) -> &RowUpdate {
    update
        .rows
        .iter()
        .find(|row| row.id == node)
        .expect("the diagram row is in the update")
}

/// **The first block with no text and a name of its own.** A drawn diagram
/// goes out with no runs and its narration as its fact; opening the text
/// turns it back into the code row, whose runs must then be *published* —
/// the client never had them, so naming them as retained would name children
/// it does not hold; closing it again takes them away.
#[test]
fn a_drawn_diagram_is_a_picture_named_with_its_narration_until_its_text_opens() {
    let (mut ws, mut editor, node) =
        diagram_document(FLOWCHART_SHOWCASE, Some(Ok(laid_out(FLOWCHART_SHOWCASE))));
    let mut doc = DocumentAccessibility::default();
    let (update, _) = publish(&mut doc, &editor, Publish::Whole);
    let row = diagram_row(&update, node);
    assert!(
        matches!(row.text, RowText::Absent),
        "a picture has no runs: {:?}",
        row.text
    );
    assert_eq!(row.facts.diagram.map(|fact| fact.picture), Some(true));
    let name = row.diagram.as_deref().expect("a picture is named");
    assert!(
        name.starts_with("Checkout flow. How an order moves"),
        "{name}"
    );
    assert!(
        name.contains("Diagram, flowchart, 10 boxes, 12 arrows. Cart, arrow to Valid order? "),
        "{name}"
    );
    assert!(doc.published_run_ids(node).is_empty());
    assert_eq!(stubs(&update), 1, "only the picture is without runs");
    assert_eq!(with_text(&update), update.rows.len() - 1);
    assert_eq!(crate::accessibility::events::coherent(&update), Ok(()));

    // An idle publish skips the picture like every other unchanged row.
    let (idle, counters) = publish(&mut doc, &editor, Publish::Delta);
    assert!(idle.rows.is_empty(), "{idle:?}");
    assert_eq!(counters.shapings, 0);

    // Enter opens the text.
    assert!(editor.open_diagram_text(node));
    editor.reflow();
    let (update, counters) = publish(&mut doc, &editor, Publish::Delta);
    let row = diagram_row(&update, node);
    assert!(
        row.diagram.is_none() && row.facts.diagram.is_none(),
        "open text is read as the text it is"
    );
    assert!(
        matches!(row.text, RowText::Runs { .. }),
        "the client never had these runs: {:?}",
        row.text
    );
    assert_eq!(counters.shapings, 1, "the code text is projected now, once");
    assert!(!doc.published_run_ids(node).is_empty());
    assert_eq!(crate::accessibility::events::coherent(&update), Ok(()));

    // Esc closes it: the picture again, and the runs go with their parent.
    assert!(editor.key(
        &mut ws,
        &winit::keyboard::Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    editor.reflow();
    let (update, _) = publish(&mut doc, &editor, Publish::Delta);
    let row = diagram_row(&update, node);
    assert!(matches!(row.text, RowText::Absent), "{:?}", row.text);
    assert_eq!(row.diagram.as_deref(), Some(name));
    assert!(doc.published_run_ids(node).is_empty());
}

/// A block waiting for its drawing is a picture named *Diagram, drawing*;
/// the host's answer renames it, moves its fact, and shapes no text.
#[test]
fn a_host_answer_renames_the_picture_and_moves_its_fact() {
    let (_ws, mut editor, node) = diagram_document(DIAGRAM_TEXT, None);
    let mut doc = DocumentAccessibility::default();
    let (update, _) = publish(&mut doc, &editor, Publish::Whole);
    let row = diagram_row(&update, node);
    assert_eq!(row.diagram.as_deref(), Some("Diagram, drawing"));
    assert!(matches!(row.text, RowText::Absent));
    let before = row
        .facts
        .diagram
        .expect("a waiting block is a diagram fact");
    assert!(before.picture);

    assert!(editor.diagram_result(
        node,
        diagram::text_hash(DIAGRAM_TEXT),
        Ok(laid_out(DIAGRAM_TEXT))
    ));
    editor.reflow();
    let (update, counters) = publish(&mut doc, &editor, Publish::Delta);
    let row = diagram_row(&update, node);
    assert_eq!(
        row.diagram.as_deref(),
        Some("Diagram, flowchart, 3 boxes, 2 arrows. A, arrow to B. B, arrow to C")
    );
    assert!(matches!(row.text, RowText::Absent));
    let after = row.facts.diagram.expect("a drawn block is a diagram fact");
    assert!(after.picture);
    assert_ne!(after.stamp, before.stamp, "the fact moved with the answer");
    assert_eq!(counters.shapings, 0, "a drawing landing shapes no text");
}

/// A block whose text shows because it does not draw keeps its text — a
/// person is fixing it — and wears the reason as its name.
#[test]
fn a_block_that_does_not_draw_keeps_its_text_under_the_reason() {
    let status = diagram::Status::NotParsed {
        line: 2,
        message: "unexpected token".into(),
        code: "merman.parse.diagram_parse".into(),
    };
    let (_ws, editor, node) = diagram_document(DIAGRAM_TEXT, Some(Err(status)));
    let mut doc = DocumentAccessibility::default();
    let (update, _) = publish(&mut doc, &editor, Publish::Whole);
    let row = diagram_row(&update, node);
    assert!(matches!(row.text, RowText::Runs { .. }), "{:?}", row.text);
    assert_eq!(
        row.diagram.as_deref(),
        Some("Diagram, not drawn: Line 2: this line could not be read")
    );
    assert_eq!(row.facts.diagram.map(|fact| fact.picture), Some(false));
    assert!(!doc.published_run_ids(node).is_empty());
}
