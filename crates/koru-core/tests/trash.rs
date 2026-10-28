// SPDX-License-Identifier: MPL-2.0

//! The trash: binning a document, getting it back, and emptying the bin.
//!
//! The trash is a **place**, not a flag — a hidden `Kind::Trash` node at the
//! library root — so almost everything here is really a test that a `move_node`
//! is enough, and that the one central exclusion (`children(Parent::Root)`
//! omits trash roots) is enough to keep it out of sight everywhere else.
//!
//! The convergence half lives in `convergence.rs`, beside the other CRDT
//! stances: two peers binning at once, and one peer typing inside a document the
//! other has just thrown away.

use koru_core::node::{BlobHash, Kind, Node};
use koru_core::schema::{Parent, StoreError, WorkspaceDoc};
use koru_core::{NodeId, WorkspaceId};

fn ws() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "test").unwrap()
}

/// A document at the top of the library.
fn doc(ws: &mut WorkspaceDoc, title: &str) -> NodeId {
    ws.insert(Parent::Root, Node::document(title).unwrap())
        .unwrap()
}

/// A document nested under another.
fn sub(ws: &mut WorkspaceDoc, under: NodeId, title: &str) -> NodeId {
    ws.insert(Parent::Node(under), Node::document(title).unwrap())
        .unwrap()
}

/// The titles the library shows, top level only — what the explore column
/// would draw.
fn library(ws: &WorkspaceDoc) -> Vec<String> {
    ws.children(Parent::Root)
        .unwrap()
        .into_iter()
        .map(|id| ws.read(id).unwrap().content.as_text().unwrap_or("").into())
        .collect()
}

const T0: i64 = 1_700_000_000_000;

// --------------------------------------------------------------- the root

/// The load-bearing one for every *other* test in the tree: a workspace that has
/// never binned anything has an empty library, so every existing assertion about
/// library length still holds.
#[test]
fn the_trash_root_is_not_created_until_something_is_trashed() {
    let mut ws = ws();
    assert_eq!(ws.children(Parent::Root).unwrap().len(), 0);
    assert_eq!(ws.trash_len(), 0);

    let d = doc(&mut ws, "Spec");
    assert_eq!(ws.children(Parent::Root).unwrap().len(), 1);

    ws.trash(d, T0, None).unwrap();
    // One document went in; the root that now holds it is not a second row.
    assert_eq!(ws.children(Parent::Root).unwrap().len(), 0);
    assert_eq!(ws.trash_len(), 1);
}

/// The root is a real child of the library — it just never appears in the list
/// the rest of the app reads. If this stopped being true the filter would be
/// doing nothing and every trashed page would come back as a row.
#[test]
fn the_trash_root_is_a_real_node_that_children_hides() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, None).unwrap();

    assert!(ws.children(Parent::Root).unwrap().is_empty());
    // But the document is still reachable, still readable, still itself.
    assert_eq!(ws.read(d).unwrap().content.as_text(), Some("Spec"));
    assert!(ws.in_trash(d));
}

// ------------------------------------------------------------- trashing

#[test]
fn trashing_a_document_takes_its_subtree_and_keeps_its_id() {
    let mut ws = ws();
    let parent = doc(&mut ws, "Q3 Launch");
    let child = sub(&mut ws, parent, "Pricing");
    let grandchild = sub(&mut ws, child, "Tiers");

    ws.trash(parent, T0, None).unwrap();

    assert_eq!(library(&ws), Vec::<String>::new());
    for id in [parent, child, grandchild] {
        assert!(ws.in_trash(id), "{id:?} should be in the trash");
        assert!(ws.read(id).is_ok(), "{id:?} should still read");
    }
    // Only the thing a person binned is an item; what came with it is not.
    assert!(ws.is_trashed(parent));
    assert!(!ws.is_trashed(child));
    assert_eq!(ws.trash_len(), 1);
}

#[test]
fn a_trashed_document_leaves_the_library_listing() {
    let mut ws = ws();
    let a = doc(&mut ws, "A");
    let _b = doc(&mut ws, "B");
    let c = doc(&mut ws, "C");

    ws.trash(a, T0, None).unwrap();
    ws.trash(c, T0, None).unwrap();
    assert_eq!(library(&ws), vec!["B"]);
}

