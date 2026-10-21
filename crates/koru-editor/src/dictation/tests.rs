// SPDX-License-Identifier: MPL-2.0

//! What a dictation into a document is allowed to do, and what it may not.
//!
//! The two properties everything else hangs off: **a composition open is not an
//! edit** — the document's Loro version vector does not move, however many
//! partials arrive — and **a commit is one logical edit and one undo step**.

use std::time::Instant;

use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Mark, NodeId, WorkspaceId};

use super::*;
use crate::{Editing, Editor};

/// The eight launch languages plus a mixed LTR/RTL line — the same fixture
/// shape `koru-ui`'s field suite and the store's suites use, so "it
/// round-trips" means one thing across the tree.
const SPOKEN: [&str; 9] = [
    "Draft the release notes",
    "Redactar las notas de la version",
    "Rascunhar as notas da versao",
    "Die Versionshinweise entwerfen",
    "Rediger les notes de version",
    "\u{62a}\u{62d}\u{631}\u{64a}\u{631} \u{645}\u{644}\u{627}\u{62d}\u{638}\u{627}\u{62a} \u{627}\u{644}\u{625}\u{635}\u{62f}\u{627}\u{631}",
    "\u{8d77}\u{8349}\u{53d1}\u{884c}\u{8bf4}\u{660e}",
    "\u{30ea}\u{30ea}\u{30fc}\u{30b9}\u{30ce}\u{30fc}\u{30c8}\u{3092}\u{4e0b}\u{66f8}\u{304d}\u{3059}\u{308b}",
    // Mixed LTR/RTL on one line, which is where a byte range and a visual run
    // stop being the same shape.
    "Meeting \u{645}\u{639} \u{627}\u{644}\u{641}\u{631}\u{64a}\u{642} at 14:00",
];

/// A workspace holding one paragraph, and a view over it. Nothing touches disk
/// and nothing reaches a store.
fn one_row(text: &str) -> (WorkspaceDoc, NodeId, Editor) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let id = ws
        .insert(Parent::Root, Node::paragraph(text).unwrap())
        .unwrap();
    // **After** the row exists, the way `prepare` turns undo on when a document
    // is opened -- so the only steps in the history are the ones a test makes,
    // and "exactly one undo step" can be asserted by exhausting it.
    ws.enable_undo();
    let editor = Editor::over(&ws);
    (ws, id, editor)
}

/// Put the caret at a byte offset on the one row, the way a click would.
fn caret_at(editor: &mut Editor, at: usize) {
    let row = editor.body.focused_mut().unwrap();
    row.select_range_now(at, at);
}

fn select(editor: &mut Editor, from: usize, to: usize) {
    let row = editor.body.focused_mut().unwrap();
    row.select_range_now(from, to);
}

/// What the row is showing — authored text with the transcript spliced in.
fn shown(editor: &Editor) -> String {
    editor.body.rows[0].text.clone()
}

/// What the document says.
fn stored(ws: &WorkspaceDoc, id: NodeId) -> String {
    ws.read(id).unwrap().content.as_text().unwrap().to_owned()
}

/// The provisional run the row is carrying, if any.
fn run(editor: &Editor) -> Option<ProvisionalRun> {
    editor.body.rows[0].composition.clone()
}

// ---------------------------------------------------------------------------
// A composition is not an edit
// ---------------------------------------------------------------------------

/// **The property this whole design exists for.** A recogniser revises itself
/// several times a second; if any of that reached Loro it would be undo
/// history, sync traffic and collaborative churn for words nobody has finished
/// saying.
#[test]
fn a_hundred_partials_produce_no_operations() {
    let (mut ws, id, mut editor) = one_row("Before");
    caret_at(&mut editor, 6);
    let before = ws.version();

    assert!(editor.begin_dictation(&mut ws));
    for i in 1..=100 {
        editor.update_dictation(&mut ws, &format!(" word {i}"));
    }
    editor.reflow();

    assert_eq!(
        ws.version(),
        before,
        "an open composition moved the document's version vector"
    );
    assert_eq!(
        stored(&ws, id),
        "Before",
        "the document never saw a partial"
    );
    assert_eq!(shown(&editor), "Before word 100", "but the row shows it");
}

