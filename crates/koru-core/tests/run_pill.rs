// SPDX-License-Identifier: MPL-2.0

//! **The delegation pill's existing-reference path, verified.**
//!
//! The canonical data model (`docs/architecture/DATAMODEL.md`) expects the
//! pill to ride `Attr { role, value: AttrValue::Document(..) }` under the
//! existing `"ref"` storage token, so that an older reader preserves it and no
//! content-schema bump (`schema::SCHEMA_VERSION`, 4) is needed. These tests
//! are that expectation checked against the code rather than assumed, across
//! the four cases the roadmap names: a text anchor, a whole-node blob
//! attribute, a target that is a Run document, and — in
//! `koru-export-markdown` — export.
//!
//! **Two readers live in this file.** Since P1.15 this build normalizes
//! `koru.run` like `document` (`attributes::DOCUMENT_REFERENCE_ROLES`), so it
//! hands a run pill back as `AttrValue::Document`. The *older* reader — every
//! shipped build, and this one before the widening — knows no `koru.run` role
//! and hands the same record back as `AttrValue::Ref` homed at the run. The
//! bytes are identical under both, which is the property these tests exist to
//! hold: the widening is a change to what the reader *answers*, never to what
//! is written.

use koru_core::node::{Attr, AttrValue, BlobHash, Node};
use koru_core::schema::{Compatibility, Parent, SCHEMA_VERSION, WorkspaceDoc};
use koru_core::{DocRef, DocumentId, DocumentReference, NodeId, WorkspaceId};

/// The role the proposal reserves for the delegation pill. Roles are open
/// text (`Attr.role: String`); the Koru-owned ones are spelled `koru.<name>`
/// (`attributes::KEY_DUE`, `KEY_TAG`, …).
///
/// Spelled out here rather than taken from `attributes::ROLE_RUN`, because
/// this file's job is to hold the *stored* name still: a test that read the
/// constant would follow a rename that must never happen.
const ROLE_RUN: &str = "koru.run";

fn a_document() -> (WorkspaceDoc, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let root = ws
        .insert(Parent::Root, Node::document("Plan").unwrap())
        .unwrap();
    (ws, root)
}

fn run_pill(run: DocumentId) -> Attr {
    Attr::new(ROLE_RUN, AttrValue::Document(DocumentReference::new(run)))
}

fn reopened(ws: &WorkspaceDoc) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap()
}

fn only_attr(ws: &WorkspaceDoc, node: NodeId) -> Attr {
    let read = ws.read(node).unwrap();
    let attrs = read.content.attrs();
    assert_eq!(attrs.len(), 1, "exactly one attribute: {attrs:?}");
    attrs[0].clone()
}

/// What **this** build reads back for a `koru.run` document reference — in
/// the writing process as much as after a snapshot, because `read` always
/// decodes the stored record: the typed `AttrValue::Document`, because
/// `koru.run` is in `attributes::DOCUMENT_REFERENCE_ROLES` (P1.15, proposal
/// §3.3 option 2).
fn assert_reads_as_the_run_document(attr: &Attr, run: DocumentId) {
    assert_eq!(attr.role, ROLE_RUN);
    assert_eq!(
        attr.as_document_reference(),
        Some(DocumentReference::new(run)),
        "a run pill read back as {:?}",
        attr.value
    );
}

/// A role this build does **not** normalize, standing in for the older
/// reader. Its record is written by exactly the same encoder as a run pill's
/// — `AttrValue::Document` under the `"ref"` token — so reading one is
/// reading a run pill through the branch a shipped build takes.
const ROLE_AN_OLDER_BUILD_DOES_NOT_KNOW: &str = "koru.notyetinvented";

// ------------------------------------------------------------------ text anchor

