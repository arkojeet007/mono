// SPDX-License-Identifier: MPL-2.0

//! Where the crumb lands, and what it is when it gets there.

use koru_core::node::{AttrValue, BlobHash, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{DocumentId, DocumentReference, NodeId, WorkspaceId};

use super::*;

/// A workspace holding one document, no store behind it.
fn holding() -> (koru_workspace::Workspace, DocumentId, NodeId) {
    let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "Work").unwrap();
    let root = doc
        .insert(Parent::Root, Node::document("Plan").unwrap())
        .unwrap();
    let id = DocumentId::new();
    (
        koru_workspace::Workspace::holding_for_tests(doc, id),
        id,
        root,
    )
}

fn row(workspace: &mut koru_workspace::Workspace, root: NodeId, text: &str) -> NodeId {
    workspace
        .ws
        .insert(Parent::Node(root), Node::paragraph(text).unwrap())
        .unwrap()
}

fn only_run_attr(doc: &WorkspaceDoc, node: NodeId) -> koru_core::node::Attr {
    let read = doc.read(node).unwrap();
    let attrs: Vec<_> = read
        .content
        .attrs()
        .iter()
        .filter(|attr| attr.role == koru_core::attributes::ROLE_RUN)
        .cloned()
        .collect();
    assert_eq!(attrs.len(), 1, "exactly one run pill: {attrs:?}");
    attrs[0].clone()
}

/// **The crumb follows the origin.** The pill goes after what is written on
/// the row, with one space between, and nothing somebody typed is touched.
#[test]
fn the_pill_follows_the_text_it_was_delegated_from() {
    let (mut workspace, document, root) = holding();
    let node = row(&mut workspace, root, "Tidy the release notes");
    let run = DocumentId::new();

    serve(
        &mut workspace,
        &RunPill {
            document,
            node,
            run,
            label: "Delegated run".into(),
        },
    )
    .expect("the pill is written");

    let read = workspace.ws.read(node).unwrap();
    let text = read.content.as_text().unwrap();
    assert!(
        text.starts_with("Tidy the release notes "),
        "the words are untouched and the pill is after them: {text:?}"
    );
    let attr = only_run_attr(&workspace.ws, node);
    assert_eq!(
        attr.as_document_reference(),
        Some(DocumentReference::new(run))
    );
    assert_eq!(attr.label.as_deref(), Some("Delegated run"));
    // Anchored in the text, so it draws where the sentence puts it.
    let anchor = workspace
        .ws
        .runs(node)
        .unwrap()
        .into_iter()
        .find(|run| run.is_pill())
        .expect("the pill has an anchor");
    assert_eq!(anchor.pill, Some(attr.id));
}

/// An empty row gets no space it does not need.
#[test]
fn an_empty_row_gets_the_pill_and_no_padding() {
    let (mut workspace, document, root) = holding();
    let node = row(&mut workspace, root, "");
    serve(
        &mut workspace,
        &RunPill {
            document,
            node,
            run: DocumentId::new(),
            label: "Delegated run".into(),
        },
    )
    .unwrap();
    let read = workspace.ws.read(node).unwrap();
    assert!(
        !read.content.as_text().unwrap().starts_with(' '),
        "no leading space on an empty row"
    );
}

/// **A row with no text takes the record on the node itself** — a delegated
/// file, which is the shape P1.0 measured for a blob node.
#[test]
fn a_file_row_carries_the_pill_as_a_whole_node_attribute() {
    let (mut workspace, document, root) = holding();
    let node = workspace
        .ws
        .insert(
            Parent::Node(root),
            Node::file("brief.pdf", BlobHash::new("b3", "abc"), "application/pdf").unwrap(),
        )
        .unwrap();
    let run = DocumentId::new();
    serve(
        &mut workspace,
        &RunPill {
            document,
            node,
            run,
            label: "Delegated run".into(),
        },
    )
    .unwrap();
    let attr = only_run_attr(&workspace.ws, node);
    assert!(matches!(attr.value, AttrValue::Document(_)));
    assert_eq!(
        attr.as_document_reference(),
        Some(DocumentReference::new(run))
    );
    assert!(
        workspace.ws.runs(node).unwrap().is_empty(),
        "a blob has no text to anchor in"
    );
}

/// **Writing twice writes one pill.** A replayed answer, a retry, or two
/// windows racing must not leave a person with two crumbs for one run.
#[test]
fn the_same_run_is_never_written_twice() {
    let (mut workspace, document, root) = holding();
    let node = row(&mut workspace, root, "Tidy the release notes");
    let run = DocumentId::new();
    let pill = RunPill {
        document,
        node,
        run,
        label: "Delegated run".into(),
    };
    serve(&mut workspace, &pill).unwrap();
    let after_once = workspace.ws.read(node).unwrap();
    let text_once = after_once.content.as_text().unwrap().to_owned();

    serve(&mut workspace, &pill).unwrap();
    only_run_attr(&workspace.ws, node);
    assert_eq!(
        workspace.ws.read(node).unwrap().content.as_text().unwrap(),
        text_once,
        "nothing was added the second time"
    );
}

/// **Two runs on one row are two pills.** Delegating the same line twice is an
/// ordinary thing to do, and both records have to stand.
#[test]
fn two_runs_on_one_row_are_two_pills() {
    let (mut workspace, document, root) = holding();
    let node = row(&mut workspace, root, "Tidy the release notes");
    for _ in 0..2 {
        serve(
            &mut workspace,
            &RunPill {
                document,
                node,
                run: DocumentId::new(),
                label: "Delegated run".into(),
            },
        )
        .unwrap();
    }
    let read = workspace.ws.read(node).unwrap();
    let runs = read
        .content
        .attrs()
        .iter()
        .filter(|attr| attr.role == koru_core::attributes::ROLE_RUN)
        .count();
    assert_eq!(runs, 2);
    assert_eq!(
        workspace
            .ws
            .runs(node)
            .unwrap()
            .iter()
            .filter(|run| run.is_pill())
            .count(),
        2
    );
}

/// **The stored shape is the one P1.0 established and the maintainers approved**: the
/// open role, an `AttrValue::Document`, and the existing `"ref"` storage
/// token — so no content-schema bump rides in, and an older reader preserves
/// what it cannot type.
#[test]
fn what_is_written_is_the_approved_shape() {
    let run = DocumentId::new();
    let attr = RunPill {
        document: DocumentId::new(),
        node: NodeId::new(),
        run,
        label: "Delegated run".into(),
    }
    .attr();
    assert_eq!(attr.role, "koru.run");
    assert_eq!(attr.value.key(), "ref", "the existing storage token");
    assert_eq!(
        attr.value,
        AttrValue::Document(DocumentReference::new(run)),
        "the run, and no node inside it"
    );
    assert_eq!(
        koru_core::schema::SCHEMA_VERSION,
        5,
        "the pill needs no content-schema bump (5 is the callout's, not the pill's)"
    );
}

/// A row that has gone — trashed and emptied, or never there — refuses rather
/// than writing somewhere else. The run is real either way; the crumb is not
/// worth guessing at.
#[test]
fn a_row_that_is_not_there_refuses() {
    let (mut workspace, document, _) = holding();
    let outcome = serve(
        &mut workspace,
        &RunPill {
            document,
            node: NodeId::new(),
            run: DocumentId::new(),
            label: "Delegated run".into(),
        },
    );
    assert!(outcome.is_err());
}