/// Every revision replaces the provisional run, and the caret follows its
/// trailing edge. Keeping the authored selection here makes streaming text
/// grow in front of a caret stranded at the start of the utterance.
#[test]
fn the_caret_follows_each_streaming_partial() {
    let (mut ws, _, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));

    for partial in ["w", "words", "words arrive"] {
        editor.update_dictation(&mut ws, partial);
        editor.reflow();
        let row = editor.body.focused().unwrap();
        assert_eq!(row.text, format!("Keep {partial} away"));
        assert_eq!(row.caret(), 5 + partial.len(), "after {partial:?}");
        assert!(row.selection.is_collapsed(), "after {partial:?}");
    }
}

/// Cancel is the interruption path — window closed, focus lost, machine slept.
/// It must leave nothing behind at all.
#[test]
fn cancel_leaves_the_document_and_the_row_byte_identical() {
    let (mut ws, id, mut editor) = one_row("Before");
    caret_at(&mut editor, 6);
    let before = ws.version();

    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, " half a sentence");
    editor.cancel_dictation(&mut ws);

    assert_eq!(ws.version(), before);
    assert_eq!(stored(&ws, id), "Before");
    assert_eq!(shown(&editor), "Before");
    assert!(editor.body.composition().is_none());
    assert!(run(&editor).is_none());
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

/// One dictation is one Ctrl+Z. It is deliberately *not* one Loro transaction
/// and not one sync message — `replace_text` wraps `atomic`, which groups undo,
/// and the delete and the insert inside it each commit.
#[test]
fn commit_is_exactly_one_undo_step() {
    let (mut ws, id, mut editor) = one_row("Before");
    caret_at(&mut editor, 6);

    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, " part");
    editor.commit_dictation(&mut ws, " and the rest");

    assert_eq!(stored(&ws, id), "Before and the rest");
    assert!(ws.undo().unwrap(), "one undo");
    assert_eq!(
        stored(&ws, id),
        "Before",
        "and it restored the whole dictation at once"
    );
    assert!(!ws.undo().unwrap_or(false), "there was no second step");
}

/// Dictation replaces a selection exactly as typing does, and undo brings the
/// replaced text back.
#[test]
fn a_composition_over_a_selection_replaces_it() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);

    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");
    assert_eq!(shown(&editor), "Keep that away");
    assert_eq!(stored(&ws, id), "Keep this away", "still nothing written");

    editor.commit_dictation(&mut ws, "that");
    assert_eq!(stored(&ws, id), "Keep that away");
    assert!(ws.undo().unwrap());
    assert_eq!(stored(&ws, id), "Keep this away");
}

/// **A dictation that produced no text records no step at all** — the rule a
/// backspace at offset zero already follows. The alternative would let saying
/// nothing over a selection delete it, which is not what anybody asked for.
#[test]
fn an_empty_final_transcript_records_nothing() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    let before = ws.version();

    assert!(editor.begin_dictation(&mut ws));
    editor.commit_dictation(&mut ws, "");

    assert_eq!(ws.version(), before, "no operation");
    assert_eq!(stored(&ws, id), "Keep this away");
    assert!(editor.body.composition().is_none(), "and it is closed");
}

/// The caret lands after the words, which is where the next sentence goes.
#[test]
fn commit_leaves_the_caret_after_the_transcript() {
    let (mut ws, _, mut editor) = one_row("Before");
    caret_at(&mut editor, 6);
    assert!(editor.begin_dictation(&mut ws));
    editor.commit_dictation(&mut ws, " and after");
    editor.reflow();
    assert_eq!(editor.body.rows[0].caret(), "Before and after".len());
}