/// A pill anchored in a paragraph: written fresh, the attribute is the typed
/// document reference; through a snapshot and back it is preserved under the
/// same `ref` token, still anchored, and re-exports byte for byte.
#[test]
fn a_run_pill_in_text_survives_a_snapshot_and_reexports_byte_identical() {
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("Delegated: ").unwrap())
        .unwrap();
    let run = DocumentId::new();
    let attr = run_pill(run);
    ws.insert_pill(line, "Delegated: ".len(), &attr).unwrap();

    // What was written is the typed reference under the existing token...
    assert_eq!(attr.value.key(), "ref", "the existing storage token");
    // ...and what is read back, even before any snapshot, is the finding.
    let fresh = only_attr(&ws, line);
    assert_reads_as_the_run_document(&fresh, run);

    let bytes = ws.export_snapshot().unwrap();
    let older = WorkspaceDoc::from_snapshot(&bytes).unwrap();

    // The anchor is still a pill drawing this attribute.
    let runs = older.runs(line).unwrap();
    let anchor = runs
        .iter()
        .find(|run| run.is_pill())
        .expect("the anchor survives");
    assert_eq!(anchor.pill, Some(attr.id));

    let read = only_attr(&older, line);
    assert_reads_as_the_run_document(&read, run);
    assert_eq!(read.id, attr.id);

    // Reading performed no write, so the older reader hands the bytes back
    // exactly as it found them.
    assert_eq!(older.export_snapshot().unwrap(), bytes);
    // And an older reader's own edit elsewhere leaves the record intact.
    let mut writer = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    writer
        .insert(
            Parent::Node(root),
            Node::paragraph("An older build typed this").unwrap(),
        )
        .unwrap();
    let after = reopened(&writer);
    assert_reads_as_the_run_document(&only_attr(&after, line), run);
    assert_eq!(
        after
            .runs(line)
            .unwrap()
            .iter()
            .filter(|r| r.is_pill())
            .count(),
        1
    );
}

// ------------------------------------------------------------------ blob node

/// A whole-node attribute on a blob node — a file delegated by right-click —
/// where there is no text to anchor in: the same record on the node's meta,
/// preserved the same way.
#[test]
fn a_run_pill_on_a_blob_node_survives_a_snapshot() {
    let (mut ws, root) = a_document();
    let file = ws
        .insert(
            Parent::Node(root),
            Node::file(
                "brief.pdf",
                BlobHash::new("b3", "abc123"),
                "application/pdf",
            )
            .unwrap(),
        )
        .unwrap();
    let run = DocumentId::new();
    let attr = run_pill(run);
    ws.set_attr(file, &attr).unwrap();
    assert_reads_as_the_run_document(&only_attr(&ws, file), run);

    let bytes = ws.export_snapshot().unwrap();
    let older = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    let read = only_attr(&older, file);
    assert_eq!(read.id, attr.id);
    assert_reads_as_the_run_document(&read, run);
    assert_eq!(older.export_snapshot().unwrap(), bytes);
}

// ------------------------------------------------------------------ run target

/// The target is a Run document: a `DocumentReference` carries only the
/// document id, so the target's kind (`koru.run` in the manifest) never
/// enters the record and cannot fail to round-trip.
///
/// **The deep target surfaces now.** The record always stored it (`target`),
/// and P1.15 widened the reader's normalization to `koru.run`, so a pill
/// pointing at one timeline entry reads back as pointing at that entry rather
/// than at the run. M1's pill points at the run and nothing inside it, so this
/// half of the change costs nothing today; it is why a later plan can deep-link
/// into a run with no storage step.
#[test]
fn a_run_document_target_round_trips_by_id_and_a_deep_target_comes_back() {
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("x").unwrap())
        .unwrap();
    let run = DocumentId::new();
    let deep = NodeId::new();
    ws.insert_pill(
        line,
        0,
        &Attr::new(
            ROLE_RUN,
            AttrValue::Document(DocumentReference::at(run, deep)),
        ),
    )
    .unwrap();

    let bytes = ws.export_snapshot().unwrap();
    let reader = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    assert_eq!(
        only_attr(&reader, line).as_document_reference(),
        Some(DocumentReference::at(run, deep)),
        "the entry, and the run it lives in"
    );
    // Reading performed no write.
    assert_eq!(reader.export_snapshot().unwrap(), bytes);
    // The `document` role is spelled the same way, and always was.
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    let root = ws
        .insert(Parent::Root, Node::document("Plan").unwrap())
        .unwrap();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("x").unwrap())
        .unwrap();
    ws.insert_pill(
        line,
        0,
        &Attr::new(
            "document",
            AttrValue::Document(DocumentReference::at(run, deep)),
        ),
    )
    .unwrap();
    assert_eq!(
        only_attr(&reopened(&ws), line).as_document_reference(),
        Some(DocumentReference::at(run, deep))
    );
}

// ------------------------------------------------------------------ the gate

