// SPDX-License-Identifier: MPL-2.0

//! Formatting as marks, and pills as anchored attributes.
//!
//! These replace the old delimiter tests: there is no `**bold**` in the text
//! any more, so what has to be proven is that marks survive editing around
//! them, that pill anchors stay put, and that two peers formatting the same
//! line converge.

use chrono::NaiveDate;
use koru_core::node::{Attr, AttrValue, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::text::{ANCHOR_LEN, ANCHOR_STR, Script};
use koru_core::{DocRef, DocumentId, DocumentReference, Mark, NodeId, ProfileId, Ref, WorkspaceId};

/// An empty workspace, for the tests that build their own nodes.
fn ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap()
}

fn workspace(line: &str) -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let id = ws.insert(Parent::Root, Node::task(line).unwrap()).unwrap();
    (ws, id)
}

/// Runs as `(text, bold, italic, underline)`.
fn shape(ws: &WorkspaceDoc, id: NodeId) -> Vec<(String, bool, bool, bool)> {
    ws.runs(id)
        .unwrap()
        .into_iter()
        .map(|r| (r.text, r.bold, r.italic, r.underline))
        .collect()
}

#[test]
fn plain_text_is_one_run() {
    let (ws, id) = workspace("Reward payout flow");
    assert_eq!(
        shape(&ws, id),
        vec![("Reward payout flow".into(), false, false, false)]
    );
}

#[test]
fn a_mark_splits_the_line_into_runs() {
    let (mut ws, id) = workspace("Reward payout flow");
    ws.set_mark(id, 0..6, Mark::Bold, true).unwrap();

    assert_eq!(
        shape(&ws, id),
        vec![
            ("Reward".into(), true, false, false),
            (" payout flow".into(), false, false, false),
        ]
    );
    // And crucially: the delimiters are not in the text.
    assert_eq!(
        ws.read(id).unwrap().content.as_text(),
        Some("Reward payout flow")
    );
}

/// The case the old parser needed special handling for — `***x***`.
#[test]
fn marks_stack_without_any_parsing() {
    let (mut ws, id) = workspace("Reward payout flow");
    ws.set_mark(id, 0..6, Mark::Bold, true).unwrap();
    ws.set_mark(id, 0..6, Mark::Italic, true).unwrap();
    ws.set_mark(id, 0..6, Mark::Underline, true).unwrap();

    let runs = ws.runs(id).unwrap();
    assert!(runs[0].bold && runs[0].italic && runs[0].underline);
    assert_eq!(
        ws.read(id).unwrap().content.as_text(),
        Some("Reward payout flow")
    );
}

#[test]
fn a_mark_can_be_cleared() {
    let (mut ws, id) = workspace("Reward payout flow");
    ws.set_mark(id, 0..6, Mark::Bold, true).unwrap();
    ws.set_mark(id, 0..6, Mark::Bold, false).unwrap();
    assert_eq!(
        shape(&ws, id),
        vec![("Reward payout flow".into(), false, false, false)]
    );
}

/// Formatting must survive edits elsewhere in the line — the whole reason for
/// marks rather than byte-indexed spans maintained by hand.
#[test]
fn a_mark_moves_when_text_is_inserted_before_it() {
    let (mut ws, id) = workspace("payout flow");
    ws.set_mark(id, 0..6, Mark::Bold, true).unwrap(); // "payout"
    ws.insert_text(id, 0, "reward ").unwrap();

    assert_eq!(
        shape(&ws, id),
        vec![
            ("reward ".into(), false, false, false),
            ("payout".into(), true, false, false),
            (" flow".into(), false, false, false),
        ],
        "the bold followed its text instead of staying at bytes 0..6"
    );
}

#[test]
fn typing_at_the_end_of_a_bold_word_continues_it() {
    let (mut ws, id) = workspace("bol");
    ws.set_mark(id, 0..3, Mark::Bold, true).unwrap();
    ws.insert_text(id, 3, "d").unwrap();
    assert_eq!(shape(&ws, id), vec![("bold".into(), true, false, false)]);
}

#[test]
fn marks_survive_non_ascii_text() {
    let (mut ws, id) = workspace("보상 약관 검토");
    let bold = "보상 ".len(); // byte offset, not char count
    ws.set_mark(id, bold..bold + "약관".len(), Mark::Bold, true)
        .unwrap();

    let runs = ws.runs(id).unwrap();
    let bolded: Vec<_> = runs
        .iter()
        .filter(|r| r.bold)
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(bolded, vec!["약관"]);
}