/// Nine strings, the eight launch languages and one mixed LTR/RTL line, through
/// begin, a half-length partial, commit and undo — byte-exact both ways.
#[test]
fn eight_languages_round_trip_through_a_commit() {
    for spoken in SPOKEN {
        let (mut ws, id, mut editor) = one_row("");
        caret_at(&mut editor, 0);
        assert!(editor.begin_dictation(&mut ws));
        let half: String = spoken.chars().take(spoken.chars().count() / 2).collect();
        editor.update_dictation(&mut ws, &half);
        assert_eq!(shown(&editor), half, "{spoken}");
        editor.commit_dictation(&mut ws, spoken);
        assert_eq!(stored(&ws, id), spoken, "{spoken}");
        assert!(ws.undo().unwrap());
        assert_eq!(stored(&ws, id), "", "{spoken}");
    }
}

// ---------------------------------------------------------------------------
// The coordinate mapping
// ---------------------------------------------------------------------------

/// With nothing being composed the two are the identity, for every byte of
/// every fixture. This is the case every caret, hit test and pill anchor is in
/// almost all of the time.
#[test]
fn with_no_composition_the_mapping_is_the_identity() {
    for spoken in SPOKEN {
        let (_, _, editor) = one_row(spoken);
        let row = &editor.body.rows[0];
        for i in 0..=spoken.len() {
            assert_eq!(row.doc_pos(i), Mapped::Authored(i), "{spoken} @{i}");
            assert_eq!(row.layout_pos(i), Mapped::Authored(i), "{spoken} @{i}");
        }
    }
}

/// **They are not two total inverses**, so the round trip is asserted only over
/// the domains where an inverse exists: every authored byte outside the
/// replaced range, and every layout byte outside the interior of the
/// transcript. Rev 2 asked for the round trip over both complete domains, which
/// is asking for something that cannot exist.
#[test]
fn the_mapping_inverts_over_the_domains_where_an_inverse_exists() {
    for spoken in SPOKEN {
        for other in SPOKEN {
            let (mut ws, _, mut editor) = one_row(spoken);
            // Replace the middle third, on character boundaries.
            let chars: Vec<usize> = spoken
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(spoken.len()))
                .collect();
            let (from, to) = (chars[chars.len() / 3], chars[2 * chars.len() / 3]);
            select(&mut editor, from, to);
            assert!(editor.begin_dictation(&mut ws));
            editor.update_dictation(&mut ws, other);

            let row = &editor.body.rows[0];
            let provisional = row.composition.clone().unwrap();
            let span = provisional.layout_range();
            assert_eq!(provisional.authored, from..to);

            // Authored -> layout -> authored, outside the hidden interior.
            for doc in (0..=spoken.len()).filter(|d| *d <= from || *d >= to) {
                let Mapped::Authored(layout) = row.layout_pos(doc) else {
                    panic!("{spoken}/{other}: authored {doc} was not mapped");
                };
                assert_eq!(
                    row.doc_pos(layout),
                    Mapped::Authored(doc),
                    "{spoken}/{other}"
                );
            }
            // Layout -> authored -> layout, outside the transcript's interior.
            for layout in (0..=row.text.len()).filter(|l| *l <= span.start || *l >= span.end) {
                let Mapped::Authored(doc) = row.doc_pos(layout) else {
                    panic!("{spoken}/{other}: layout {layout} was not mapped");
                };
                assert_eq!(
                    row.layout_pos(doc),
                    Mapped::Authored(layout),
                    "{spoken}/{other}"
                );
            }
        }
    }
}

/// A position strictly inside the transcript has no authored counterpart, and
/// says so — with the offset *within the transcript*, so a caller placing
/// something against what was heard still can.
#[test]
fn a_position_inside_the_transcript_is_provisional() {
    let (mut ws, _, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "spoken");

    let row = &editor.body.rows[0];
    assert_eq!(shown(&editor), "Keep spoken away");
    for inside in 1.."spoken".len() {
        assert_eq!(row.doc_pos(5 + inside), Mapped::Provisional(inside));
    }
    // The two edges are authored: they are exactly the edges of the range the
    // transcript replaced, and the far one is where the caret sits.
    assert_eq!(row.doc_pos(5), Mapped::Authored(5));
    assert_eq!(row.doc_pos(5 + "spoken".len()), Mapped::Authored(9));
}