/// Nothing above moves the content schema: a document carrying a run pill is
/// stamped with this build's version and is writable by this build.
/// `SCHEMA_VERSION` is pinned so a bump cannot ride in unremarked — and
/// `stamp_schema_version` stamps the build's version on **every** newly
/// created document, so a bump would reach far beyond pill-carrying ones.
#[test]
fn a_run_pill_needs_no_content_schema_bump() {
    assert_eq!(
        SCHEMA_VERSION, 5,
        "P1.0 established the pill at schema 4; callouts moved it to 5"
    );
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("x").unwrap())
        .unwrap();
    ws.insert_pill(line, 0, &run_pill(DocumentId::new()))
        .unwrap();
    assert_eq!(ws.schema_version(), Some(SCHEMA_VERSION));
    assert_eq!(reopened(&ws).compatibility(), Compatibility::Writable);
}

/// **What the reader normalizes, and why.** `document` and `koru.run` — and
/// nothing else — come back as the typed `AttrValue::Document`; every other
/// role's byte-identical record comes back as a `Ref` homed at the same
/// document.
///
/// This is the pin the widening landed on. Before P1.15 it read *"the document
/// role is the one the reader normalizes today"*; the run role joined it
/// because a run pill's whole purpose is to name a Run document, and a second
/// resolution path through `home` in every surface that draws one is larger
/// than one branch here (proposal §3.3, option 2; maintainer answer to Q4). The set
/// is deliberately short, and belonging to it changes what is *answered*,
/// never what is stored.
#[test]
fn the_document_and_run_roles_are_what_the_reader_normalizes() {
    assert_eq!(
        koru_core::attributes::DOCUMENT_REFERENCE_ROLES,
        ["document", ROLE_RUN],
    );
    let target = DocumentId::new();
    let read_under = |role: &str| {
        let (mut ws, root) = a_document();
        let line = ws
            .insert(Parent::Node(root), Node::paragraph("x").unwrap())
            .unwrap();
        ws.insert_pill(
            line,
            0,
            &Attr::new(role, AttrValue::Document(DocumentReference::new(target))),
        )
        .unwrap();
        only_attr(&reopened(&ws), line)
    };
    for role in koru_core::attributes::DOCUMENT_REFERENCE_ROLES {
        assert_eq!(
            read_under(role).as_document_reference(),
            Some(DocumentReference::new(target)),
            "{role}"
        );
    }
    let other = read_under(ROLE_AN_OLDER_BUILD_DOES_NOT_KNOW);
    assert_eq!(other.as_document_reference(), None);
    assert!(
        matches!(other.value, AttrValue::Ref(_)),
        "{:?}",
        other.value
    );
}

// ------------------------------------------------------------------ the older reader

/// **The record an older build reads is the record this one writes.**
///
/// A shipped build's `read_attr_value` normalizes only the `document` role, so
/// it hands a run pill back as `AttrValue::Ref` homed at the run. This build
/// cannot *be* that build, so the branch is exercised through a role that is
/// not in `DOCUMENT_REFERENCE_ROLES`: the encoder is the same one taking the
/// same `AttrValue::Document`, so the bytes are a run pill's in every respect
/// but the role string.
///
/// What it holds: nothing is lost (the run is reached through `home`), the
/// label is there to draw with, and re-exporting after reading changes no
/// bytes.
#[test]
fn a_role_this_build_does_not_normalize_reads_as_a_ref_homed_at_the_run() {
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("x").unwrap())
        .unwrap();
    let run = DocumentId::new();
    ws.insert_pill(
        line,
        0,
        &Attr::labelled(
            ROLE_AN_OLDER_BUILD_DOES_NOT_KNOW,
            AttrValue::Document(DocumentReference::new(run)),
            "Run",
        ),
    )
    .unwrap();

    let bytes = ws.export_snapshot().unwrap();
    let older = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    let read = only_attr(&older, line);
    match &read.value {
        AttrValue::Ref(reference) => {
            assert_eq!(reference.home, Some(DocRef::Document(run)));
            assert_eq!(reference.node.as_uuid(), run.as_uuid());
        }
        other => panic!("an unknown role read back as {other:?}"),
    }
    assert_eq!(read.as_document_reference(), None);
    assert_eq!(
        read.label.as_deref(),
        Some("Run"),
        "the label is what such a build draws"
    );
    assert_eq!(older.export_snapshot().unwrap(), bytes);
}

// ------------------------------------------------------- P1.15: the round trip