#[test]
fn trash_refuses_the_trash_root_and_anything_already_in_it() {
    let mut ws = ws();
    let parent = doc(&mut ws, "Q3");
    let child = sub(&mut ws, parent, "Pricing");
    ws.trash(parent, T0, None).unwrap();

    assert!(matches!(
        ws.trash(parent, T0, None),
        Err(StoreError::AlreadyInTrash(_))
    ));
    assert!(matches!(
        ws.trash(child, T0, None),
        Err(StoreError::AlreadyInTrash(_))
    ));
}

#[test]
fn trashing_records_when_and_who() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, Some("Sam")).unwrap();

    let entry = &ws.trashed().unwrap()[0];
    assert_eq!(entry.trashed_at, Some(T0));
    assert_eq!(entry.trashed_by.as_deref(), Some("Sam"));
    assert_eq!(ws.trashed_by(d).as_deref(), Some("Sam"));
}

/// A missing label is a normal state, not an error — an unnamed profile, or a
/// build that never wrote one. Every message built from it has a form with no
/// "by" clause, so `None` has to survive the round trip as `None`.
#[test]
fn a_trash_with_no_label_reads_back_as_none() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, None).unwrap();

    assert_eq!(ws.trashed_by(d), None);
    assert_eq!(ws.trashed().unwrap()[0].trashed_by, None);
}

// ------------------------------------------------------------- restoring

#[test]
fn restore_puts_it_back_under_its_original_parent() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");

    ws.trash(d, T0, None).unwrap();
    assert!(ws.children(Parent::Node(home)).unwrap().is_empty());

    ws.restore(d, None).unwrap();
    assert_eq!(ws.children(Parent::Node(home)).unwrap(), vec![d]);
    assert!(!ws.in_trash(d));
    assert_eq!(ws.trash_len(), 0);
}

#[test]
fn restore_replaces_the_trash_marks_with_who_restored_it() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, Some("Sam")).unwrap();
    ws.restore(d, Some("Ash")).unwrap();

    let meta = ws.read(d).unwrap().meta;
    assert_eq!(meta.trashed_at, None, "the timestamp is stale now");
    assert_eq!(meta.origin, None);
    assert_eq!(meta.trashed_by, None, "exactly one of the two is ever live");
    assert_eq!(meta.restored_by.as_deref(), Some("Ash"));
    assert_eq!(ws.restored_by(d).as_deref(), Some("Ash"));
}

/// Binning it again clears the restore label, so the pair can never both claim
/// to be current.
#[test]
fn trashing_again_clears_the_restore_label() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, Some("Sam")).unwrap();
    ws.restore(d, Some("Ash")).unwrap();
    ws.trash(d, T0 + 1, Some("Sam")).unwrap();

    let meta = ws.read(d).unwrap().meta;
    assert_eq!(meta.restored_by, None);
    assert_eq!(meta.trashed_by.as_deref(), Some("Sam"));
}

#[test]
fn restore_falls_back_to_the_library_when_the_origin_was_hard_deleted() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");
    ws.trash(d, T0, None).unwrap();
    ws.delete(home).unwrap();

    ws.restore(d, None).unwrap();
    assert_eq!(library(&ws), vec!["Spec"]);
}

#[test]
fn restore_falls_back_to_the_library_when_the_origin_is_itself_trashed() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");

    // Bin the child first, then its parent, then take only the child back:
    // restoring into a parent that is itself in the bin would leave it somewhere
    // still invisible, which reads as the button doing nothing.
    ws.trash(d, T0, None).unwrap();
    ws.trash(home, T0, None).unwrap();
    ws.restore(d, None).unwrap();

    assert_eq!(library(&ws), vec!["Spec"]);
    assert!(!ws.in_trash(d));
    assert!(ws.in_trash(home));
}

#[test]
fn restore_falls_back_when_the_origin_moved_inside_the_item() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");
    ws.trash(d, T0, None).unwrap();
    // A peer can do this while the item is away: the old parent is now a
    // descendant of the thing that is supposed to go back under it.
    ws.move_node(home, Parent::Node(d), 0).unwrap();

    ws.restore(d, None).unwrap();
    assert_eq!(library(&ws), vec!["Spec"]);
    assert_eq!(ws.children(Parent::Node(d)).unwrap(), vec![home]);
}

#[test]
fn restore_falls_back_when_the_origin_can_no_longer_adopt() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");
    ws.trash(d, T0, None).unwrap();
    // The parent became a picture while the item was in the bin.
    ws.set_image(home, BlobHash("fnv1a:1".into()), "image/png", None, None)
        .unwrap();

    ws.restore(d, None).unwrap();
    assert_eq!(library(&ws).len(), 2, "both are top level now");
    assert!(!ws.in_trash(d));
}