/// A position strictly inside the replaced range is hidden, and is reported at
/// the nearer boundary **with an affinity** rather than silently rounded — so a
/// caller can tell the front of the replaced range from the back rather than
/// finding two authored bytes reporting one number with nothing to separate
/// them.
#[test]
fn a_position_inside_the_replaced_range_is_hidden_at_a_boundary() {
    let (mut ws, _, mut editor) = one_row("Keep abcdefgh away");
    select(&mut editor, 5, 13);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "xy");

    let row = &editor.body.rows[0];
    assert_eq!(shown(&editor), "Keep xy away");
    // 5..13 replaced by two bytes at 5..7. The near half rounds back to 5.
    for doc in 6..=9 {
        assert_eq!(
            row.layout_pos(doc),
            Mapped::Hidden {
                boundary: 5,
                affinity: Affinity::Leading
            },
            "authored {doc}"
        );
    }
    for doc in 10..13 {
        assert_eq!(
            row.layout_pos(doc),
            Mapped::Hidden {
                boundary: 7,
                affinity: Affinity::Trailing
            },
            "authored {doc}"
        );
    }
    assert_eq!(row.layout_pos(5), Mapped::Authored(5));
    assert_eq!(row.layout_pos(13), Mapped::Authored(7));
    assert_eq!(row.layout_pos(18), Mapped::Authored(12));
}

// ---------------------------------------------------------------------------
// Remote edits
// ---------------------------------------------------------------------------

/// A second peer over the same document, and the import path the view really
/// takes: `defer_pending` drains a Loro *subscription*, so the update is
/// already applied by the time the view hears of it.
fn peer(ws: &WorkspaceDoc) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap()
}

fn receive(ws: &mut WorkspaceDoc, from: &WorkspaceDoc, editor: &mut Editor) {
    ws.import(&from.export_all_updates().unwrap()).unwrap();
    editor.body.defer_pending(ws);
    editor.body.catch_up(ws);
}

/// The whole point of anchoring with two Loro cursors: a peer typing *before*
/// the selection moves the bytes, and the composition still covers the words it
/// meant to replace.
#[test]
fn a_remote_edit_before_the_range_leaves_the_composition_over_the_same_bytes() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");

    let mut other = peer(&ws);
    other.insert_text(id, 0, "Please ").unwrap();
    receive(&mut ws, &other, &mut editor);

    assert_eq!(run(&editor).unwrap().authored, 12..16);
    assert_eq!(shown(&editor), "Please Keep that away");
    editor.commit_dictation(&mut ws, "that");
    assert_eq!(stored(&ws, id), "Please Keep that away");
}

/// A peer editing *inside* the selection. The endpoints still resolve, so the
/// composition continues against the converged document and replaces whatever
/// now lies between them — which is the only answer that does not silently
/// discard somebody's words or somebody else's.
#[test]
fn a_remote_edit_inside_the_range_is_carried_along() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");

    let mut other = peer(&ws);
    other.insert_text(id, 7, "XX").unwrap();
    receive(&mut ws, &other, &mut editor);

    let authored = run(&editor).unwrap().authored;
    assert_eq!(authored, 5..11, "the range grew with the text inside it");
    assert_eq!(stored(&ws, id), "Keep thXXis away");
    assert_eq!(shown(&editor), "Keep that away");
    editor.commit_dictation(&mut ws, "that");
    assert_eq!(stored(&ws, id), "Keep that away");
}

/// A peer typing *after* the selection changes nothing about it.
#[test]
fn a_remote_edit_after_the_range_leaves_the_composition_alone() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");

    let mut other = peer(&ws);
    other.insert_text(id, 14, " now").unwrap();
    receive(&mut ws, &other, &mut editor);

    assert_eq!(run(&editor).unwrap().authored, 5..9);
    assert_eq!(shown(&editor), "Keep that away now");
    editor.commit_dictation(&mut ws, "that");
    assert_eq!(stored(&ws, id), "Keep that away now");
}