// ---------------------------------------------------------------- pills

fn due(y: i32, m: u32, d: u32) -> Attr {
    Attr::new(
        "due",
        AttrValue::Date(koru_core::time::DateOnly::new(
            NaiveDate::from_ymd_opt(y, m, d).unwrap(),
        )),
    )
}

#[test]
fn a_pill_is_an_anchor_marked_with_its_attribute() {
    let (mut ws, id) = workspace("Reward flow ");
    let attr = due(2026, 8, 2);
    let attr_id = ws.insert_pill(id, 12, &attr).unwrap();

    let runs = ws.runs(id).unwrap();
    let pill = runs.iter().find(|r| r.is_pill()).expect("a pill run");
    assert_eq!(pill.pill, Some(attr_id));
    assert_eq!(pill.text, ANCHOR_STR, "the anchor is what holds the place");
    assert_eq!(pill.range().len(), ANCHOR_LEN);

    // And the attribute is queryable without touching the text.
    assert_eq!(
        ws.read(id).unwrap().due(),
        Some(koru_core::time::Temporal::Date(
            koru_core::time::DateOnly::new(NaiveDate::from_ymd_opt(2026, 8, 2).unwrap())
        ))
    );
}

/// The behaviour the old model needed a `BTreeMap<usize, Pill>` and manual
/// index shifting to achieve.
#[test]
fn a_pill_stays_on_its_anchor_when_text_is_inserted_before_it() {
    let (mut ws, id) = workspace("flow ");
    let attr_id = ws.insert_pill(id, 5, &due(2026, 8, 2)).unwrap();

    ws.insert_text(id, 0, "Reward payout ").unwrap();

    let runs = ws.runs(id).unwrap();
    let pill = runs.iter().find(|r| r.is_pill()).unwrap();
    assert_eq!(pill.pill, Some(attr_id));
    assert_eq!(
        pill.start.get(),
        "Reward payout flow ".len(),
        "the anchor moved with the text, with no bookkeeping on our side"
    );
}

/// Typing next to a pill must not be absorbed into it — the reason `pill` is
/// configured with `ExpandType::None`.
#[test]
fn typing_beside_a_pill_does_not_join_it() {
    let (mut ws, id) = workspace("for ");
    ws.insert_pill(id, 4, &due(2026, 8, 2)).unwrap();

    ws.insert_text(id, 4 + ANCHOR_LEN, " today").unwrap();

    let runs = ws.runs(id).unwrap();
    let pills: Vec<_> = runs.iter().filter(|r| r.is_pill()).collect();
    assert_eq!(pills.len(), 1);
    assert_eq!(
        pills[0].text, ANCHOR_STR,
        "the pill is still just its anchor"
    );
    assert!(runs.iter().any(|r| r.text == " today" && !r.is_pill()));
}

/// Deleting the anchor removes the whole pill, attribute included.
#[test]
fn deleting_the_anchor_removes_the_attribute() {
    let (mut ws, id) = workspace("Reward flow ");
    ws.insert_pill(id, 12, &due(2026, 8, 2)).unwrap();
    assert!(ws.read(id).unwrap().due().is_some());

    ws.delete_text(id, 12..12 + ANCHOR_LEN).unwrap();

    assert!(ws.runs(id).unwrap().iter().all(|r| !r.is_pill()));
    assert_eq!(
        ws.read(id).unwrap().due(),
        None,
        "the attribute went with it"
    );
}

#[test]
fn deleting_text_around_a_pill_leaves_it_alone() {
    let (mut ws, id) = workspace("Reward flow ");
    ws.insert_pill(id, 12, &due(2026, 8, 2)).unwrap();

    ws.delete_text(id, 0..7).unwrap(); // "Reward "

    assert!(ws.runs(id).unwrap().iter().any(|r| r.is_pill()));
    assert!(ws.read(id).unwrap().due().is_some());
}

#[test]
fn several_pills_coexist_on_one_line() {
    let (mut ws, id) = workspace("a b ");
    let first = ws.insert_pill(id, 2, &due(2026, 8, 2)).unwrap();
    let second = ws
        .insert_pill(
            id,
            4 + ANCHOR_LEN,
            &Attr::new("assignee", AttrValue::Ref(Ref::new(NodeId::new()))),
        )
        .unwrap();

    let found: Vec<_> = ws.runs(id).unwrap().iter().filter_map(|r| r.pill).collect();
    assert_eq!(found.len(), 2);
    assert!(found.contains(&first) && found.contains(&second));
}