/// Deliberately **not** remembering an index. One is wrong the moment any
/// sibling moves, and "restore to position 3" landing arbitrarily is worse than
/// a consistent append.
#[test]
fn restore_appends_rather_than_remembering_an_index() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let first = sub(&mut ws, home, "First");
    let second = sub(&mut ws, home, "Second");
    let third = sub(&mut ws, home, "Third");

    ws.trash(first, T0, None).unwrap();
    ws.restore(first, None).unwrap();

    assert_eq!(
        ws.children(Parent::Node(home)).unwrap(),
        vec![second, third, first],
        "it goes to the end, not back to slot 0"
    );
}

#[test]
fn restore_refuses_something_that_is_not_a_trash_item() {
    let mut ws = ws();
    let parent = doc(&mut ws, "Q3");
    let child = sub(&mut ws, parent, "Pricing");
    let live = doc(&mut ws, "Live");
    ws.trash(parent, T0, None).unwrap();

    assert!(matches!(
        ws.restore(live, None),
        Err(StoreError::NotInTrash(_))
    ));
    assert!(
        matches!(ws.restore(child, None), Err(StoreError::NotInTrash(_))),
        "what came with an item is not separately restorable"
    );
}

// ------------------------------------------------------------- emptying

#[test]
fn emptying_the_trash_removes_the_items_and_leaves_the_root() {
    let mut ws = ws();
    let a = doc(&mut ws, "A");
    let b = doc(&mut ws, "B");
    ws.trash(a, T0, None).unwrap();
    ws.trash(b, T0, None).unwrap();

    assert_eq!(ws.empty_trash().unwrap(), 2);
    assert_eq!(ws.trash_len(), 0);
    assert!(ws.read(a).is_err(), "really gone this time");
    assert!(ws.read(b).is_err());
    // And the library is still empty as far as anything that draws it knows.
    assert!(ws.children(Parent::Root).unwrap().is_empty());
}

#[test]
fn emptying_an_empty_trash_is_nothing_rather_than_an_error() {
    let mut ws = ws();
    assert_eq!(ws.empty_trash().unwrap(), 0);
}

/// The guard that stops a mis-wired button from skipping the bin.
#[test]
fn delete_permanently_refuses_a_node_that_is_not_in_the_trash() {
    let mut ws = ws();
    let live = doc(&mut ws, "Live");
    let binned = doc(&mut ws, "Binned");
    ws.trash(binned, T0, None).unwrap();

    assert!(matches!(
        ws.delete_permanently(live),
        Err(StoreError::NotInTrash(_))
    ));
    assert!(ws.read(live).is_ok(), "and it really did not delete it");

    ws.delete_permanently(binned).unwrap();
    assert!(ws.read(binned).is_err());
}

// ---------------------------------------------------------------- counting

#[test]
fn nested_documents_counts_documents_not_paragraphs() {
    let mut ws = ws();
    let d = doc(&mut ws, "Q3 Launch");
    for line in ["intro", "body", "more", "and more", "still going"] {
        ws.insert(Parent::Node(d), Node::paragraph(line).unwrap())
            .unwrap();
    }
    assert_eq!(
        ws.nested_documents(d),
        0,
        "five paragraphs are not five items"
    );

    let child = sub(&mut ws, d, "Pricing");
    sub(&mut ws, child, "Tiers");
    assert_eq!(ws.nested_documents(d), 2, "a child and a grandchild");
    assert_eq!(ws.nested_documents(child), 1);
}

#[test]
fn the_listing_reports_what_the_screen_shows() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Q3 Launch");
    sub(&mut ws, d, "Pricing");
    ws.trash(d, T0, Some("Sam")).unwrap();

    let entries = ws.trashed().unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.node, d);
    assert_eq!(e.name, "Q3 Launch");
    assert_eq!(e.origin, Some(home));
    assert_eq!(e.nested, 1);
    assert_eq!(e.trashed_at, Some(T0));
    assert_eq!(e.trashed_by.as_deref(), Some("Sam"));
}

#[test]
fn the_listing_is_newest_first() {
    let mut ws = ws();
    let old = doc(&mut ws, "Old");
    let new = doc(&mut ws, "New");
    ws.trash(old, T0, None).unwrap();
    ws.trash(new, T0 + 86_400_000, None).unwrap();

    let names: Vec<String> = ws.trashed().unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["New", "Old"]);
}

// -------------------------------------------- the walk that sees the bin