/// **Scoped to the composed node.** An edit somewhere else in the workspace is
/// not this dictation's business.
#[test]
fn a_remote_edit_elsewhere_does_not_disturb_the_composition() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    let sibling = ws
        .insert(Parent::Root, Node::paragraph("Another line").unwrap())
        .unwrap();
    editor.body.rebuild(&ws);
    editor.body.focus_on(id);
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");

    let mut other = peer(&ws);
    other.insert_text(sibling, 0, "Prefixed ").unwrap();
    receive(&mut ws, &other, &mut editor);

    assert_eq!(run(&editor).unwrap().authored, 5..9);
    assert_eq!(editor.body.row(id).unwrap().text, "Keep that away");
}

/// A node deleted underneath a dictation discards it. There is nowhere for the
/// words to go that is not somebody else's text.
#[test]
fn a_deleted_node_discards_the_composition() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    ws.insert(Parent::Root, Node::paragraph("Another line").unwrap())
        .unwrap();
    editor.body.rebuild(&ws);
    editor.body.focus_on(id);
    select(&mut editor, 5, 9);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "that");

    let mut other = peer(&ws);
    other.delete(id).unwrap();
    receive(&mut ws, &other, &mut editor);

    assert!(editor.body.composition().is_none(), "discarded");
    // And a commit that arrives afterwards writes nothing anywhere.
    let after = ws.version();
    editor.commit_dictation(&mut ws, "that");
    assert_eq!(ws.version(), after);
}

/// `StableTextRange` deliberately does not normalise direction, and neither
/// does this: a selection dragged right-to-left is still backwards after a peer
/// edit and a resolve, so returning focus to it does not flip which end grows.
#[test]
fn a_backwards_selection_is_still_backwards_after_a_remote_edit() {
    let (mut ws, id, mut editor) = one_row("Keep this away");
    // Anchor at 9, focus at 5: dragged leftwards.
    select(&mut editor, 9, 5);
    assert_eq!(editor.body.rows[0].directed_range(), Some((9, 5)));
    assert!(editor.begin_dictation(&mut ws));

    let mut other = peer(&ws);
    other.insert_text(id, 0, "Please ").unwrap();
    receive(&mut ws, &other, &mut editor);

    let open = editor.body.composition().unwrap();
    assert_eq!(open.resolve(&ws), Some((16, 12)), "backwards, and moved");
    assert_eq!(open.range(&ws), Some(12..16), "normalised only to replace");
}

// ---------------------------------------------------------------------------
// Shapes that are easy to get wrong
// ---------------------------------------------------------------------------

/// A collapsed range is an insertion at a caret and needs no second case.
#[test]
fn a_collapsed_range_is_an_insertion() {
    let (mut ws, id, mut editor) = one_row("ab");
    caret_at(&mut editor, 1);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "XYZ");
    assert_eq!(run(&editor).unwrap().authored, 1..1);
    assert_eq!(shown(&editor), "aXYZb");
    editor.commit_dictation(&mut ws, "XYZ");
    assert_eq!(stored(&ws, id), "aXYZb");
}

/// A composition over a multi-codepoint character. `replace_text` validates
/// character boundaries before the first write, so this is about the splice
/// agreeing with it.
#[test]
fn a_composition_at_an_emoji_boundary() {
    let (mut ws, id, mut editor) = one_row("a\u{1f600}b");
    select(&mut editor, 1, 5);
    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "smile");
    assert_eq!(shown(&editor), "asmileb");
    editor.commit_dictation(&mut ws, "smile");
    assert_eq!(stored(&ws, id), "asmileb");
}

/// A table row's canonical content is its Markdown source, so a dictation into
/// a cell is a dictation into that source. The derived grid is rebuilt from the
/// composed text like any other layout.
#[test]
fn a_composition_inside_a_table_cell() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    ws.enable_undo();
    let source = "| a | b |\n| --- | --- |\n| one | two |";
    let id = ws
        .insert(Parent::Root, Node::table(source).unwrap())
        .unwrap();
    let mut editor = Editor::over(&ws);
    let cell = source.find("one").unwrap();
    select(&mut editor, cell, cell + 3);

    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "uno");
    assert_eq!(shown(&editor), source.replace("one", "uno"));
    assert_eq!(stored(&ws, id), source, "nothing written yet");
    editor.commit_dictation(&mut ws, "uno");
    assert_eq!(stored(&ws, id), source.replace("one", "uno"));
}

