// SPDX-License-Identifier: MPL-2.0

//! The version gate: which builds may write to which documents.
//!
//! `schema_version` has been written into every workspace since the first one
//! and read by nothing. That was fine while one program wrote it; it stops
//! being fine the moment a released build and a build from this tree take turns
//! over one directory. These pin the rule that keeps the older one honest.

use koru_core::WorkspaceId;
use koru_core::node::Node;
use koru_core::schema::{
    Compatibility, LEGACY_SCHEMA_VERSION, Parent, SCHEMA_VERSION, WorkspaceDoc,
};

/// What the Koru Store hands a client: a `LoroDoc` with nothing in it, and in
/// particular no `meta` map. Every document created since the cutover began
/// life like this, which is why "no marker" stopped meaning "written long ago".
fn a_document_the_store_made() -> WorkspaceDoc {
    let empty = loro::LoroDoc::new();
    let bytes = empty.export(loro::ExportMode::Snapshot).unwrap();
    WorkspaceDoc::from_snapshot(&bytes).unwrap()
}

/// A document carrying a marker this build did not write — an older one, or one
/// from a build that does not exist yet. Written through raw Loro because the
/// model deliberately offers no way to set an arbitrary version.
fn a_document_with_schema_version(version: i64) -> WorkspaceDoc {
    let doc = loro::LoroDoc::new();
    doc.get_map("meta")
        .insert("schema_version", version)
        .unwrap();
    doc.commit();
    let bytes = doc.export(loro::ExportMode::Snapshot).unwrap();
    WorkspaceDoc::from_snapshot(&bytes).unwrap()
}

#[test]
fn a_new_workspace_records_the_version_it_was_written_by() {
    let doc = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    assert_eq!(doc.schema_version(), Some(SCHEMA_VERSION));
    assert_eq!(doc.compatibility(), Compatibility::Writable);
}

/// The field has to survive the snapshot, or the gate would only ever see the
/// document it just made.
#[test]
fn the_version_survives_a_snapshot() {
    let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    doc.insert(Parent::Root, Node::task("something").unwrap())
        .unwrap();
    let bytes = doc.export_snapshot().unwrap();

    let reopened = WorkspaceDoc::from_snapshot(&bytes).unwrap();
    assert_eq!(reopened.schema_version(), Some(SCHEMA_VERSION));
    assert!(reopened.compatibility().writable());
}

/// The decision table, without needing a document from the future to test it.
#[test]
fn a_newer_document_is_not_writable_by_this_build() {
    assert_eq!(
        Compatibility::of(Some(SCHEMA_VERSION)),
        Compatibility::Writable
    );

    let newer = Compatibility::of(Some(SCHEMA_VERSION + 1));
    assert_eq!(
        newer,
        Compatibility::TooNew {
            found: SCHEMA_VERSION + 1,
            understood: SCHEMA_VERSION,
        }
    );
    assert!(!newer.writable());
    // The message is what the user is shown, so it has to name both numbers.
    let said = newer.to_string();
    assert!(said.contains(&(SCHEMA_VERSION + 1).to_string()), "{said}");
    assert!(said.contains(&SCHEMA_VERSION.to_string()), "{said}");
}

/// A document written before the field existed is a legacy version 1, not an
/// unknown one. Refusing to write to it would lock out exactly the documents
/// this change is meant to protect.
#[test]
fn a_document_from_before_the_field_is_still_writable() {
    assert_eq!(Compatibility::of(None), Compatibility::Writable);
}