/// A projection has to index binned content so a search can find it and offer
/// to put it back, which makes it the one reader that needs the answer `walk`
/// refuses to give. Both halves are pinned together on purpose: the new door
/// sees the bin, and the old one still does not.
#[test]
fn walk_with_trash_includes_the_binned_subtree() {
    let mut ws = ws();
    let kept = doc(&mut ws, "Kept");
    let kept_child = sub(&mut ws, kept, "Notes");
    let binned = doc(&mut ws, "Q3 Launch");
    let child = sub(&mut ws, binned, "Pricing");
    let grandchild = sub(&mut ws, child, "Tiers");

    let before = ws.walk().unwrap();
    assert_eq!(
        before,
        vec![
            (kept, 0),
            (kept_child, 1),
            (binned, 0),
            (child, 1),
            (grandchild, 2),
        ]
    );

    ws.trash(binned, T0, None).unwrap();

    // `walk` is unchanged: the binned subtree left it, and everything that
    // stayed in the library kept the depth it already had.
    let after = ws.walk().unwrap();
    assert_eq!(after, vec![(kept, 0), (kept_child, 1)]);
    assert_eq!(
        after,
        before
            .into_iter()
            .filter(|(id, _)| ![binned, child, grandchild].contains(id))
            .collect::<Vec<_>>(),
        "what walk still answers is exactly what it answered before, minus the bin"
    );

    // The trash-inclusive walk sees the bin's own root, the item, and
    // everything that went in with it — each one level deeper than it stood,
    // because the trash is a place rather than a flag.
    let all = ws.walk_with_trash().unwrap();
    let tops: Vec<NodeId> = all
        .iter()
        .filter(|(_, depth)| *depth == 0)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(tops.len(), 2, "the one live document and the bin's root");
    let trash_root = *tops.iter().find(|id| **id != kept).unwrap();
    assert_eq!(ws.read(trash_root).unwrap().kind, Kind::Trash);

    assert_eq!(
        all,
        vec![
            (kept, 0),
            (kept_child, 1),
            (trash_root, 0),
            (binned, 1),
            (child, 2),
            (grandchild, 3),
        ]
    );
}

/// Nothing to see is not a special case: with an empty bin — or none minted at
/// all — the trash-inclusive walk is the ordinary one.
#[test]
fn walk_with_trash_matches_walk_while_nothing_is_binned() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    let _ = sub(&mut ws, d, "Pricing");

    assert_eq!(ws.walk_with_trash().unwrap(), ws.walk().unwrap());
}

// ------------------------------------------------- what must NOT change

/// A trashed image's blob has to stay decodable, or Restore hands back a broken
/// picture. `blob_hashes` reads the tree directly rather than going through
/// `children`, so it is unaffected by the exclusion — **deliberately**, and this
/// pins it so nobody "fixes" the inconsistency.
#[test]
fn a_trashed_image_still_reports_its_blob() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.insert(
        Parent::Node(d),
        Node::image(BlobHash("fnv1a:abc".into()), "image/png", None, None).unwrap(),
    )
    .unwrap();
    assert!(ws.blob_hashes().unwrap().iter().any(|h| h == "fnv1a:abc"));

    ws.trash(d, T0, None).unwrap();
    assert!(
        ws.blob_hashes().unwrap().iter().any(|h| h == "fnv1a:abc"),
        "the bytes must survive as long as the picture can come back"
    );
}

/// A cleanup pass must not rewrite something the user has thrown away, or a
/// restore hands back a modified document.
#[test]
fn pruning_empty_list_blocks_does_not_reach_into_the_trash() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    let empty = ws
        .insert(Parent::Node(d), Node::list_block().unwrap())
        .unwrap();
    ws.trash(d, T0, None).unwrap();

    ws.prune_empty_containers().unwrap();
    assert!(ws.read(empty).is_ok(), "left exactly as it was binned");
}

/// The off-by-one guard. Indices into the library are in the *visible*
/// coordinate system; the trash root is a real sibling that Loro counts and
/// `children` does not. Without the translation in `move_node`, dragging in the
/// explore column would land one slot out whenever the root sorted first.
#[test]
fn moving_into_the_library_uses_the_visible_index() {
    let mut ws = ws();
    let binned = doc(&mut ws, "Binned");
    ws.trash(binned, T0, None).unwrap();
    // The trash root now sits at raw index 0.
    let a = doc(&mut ws, "A");
    let b = doc(&mut ws, "B");
    let c = doc(&mut ws, "C");
    assert_eq!(library(&ws), vec!["A", "B", "C"]);

    // "Put C first" means first among what a person can see.
    ws.move_node(c, Parent::Root, 0).unwrap();
    assert_eq!(library(&ws), vec!["C", "A", "B"]);

    // And the end still means the end.
    ws.move_node(c, Parent::Root, 2).unwrap();
    assert_eq!(library(&ws), vec!["A", "B", "C"]);

    assert_eq!(ws.sibling_index(a).unwrap(), 0);
    assert_eq!(ws.sibling_index(b).unwrap(), 1);
    assert_eq!(ws.sibling_index(c).unwrap(), 2);
}