/// Formatting either side of the transcript keeps its bytes; a mark that
/// straddles the replaced range is clipped to the parts that survive. The
/// document is untouched throughout — these are layout runs, not marks.
#[test]
fn marks_are_re_ranged_across_the_splice() {
    let (mut ws, id, mut editor) = one_row("one two three");
    ws.set_mark(id, 0..7, Mark::Bold, true).unwrap();
    editor.body.refresh(&ws, id);
    select(&mut editor, 4, 7);

    assert!(editor.begin_dictation(&mut ws));
    editor.update_dictation(&mut ws, "second");
    assert_eq!(shown(&editor), "one second three");

    let bold: Vec<_> = editor.body.rows[0]
        .runs
        .iter()
        .filter(|r| r.bold)
        .map(|r| r.range())
        .collect();
    assert_eq!(bold, vec![0..4], "clipped to what is still authored");
}

/// A pill anchor inside the replaced range is dropped for the life of the
/// composition: it is one of the bytes being replaced, so there is nothing for
/// it to hang on, and at commit it is gone. One outside is only shifted.
#[test]
fn a_pill_anchor_inside_the_range_is_dropped_and_one_outside_is_shifted() {
    use koru_core::AttrId;
    use koru_core::text::{TextRun, Utf8Pos};

    let anchor = |start: usize, end: usize, pill: Option<AttrId>| TextRun {
        start: Utf8Pos(start),
        end: Utf8Pos(end),
        text: "x".repeat(end - start),
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        code: false,
        style: Default::default(),
        pill,
    };
    let inside = AttrId::new();
    let outside = AttrId::new();
    let runs = vec![
        anchor(0, 2, Some(inside)),
        anchor(2, 6, None),
        anchor(6, 8, Some(outside)),
    ];
    let spliced = splice_runs(&runs, &(0..3), 10);
    assert_eq!(
        spliced.iter().filter_map(|r| r.pill).collect::<Vec<_>>(),
        vec![outside],
        "the anchor inside the hole went with it"
    );
    assert_eq!(
        spliced.last().unwrap().range(),
        13..15,
        "and one after moved"
    );
}

/// A read-only view — which is what a trashed document is — refuses outright,
/// so nothing above has to remember to ask.
#[test]
fn a_read_only_view_refuses() {
    let (mut ws, _, mut editor) = one_row("Before");
    editor.view.read_only = true;
    caret_at(&mut editor, 6);
    assert!(!editor.begin_dictation(&mut ws));
    assert!(editor.body.composition().is_none());
}

/// A second `begin` while one is open is refused rather than silently replacing
/// the first, because the caller that lost track of it is the bug.
#[test]
fn a_second_begin_is_refused() {
    let (mut ws, _, mut editor) = one_row("Before");
    caret_at(&mut editor, 6);
    assert!(editor.begin_dictation(&mut ws));
    assert!(!editor.begin_dictation(&mut ws));
}

/// The title row is an ordinary row for typing, so it is an ordinary row for
/// dictation. `Body::is_title_row` guards *structure*, and nothing here is
/// structural.
#[test]
fn the_title_row_is_not_a_barrier() {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    ws.enable_undo();
    let doc = ws
        .insert(Parent::Root, Node::document("Untitled").unwrap())
        .unwrap();
    ws.insert(Parent::Node(doc), Node::paragraph("body").unwrap())
        .unwrap();
    let mut editor = Editor::rooted_at(&ws, doc, true);
    editor.body.title_row = true;
    editor.body.rebuild(&ws);
    editor.body.focus = 0;
    // A selection is derived from the row's parley layout, so the rows have to
    // have been laid out before one can be placed.
    editor.reflow();
    assert!(editor.body.is_title_row(0));
    select(&mut editor, 0, "Untitled".len());

    assert!(editor.begin_dictation(&mut ws));
    editor.commit_dictation(&mut ws, "Release plan");
    assert_eq!(stored(&ws, doc), "Release plan");
}