/// **A missing marker is a number, and everything that reports one says so.**
///
/// `schema_version` answers `None` and there is no way for a reader on the far
/// side of an API to substitute the 1 for itself without knowing this rule. MCP
/// published the raw `Option` as a required integer and every store-created
/// document came back with `null` in a field its own schema said was an
/// integer — a whole tool rejected by strict clients over an absence that has a
/// perfectly good value.
#[test]
fn an_unmarked_document_reports_the_legacy_version_rather_than_nothing() {
    let doc = a_document_the_store_made();
    assert_eq!(doc.schema_version(), None);
    assert_eq!(doc.effective_schema_version(), LEGACY_SCHEMA_VERSION);
    assert_eq!(doc.compatibility(), Compatibility::Writable);

    let created = WorkspaceDoc::create(WorkspaceId::new(), "acme").unwrap();
    assert_eq!(created.effective_schema_version(), SCHEMA_VERSION);
}

/// The store's empty document is the one shape `create` never touches, so the
/// marker has to be put on it explicitly. Stamping is creation-time only: it
/// must never overwrite what another build recorded, in either direction.
#[test]
fn stamping_marks_an_unmarked_document_and_leaves_a_marked_one_alone() {
    let mut fresh = a_document_the_store_made();
    fresh.stamp_schema_version().unwrap();
    assert_eq!(fresh.schema_version(), Some(SCHEMA_VERSION));

    // It survives the round trip, or the gate would only see it in the process
    // that stamped it.
    let bytes = fresh.export_snapshot().unwrap();
    assert_eq!(
        WorkspaceDoc::from_snapshot(&bytes)
            .unwrap()
            .schema_version(),
        Some(SCHEMA_VERSION)
    );

    // A document from a newer build keeps its own number. Stamping this
    // build's over it would claim this build understood content it cannot
    // read — the silent downgrade the gate exists to prevent — and stamping
    // over an *older* number would claim it wrote content that is not there.
    for recorded in [SCHEMA_VERSION + 1, 1] {
        let mut marked = a_document_with_schema_version(recorded);
        marked.stamp_schema_version().unwrap();
        assert_eq!(marked.schema_version(), Some(recorded));
    }
}

/// Where a migration will go. There has only ever been one version, so an older
/// document is accepted as-is. Version 2 added the `Divider` kind — a brand-new
/// block no older document contains, so v1 still needs **no data migration**:
/// the bump only ensures a v2 document (with dividers) opens read-only in a v1
/// build rather than being silently re-exported without them. Bumping the
/// version without recording the stance here fails this test on purpose.
#[test]
fn an_older_document_is_accepted_until_there_is_something_to_migrate() {
    assert_eq!(
        SCHEMA_VERSION, 5,
        "v4 added the root-only File kind and v5 the Callout kind; \
         neither migrates older content"
    );
    assert_eq!(Compatibility::of(Some(1)), Compatibility::Writable);
    assert_eq!(Compatibility::of(Some(0)), Compatibility::Writable);
}

/// There is more than one document type now, and each versions itself. A build
/// that understands workspaces at 2 and profiles at 1 has to be expressible, or
/// the two would be forced to bump together forever.
#[test]
fn compatibility_gates_on_whatever_version_it_is_handed() {
    assert_eq!(Compatibility::against(Some(1), 2), Compatibility::Writable);
    assert_eq!(Compatibility::against(None, 7), Compatibility::Writable);
    assert_eq!(
        Compatibility::against(Some(3), 2),
        Compatibility::TooNew {
            found: 3,
            understood: 2,
        }
    );
    // `of` is that function with this build's workspace version filled in.
    assert_eq!(
        Compatibility::of(Some(SCHEMA_VERSION + 1)),
        Compatibility::against(Some(SCHEMA_VERSION + 1), SCHEMA_VERSION)
    );
}

/// The sentence names the kind of document it is about. "This workspace was
/// written by a newer version" shown for a *profile* sends somebody looking in
/// the wrong place for a file that is perfectly fine.
#[test]
fn the_message_names_the_kind_of_document_it_is_about() {
    let newer = Compatibility::against(Some(9), 1);
    assert!(newer.describe("profile").contains("profile"), "{newer:?}");
    assert!(!newer.describe("profile").contains("workspace"));
    // `Display` is the workspace's, which is what every existing caller means.
    assert_eq!(newer.to_string(), newer.describe("workspace"));
}