/// **Save, close, reopen — twice — and the pill is the same pill naming the
/// same run.**
///
/// The roadmap's first verification row for P1.15. "Close and reopen" is a
/// snapshot exported and a `WorkspaceDoc` built from it: what the store hands a
/// window on the next open. The second cycle is not ceremony — the first export
/// is written by the process that made the pill and the second by a process
/// that only ever read it, and only the second shows a reader re-exporting what
/// it did not write.
#[test]
fn a_run_pill_survives_save_close_and_reopen_and_still_names_its_run() {
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("Delegated ").unwrap())
        .unwrap();
    let run = DocumentId::new();
    let attr = Attr::labelled(
        ROLE_RUN,
        AttrValue::Document(DocumentReference::new(run)),
        "Run",
    );
    ws.insert_pill(line, "Delegated ".len(), &attr).unwrap();

    let saved = ws.export_snapshot().unwrap();
    drop(ws);

    let once = WorkspaceDoc::from_snapshot(&saved).unwrap();
    assert_reads_as_the_run_document(&only_attr(&once, line), run);
    let saved_again = once.export_snapshot().unwrap();
    assert_eq!(
        saved_again, saved,
        "a reader re-exports what it did not write"
    );

    let twice = WorkspaceDoc::from_snapshot(&saved_again).unwrap();
    let read = only_attr(&twice, line);
    assert_reads_as_the_run_document(&read, run);
    assert_eq!(read.id, attr.id, "the same attribute, not a new one");
    assert_eq!(read.label.as_deref(), Some("Run"));
    let anchor = twice
        .runs(line)
        .unwrap()
        .into_iter()
        .find(|run| run.is_pill())
        .expect("the anchor survives two round trips");
    assert_eq!(anchor.pill, Some(attr.id));
}

/// **Two roots, one run.** The roadmap's sync row: the pill written on one
/// root arrives on the other naming the same run — and the executing device is
/// *not* in the record, which is exactly what lets the second root name a
/// device that is not its own. Both roots read one id and ask the Run document.
///
/// The merge is Loro's own, with the second root editing elsewhere in the same
/// document while the delegation is accepted on the first.
#[test]
fn a_run_pill_syncs_to_a_second_root_carrying_only_the_run_id() {
    let (mut here, root) = a_document();
    let line = here
        .insert(Parent::Node(root), Node::paragraph("Delegated ").unwrap())
        .unwrap();
    let base = here.export_snapshot().unwrap();

    // The second root opened the document before the pill existed.
    let mut there = WorkspaceDoc::from_snapshot(&base).unwrap();

    let run = DocumentId::new();
    let attr = Attr::labelled(
        ROLE_RUN,
        AttrValue::Document(DocumentReference::new(run)),
        "Run",
    );
    here.insert_pill(line, "Delegated ".len(), &attr).unwrap();
    there
        .insert(
            Parent::Node(root),
            Node::paragraph("meanwhile, over here").unwrap(),
        )
        .unwrap();

    there.import(&here.export_snapshot().unwrap()).unwrap();

    let read = only_attr(&there, line);
    assert_reads_as_the_run_document(&read, run);
    assert_eq!(read.id, attr.id);
    // **Nothing about the execution is in the record**: one role, one id, one
    // label. Status and the executing device are the Run document's to answer,
    // by the id this record carries.
    assert_eq!(read.label.as_deref(), Some("Run"));
    assert_eq!(
        there
            .runs(line)
            .unwrap()
            .iter()
            .filter(|r| r.is_pill())
            .count(),
        1
    );

    // And back the other way.
    here.import(&there.export_snapshot().unwrap()).unwrap();
    assert_reads_as_the_run_document(&only_attr(&here, line), run);
}

/// **The run outlives the paragraph it came from** (D31). Trashing the source
/// row takes the pill out of the reading with it and destroys nothing: the
/// record is still there, still naming the run, and the Run document — which
/// this file never touches — is not the document's to delete.
#[test]
fn trashing_the_source_row_takes_the_pill_and_not_the_run() {
    let (mut ws, root) = a_document();
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("Delegated ").unwrap())
        .unwrap();
    let run = DocumentId::new();
    ws.insert_pill(line, "Delegated ".len(), &run_pill(run))
        .unwrap();
    ws.trash(line, 1_700_000_000_000, None).unwrap();

    let after = reopened(&ws);
    assert_reads_as_the_run_document(&only_attr(&after, line), run);
    assert!(after.is_trashed(line));
}