/// The transcript's rectangle is the **union** of its pieces, in view space. A
/// bidi line is where a byte range and a visual run stop being the same shape,
/// which is why one rect is a union rather than the first thing parley hands
/// back.
#[test]
fn the_composition_rect_covers_the_transcript() {
    let (mut ws, _, mut editor) = one_row("Meeting at 14:00");
    caret_at(&mut editor, 8);
    assert!(editor.begin_dictation(&mut ws));
    editor.reflow();
    assert!(
        rect(&editor.body, &editor.view).is_none(),
        "nothing said yet covers no glyphs"
    );

    editor.update_dictation(
        &mut ws,
        "\u{645}\u{639} \u{627}\u{644}\u{641}\u{631}\u{64a}\u{642} ",
    );
    editor.reflow();
    let r = rect(&editor.body, &editor.view).expect("a transcript has a rectangle");
    assert!(r.w > 0.0 && r.h > 0.0, "{r:?}");
}

// ---------------------------------------------------------------------------
// Speed
// ---------------------------------------------------------------------------

/// **A partial costs no more than a keystroke** — measured (the median of each),
/// not asserted in milliseconds, because a millisecond budget passes on a fast machine and gets
/// relaxed until it asserts nothing.
///
/// Typing is not flat: `rowlayout::reflow` calls `position_rows` twice and
/// `position_rows` walks every row, which is pre-existing and belongs to a
/// TECHDEBT entry rather than to this feature. So the criterion is the *ratio*
/// against typing into the same document, which pays exactly the same tax.
#[test]
fn a_partial_costs_no_more_than_a_keystroke() {
    const ROWS: usize = 2_000;
    const ATTEMPTS: usize = 3;
    // The best of a few whole measurements. The two phases run one after the
    // other, so load that arrives between them - another test thread on a
    // four-core runner - lands on one side only: a single loaded run read
    // 516 us against 130 us on the medians, where every other read about 1.2
    // times. A partial that really costs more than two keystrokes does so in
    // every attempt; a busy machine does not stay busy for all of them.
    let mut attempts = Vec::with_capacity(ATTEMPTS);
    for _ in 0..ATTEMPTS {
        let (typing, dictating) = keystroke_and_partial(ROWS);
        // Printed rather than only asserted, so `-- --nocapture` says what
        // the machine actually did rather than only that it was in a ratio.
        println!("{ROWS} rows: keystroke {typing}us, partial {dictating}us");
        if dictating <= typing * 2 {
            return;
        }
        attempts.push((typing, dictating));
    }
    panic!("a partial cost more than two keystrokes at {ROWS} rows in every attempt: {attempts:?}");
}

/// One measurement: the median keystroke and the median partial, in
/// microseconds, over the same freshly built document.
fn keystroke_and_partial(rows: usize) -> (u128, u128) {
    const SAMPLES: usize = 20;
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    ws.enable_undo();
    let mut first = None;
    for i in 0..rows {
        let id = ws
            .insert(Parent::Root, Node::paragraph(format!("row {i}")).unwrap())
            .unwrap();
        first.get_or_insert(id);
    }
    let first = first.unwrap();
    let mut editor = Editor::over(&ws);
    editor.body.focus_on(first);
    editor.reflow();

    // A keystroke: the row writes one character and re-reads itself, then the
    // frame reflows. Exactly what `Paragraph::insert_text` does, which is the
    // whole of what typing costs.
    let mut typing = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        editor.body.focused_mut().unwrap().insert_text(&mut ws, "x");
        editor.reflow();
        typing.push(start.elapsed().as_micros());
    }

    editor.body.focus_on(first);
    caret_at(&mut editor, 0);
    assert!(editor.begin_dictation(&mut ws));
    let mut dictating = Vec::with_capacity(SAMPLES);
    for i in 1..=SAMPLES {
        let partial = "word ".repeat(i);
        let start = Instant::now();
        editor.update_dictation(&mut ws, &partial);
        editor.reflow();
        dictating.push(start.elapsed().as_micros());
    }
    (median(typing), median(dictating))
}

/// The middle sample. A mean of twenty ~150 us samples moves by hundreds of
/// microseconds when one of them loses its time slice to another test thread,
/// and on a four-core runner one does: 400 us against 190 us in one loaded run.
/// One interrupted sample cannot move a median.
fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}