// ------------------------------------------------------- durability, undo

#[test]
fn the_trash_survives_a_snapshot() {
    let mut ws = ws();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");
    ws.trash(d, T0, Some("Sam")).unwrap();
    let bytes = ws.export_snapshot().unwrap();

    let loaded = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    assert_eq!(loaded.trash_len(), 1);
    assert!(loaded.children(Parent::Root).unwrap().len() == 1); // "Projects"
    let e = &loaded.trashed().unwrap()[0];
    assert_eq!(e.node, d);
    assert_eq!(e.name, "Spec");
    assert_eq!(e.trashed_at, Some(T0));
    assert_eq!(e.trashed_by.as_deref(), Some("Sam"));
    assert_eq!(e.origin, Some(home));
}

#[test]
fn undo_of_a_trash_is_one_step() {
    let mut ws = ws();
    ws.enable_undo();
    let home = doc(&mut ws, "Projects");
    let d = sub(&mut ws, home, "Spec");
    ws.checkpoint();

    ws.trash(d, T0, Some("Sam")).unwrap();
    assert!(ws.in_trash(d));

    assert!(ws.undo().unwrap());
    assert!(!ws.in_trash(d), "one Ctrl+Z, not two");
    assert_eq!(ws.children(Parent::Node(home)).unwrap(), vec![d]);
    assert_eq!(ws.read(d).unwrap().meta.trashed_at, None);
    assert_eq!(ws.trash_len(), 0);
}

// ------------------------------------------------------------ save a copy

/// **A copy, not a move.** Somebody whose document was binned under them can
/// keep their version; the original stays exactly where the other peer put it.
#[test]
fn duplicate_makes_a_new_document_and_leaves_the_original_alone() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    let child = sub(&mut ws, d, "Pricing");
    ws.trash(d, T0, Some("Ana")).unwrap();

    let copy = ws.duplicate(d, Parent::Root, "Spec (my version)").unwrap();

    assert_ne!(copy, d, "a different node, not a second name for one");
    assert!(!ws.in_trash(copy), "and it is not in the bin");
    assert!(ws.in_trash(d), "which the original still is");
    assert_eq!(library(&ws), vec!["Spec (my version)"]);

    // The subtree came too, as copies.
    let kids = ws.children(Parent::Node(copy)).unwrap();
    assert_eq!(kids.len(), 1);
    assert_ne!(kids[0], child);
    assert_eq!(ws.read(kids[0]).unwrap().content.as_text(), Some("Pricing"));
}

/// A copy of something binned is not itself binned — the trash marks are the
/// one thing deliberately left behind.
#[test]
fn a_copy_does_not_inherit_the_trash_marks() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, Some("Ana")).unwrap();

    let copy = ws.duplicate(d, Parent::Root, "Mine").unwrap();
    let meta = ws.read(copy).unwrap().meta;
    assert_eq!(meta.trashed_at, None);
    assert_eq!(meta.trashed_by, None);
    assert_eq!(meta.origin, None);
    assert!(!ws.is_trashed(copy));
}

/// Formatting is what makes it *their version* rather than the plain text of
/// it, so a copy that dropped it would not be worth offering.
#[test]
fn a_copy_carries_the_inline_formatting() {
    use koru_core::Mark;
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    let line = ws
        .insert(Parent::Node(d), Node::paragraph("hello there").unwrap())
        .unwrap();
    ws.set_mark(line, 0..5, Mark::Bold, true).unwrap();

    let copy = ws.duplicate(d, Parent::Root, "Mine").unwrap();
    let copied_line = ws.children(Parent::Node(copy)).unwrap()[0];
    let runs = ws.runs(copied_line).unwrap();
    assert!(
        runs.iter().any(|r| r.bold && r.text.starts_with("hello")),
        "the bold run came across: {runs:?}"
    );
}