#[test]
fn a_legacy_routed_document_reference_decodes_to_stable_identity() {
    let (mut ws, id) = workspace("see ");
    let (profile, workspace, document) = (ProfileId::new(), WorkspaceId::new(), DocumentId::new());
    let reference = Ref::in_doc(
        NodeId::from_uuid(document.as_uuid()),
        DocRef::DocumentAt {
            profile,
            workspace,
            document,
        },
    );
    ws.insert_pill(
        id,
        4,
        &Attr::labelled("document", AttrValue::Ref(reference), "Hello"),
    )
    .unwrap();

    let node = ws.read(id).unwrap();
    let stored = node
        .content
        .attrs()
        .first()
        .unwrap()
        .as_document_reference();
    assert_eq!(stored, Some(DocumentReference::new(document)));
}

#[test]
fn a_document_reference_round_trips_an_optional_block_target() {
    let (mut ws, id) = workspace("see ");
    let reference = DocumentReference::at(DocumentId::new(), NodeId::new());
    ws.insert_pill(
        id,
        4,
        &Attr::labelled("document", AttrValue::Document(reference), "Hello"),
    )
    .unwrap();

    let node = ws.read(id).unwrap();
    assert_eq!(
        node.content
            .attrs()
            .first()
            .unwrap()
            .as_document_reference(),
        Some(reference)
    );
}

// ---------------------------------------------------------------- toolbar

#[test]
fn covering_marks_drive_whether_ctrl_b_sets_or_clears() {
    let (mut ws, id) = workspace("bold plain");
    ws.set_mark(id, 0..4, Mark::Bold, true).unwrap();

    assert!(ws.marks_in(id, 0..4).unwrap().bold, "fully bold");
    assert!(!ws.marks_in(id, 0..10).unwrap().bold, "only partly bold");
    assert!(!ws.marks_in(id, 5..10).unwrap().bold, "not bold at all");
}

// ---------------------------------------------------------------- sync

#[test]
fn concurrent_formatting_converges() {
    let (mut a, id) = workspace("reward payout flow");
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.set_mark(id, 0..6, Mark::Bold, true).unwrap(); // "reward"
    b.set_mark(id, 7..13, Mark::Italic, true).unwrap(); // "payout"

    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();

    assert_eq!(shape(&a, id), shape(&b, id), "replicas agree");
    let runs = a.runs(id).unwrap();
    assert!(runs.iter().any(|r| r.text == "reward" && r.bold));
    assert!(runs.iter().any(|r| r.text == "payout" && r.italic));
}

#[test]
fn a_pill_and_a_concurrent_edit_to_the_same_line_both_survive() {
    let (mut a, id) = workspace("payout flow ");
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.insert_pill(id, 12, &due(2026, 8, 2)).unwrap();
    b.insert_text(id, 0, "reward ").unwrap();

    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();

    assert_eq!(shape(&a, id), shape(&b, id));
    assert!(
        a.runs(id).unwrap().iter().any(|r| r.is_pill()),
        "pill survived"
    );
    assert!(
        a.read(id)
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .starts_with("reward"),
        "and so did the prepend"
    );
    assert!(a.read(id).unwrap().due().is_some());
}

// -------------------------------------------------------------- value marks

/// A value mark is stored like a flag but carries data. This is the property
/// everything in phase 1 waits on — highlight colour, font size, sub/superscript
/// and later comment anchors are all the same mechanism.
#[test]
fn value_marks_round_trip_through_the_document() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("normal big yellow").unwrap())
        .unwrap();

    ws.set_size(id, 7..10, Some(24.0)).unwrap();
    ws.set_highlight(id, 11..17, Some("yellow")).unwrap();
    ws.set_script(id, 0..6, Some(Script::Super)).unwrap();

    let runs = ws.runs(id).unwrap();
    let big: String = runs
        .iter()
        .filter(|r| r.style.size == Some(24.0))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(big, "big");

    let lit: String = runs
        .iter()
        .filter(|r| r.style.highlight.as_deref() == Some("yellow"))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(lit, "yellow");

    let sup: String = runs
        .iter()
        .filter(|r| r.style.script == Some(Script::Super))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(sup, "normal");
}

/// Clearing a value has to remove the mark, not set it to a default — a run
/// with no size follows the theme, and a run sized 16 does not.
#[test]
fn clearing_a_value_mark_removes_it() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("highlighted").unwrap())
        .unwrap();

    ws.set_highlight(id, 0..11, Some("green")).unwrap();
    assert!(
        ws.runs(id)
            .unwrap()
            .iter()
            .any(|r| r.style.highlight.is_some())
    );

    ws.set_highlight(id, 0..11, None).unwrap();
    assert!(
        ws.runs(id)
            .unwrap()
            .iter()
            .all(|r| r.style.highlight.is_none()),
        "the mark is gone, not set to a default colour"
    );
}