/// **The spelling language comes with the copy.** `prose_language` was
/// written by `set_prose_language` and read by `read_node` from the start, but
/// `write_node` — which `plant` goes through — never wrote it, so every copy
/// of a German page came out in the reader's default language, on every
/// build. Through a snapshot, so what is proved is storage.
#[test]
fn a_copy_keeps_its_prose_language() {
    let mut source = ws();
    let d = doc(&mut source, "Spezifikation");
    source.set_prose_language(d, Some("de-CH")).unwrap();

    let mut target = ws();
    let copy = target
        .plant(&source.cutting(d).unwrap(), Parent::Root, "")
        .unwrap();
    let target = WorkspaceDoc::from_snapshot(&target.export_snapshot().unwrap()).unwrap();
    assert_eq!(
        target.read(copy).unwrap().meta.prose_language.as_deref(),
        Some("de-CH")
    );

    let copy = source.duplicate(d, Parent::Root, "Kopie").unwrap();
    assert_eq!(
        source.read(copy).unwrap().meta.prose_language.as_deref(),
        Some("de-CH"),
        "and so does a duplicate in the same document"
    );
}

/// An empty title leaves the original's, so "save a copy" without renaming is
/// still a sensible thing to do.
#[test]
fn an_empty_title_keeps_the_original_name() {
    let mut ws = ws();
    let d = doc(&mut ws, "Spec");
    ws.trash(d, T0, None).unwrap();
    let copy = ws.duplicate(d, Parent::Root, "").unwrap();
    assert_eq!(ws.read(copy).unwrap().content.as_text(), Some("Spec"));
}

// ---------------------------------------------- a copy that crosses documents

/// **The copy that *Save a copy* actually makes now.**
///
/// A document is its own `LoroDoc` ([ADR-90]), so forking one is a read here and
/// a write in a document that does not exist yet — `duplicate` cannot express
/// that, because it borrows one document mutably for both halves. `cutting` and
/// `plant` are the two halves as values, and `duplicate` is now the case where
/// both ends happen to be the same document.
#[test]
fn a_cutting_plants_into_a_different_document() {
    use koru_core::Mark;

    let mut source = ws();
    let d = doc(&mut source, "Spec");
    let line = source
        .insert(Parent::Node(d), Node::paragraph("hello there").unwrap())
        .unwrap();
    source.set_mark(line, 0..5, Mark::Bold, true).unwrap();
    let nested = sub(&mut source, d, "Pricing");
    source.trash(d, T0, Some("Ana")).unwrap();

    // Cut, then plant somewhere the source document has never heard of.
    let cutting = source.cutting(d).unwrap();
    let mut target = ws();
    let copy = target
        .plant(&cutting, Parent::Root, "Spec (my version)")
        .unwrap();

    assert_eq!(library(&target), vec!["Spec (my version)"]);
    assert!(
        !target.is_trashed(copy),
        "a copy of something binned is not binned"
    );
    assert!(
        source.in_trash(d),
        "and the original is where the peer put it"
    );

    // **Every id is a new one.** Importing the source's snapshot would have been
    // the cheap answer and would have carried them across — two documents naming
    // the same nodes, and a search index keyed by `(profile, workspace, node)`
    // with two rows fighting over one key.
    assert_ne!(copy, d);
    let kids = target.children(Parent::Node(copy)).unwrap();
    let ids: Vec<_> = kids.clone();
    assert!(
        !ids.contains(&line) && !ids.contains(&nested),
        "an id came across"
    );

    // The shape, the text and the formatting did.
    let titles: Vec<_> = kids
        .iter()
        .map(|&k| {
            target
                .read(k)
                .unwrap()
                .content
                .as_text()
                .unwrap_or("")
                .to_owned()
        })
        .collect();
    assert_eq!(titles, vec!["hello there".to_owned(), "Pricing".to_owned()]);
    let runs = target.runs(kids[0]).unwrap();
    assert!(
        runs.iter().any(|r| r.bold && r.text.starts_with("hello")),
        "the bold run did not come across: {runs:?}"
    );
}

/// One cutting, planted twice, is two documents rather than one document named
/// twice. The property that makes a `Cutting` a value worth having.
#[test]
fn one_cutting_can_be_planted_more_than_once() {
    let mut source = ws();
    let d = doc(&mut source, "Spec");
    let cutting = source.cutting(d).unwrap();

    let mut target = ws();
    let first = target.plant(&cutting, Parent::Root, "One").unwrap();
    let second = target.plant(&cutting, Parent::Root, "Two").unwrap();
    assert_ne!(first, second);
    assert_eq!(library(&target), vec!["One", "Two"]);
}