/// The same journey a flag takes: split carries it to the new node, merge
/// carries it back.
#[test]
fn a_value_mark_survives_split_and_merge() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("plain marked").unwrap())
        .unwrap();
    ws.set_highlight(id, 6..12, Some("blue")).unwrap();
    ws.set_size(id, 6..12, Some(20.0)).unwrap();

    let tail = ws.split(id, 6).unwrap();
    let runs = ws.runs(tail).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "marked");
    assert_eq!(runs[0].style.highlight.as_deref(), Some("blue"));
    assert_eq!(runs[0].style.size, Some(20.0));
    assert!(
        ws.runs(id)
            .unwrap()
            .iter()
            .all(|r| r.style.highlight.is_none()),
        "and the head did not inherit it"
    );

    ws.merge_into(id, tail).unwrap();
    let back: String = ws
        .runs(id)
        .unwrap()
        .iter()
        .filter(|r| r.style.highlight.as_deref() == Some("blue"))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(back, "marked", "exactly the text that was highlighted");
}

/// Two peers marking different words of the same line must both survive — the
/// property a value mark has to share with a flag.
#[test]
fn value_marks_from_two_peers_both_survive() {
    let mut a = ws();
    let id = a
        .insert(Parent::Root, Node::task("alpha beta").unwrap())
        .unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.set_highlight(id, 0..5, Some("yellow")).unwrap();
    b.set_size(id, 6..10, Some(28.0)).unwrap();

    a.import(&b.export_all_updates().unwrap()).unwrap();
    b.import(&a.export_all_updates().unwrap()).unwrap();

    for doc in [&a, &b] {
        let runs = doc.runs(id).unwrap();
        let lit: String = runs
            .iter()
            .filter(|r| r.style.highlight.as_deref() == Some("yellow"))
            .map(|r| r.text.as_str())
            .collect();
        let big: String = runs
            .iter()
            .filter(|r| r.style.size == Some(28.0))
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(lit, "alpha");
        assert_eq!(big, "beta");
    }
}

/// Values expand rightwards like flags, so typing after highlighted text
/// continues it — and `style_at` is what lets a toolbar offer to turn it off.
#[test]
fn a_value_mark_expands_into_what_is_typed_after_it() {
    let mut ws = ws();
    let id = ws.insert(Parent::Root, Node::task("lit").unwrap()).unwrap();
    ws.set_highlight(id, 0..3, Some("pink")).unwrap();

    assert_eq!(
        ws.style_at(id, 3).unwrap().highlight.as_deref(),
        Some("pink"),
        "text typed at the end would inherit it"
    );

    ws.insert_text(id, 3, "more").unwrap();
    let lit: String = ws
        .runs(id)
        .unwrap()
        .iter()
        .filter(|r| r.style.highlight.as_deref() == Some("pink"))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(lit, "litmore");
}

/// A pill anchor carries its pill mark and nothing else.
///
/// Every other mark expands rightwards, so an anchor inserted at the end of a
/// highlighted range would inherit the highlight — and a highlight paints a
/// band, which would stripe the pill drawn over it. Bold has the same problem
/// and got away with it only because a pill hides its anchor.
#[test]
fn a_pill_anchor_inherits_no_formatting_from_its_neighbour() {
    let mut ws = ws();
    let id = ws
        .insert(Parent::Root, Node::task("due ").unwrap())
        .unwrap();
    ws.set_highlight(id, 0..4, Some("yellow")).unwrap();
    ws.set_mark(id, 0..4, Mark::Bold, true).unwrap();

    let attr = Attr::new(
        "due",
        AttrValue::Date(koru_core::time::DateOnly::new(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
        )),
    );
    ws.insert_pill(id, 4, &attr).unwrap();

    let pill = ws
        .runs(id)
        .unwrap()
        .into_iter()
        .find(|r| r.is_pill())
        .expect("the anchor is there");
    assert!(
        pill.style.highlight.is_none(),
        "an anchor draws its pill, not the text's background"
    );
    assert!(!pill.bold, "nor its neighbour's weight");

    // The text either side keeps what it had.
    let lit: String = ws
        .runs(id)
        .unwrap()
        .iter()
        .filter(|r| r.style.highlight.as_deref() == Some("yellow"))
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(lit, "due ");
}

#[test]
fn a_stable_text_position_moves_with_a_remote_insert() {
    let (mut a, id) = workspace("a😀b");
    let snapshot = a.export_snapshot().unwrap();
    let mut b = WorkspaceDoc::from_snapshot(&snapshot).unwrap();

    // Byte five is after the four-byte emoji. Loro receives the corresponding
    // Unicode position and keeps it attached while another peer types before it.
    let position = a.capture_text_position(id, 5).unwrap();
    b.insert_text(id, 0, "zz").unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();

    assert_eq!(a.resolve_text_position(&position).unwrap(), 7);
}

#[test]
fn a_stable_range_preserves_selection_direction() {
    let (ws, id) = workspace("a😀b");
    let range = ws.capture_text_range(id, 6, 1).unwrap();
    assert_eq!(ws.resolve_text_range(&range).unwrap(), (6, 1));
}

#[test]
fn replacing_text_is_one_undo_step() {
    let (mut ws, id) = workspace(":rock");
    ws.enable_undo();
    assert_eq!(ws.replace_text(id, 0..5, "🚀").unwrap(), "🚀".len());
    assert_eq!(ws.read(id).unwrap().content.as_text(), Some("🚀"));
    assert!(ws.undo().unwrap());
    assert_eq!(ws.read(id).unwrap().content.as_text(), Some(":rock"));
}

#[test]
fn a_page_icon_replicates_as_node_metadata() {
    let mut a = ws();
    let page = a
        .insert(Parent::Root, Node::document("Roadmap").unwrap())
        .unwrap();
    a.set_icon(page, Some("🚀")).unwrap();

    let mut b = WorkspaceDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    assert_eq!(b.read(page).unwrap().meta.icon.as_deref(), Some("🚀"));

    b.set_icon(page, Some("🔥")).unwrap();
    a.import(&b.export_all_updates().unwrap()).unwrap();
    assert_eq!(a.read(page).unwrap().meta.icon.as_deref(), Some("🔥"));
}

/// A callout is a box: a container with no text of its own whose children
/// are the lines inside it. Wrapping two lines puts the box in their slot
/// with the lines under it; splitting a line inside stays inside; the look
/// lives on the box; dissolving the box leaves the lines where it was.
#[test]
fn a_callout_box_wraps_lines_and_dissolves_back_into_them() {
    let mut ws = ws();
    let before = ws
        .insert(Parent::Root, Node::paragraph("before").unwrap())
        .unwrap();
    let one = ws
        .insert(Parent::Root, Node::paragraph("Book tickets").unwrap())
        .unwrap();
    let two = ws
        .insert(Parent::Root, Node::paragraph("early").unwrap())
        .unwrap();
    let after = ws
        .insert(Parent::Root, Node::paragraph("after").unwrap())
        .unwrap();

    let callout = ws.wrap_in_callout_many(&[one, two]).unwrap();
    assert_eq!(
        ws.children(Parent::Root).unwrap(),
        vec![before, callout, after]
    );
    assert_eq!(ws.children(Parent::Node(callout)).unwrap(), vec![one, two]);
    let node = ws.read(callout).unwrap();
    assert_eq!(node.kind, koru_core::Kind::Callout);
    assert_eq!(node.content.as_text(), Some(""));
    assert!(ws.effective_schema_version() >= 5);

    ws.set_icon(callout, Some("⚠️")).unwrap();
    ws.set_color(callout, Some("red")).unwrap();
    let node = ws.read(callout).unwrap();
    assert_eq!(koru_core::callout::icon(&node.meta), "⚠️");
    assert_eq!(
        koru_core::callout::tint(&node.meta),
        Some(koru_core::AccentColor::Red)
    );

    let tail = ws.split(one, 4).unwrap();
    assert_eq!(
        ws.children(Parent::Node(callout)).unwrap(),
        vec![one, tail, two]
    );

    assert!(
        Node::new(
            koru_core::Kind::Callout,
            koru_core::node::Content::text("prose")
        )
        .is_err(),
        "a box carries no text of its own"
    );
    assert!(
        ws.wrap_in_callout_many(&[before, after]).is_err(),
        "rows with a gap between them are not one box"
    );

    let lines = ws.dissolve_callout(callout).unwrap();
    assert_eq!(lines, vec![one, tail, two]);
    assert_eq!(
        ws.children(Parent::Root).unwrap(),
        vec![before, one, tail, two, after]
    );
    assert!(ws.read(callout).is_err() || ws.kind_of(callout).is_none());
}
