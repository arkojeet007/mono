// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::Mark;
use koru_core::NodeId;
use koru_core::WorkspaceId;
use koru_core::node::{Attr, AttrValue, BlobHash, Node, NodeMeta};
use koru_core::schema::WorkspaceDoc;
use koru_export::{NoBlobs, NoProgress};

fn blank() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "export-test").unwrap()
}

fn with_doc(title: &str) -> (WorkspaceDoc, NodeId) {
    let mut ws = blank();
    let root = ws
        .insert(Parent::Root, Node::document(title).unwrap())
        .unwrap();
    (ws, root)
}

#[test]
fn title_and_paragraph_become_markdown() {
    let (mut ws, root) = with_doc("Hello");
    ws.insert(Parent::Node(root), Node::paragraph("Body line").unwrap())
        .unwrap();
    let rendered = render_document(&ws, &NoBlobs, "Hello.assets").unwrap();
    assert_eq!(rendered.markdown, "# Hello\n\nBody line\n");
}

#[test]
fn heading_quote_code_divider_and_table() {
    let (mut ws, root) = with_doc("Doc");
    let mut h = Node::paragraph("Section").unwrap();
    h.meta.heading = Some(Heading::H2);
    ws.insert(Parent::Node(root), h).unwrap();
    ws.insert(Parent::Node(root), Node::quote("said so").unwrap())
        .unwrap();
    let mut code = Node::code("fn main() {}").unwrap();
    code.meta.language = Some("rust".into());
    ws.insert(Parent::Node(root), code).unwrap();
    ws.insert(Parent::Node(root), Node::divider().unwrap())
        .unwrap();
    ws.insert(
        Parent::Node(root),
        Node::table("| a | b |\n| --- | --- |\n| 1 | 2 |").unwrap(),
    )
    .unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("## Section"));
    assert!(md.contains("> said so"));
    assert!(md.contains("```rust\nfn main() {}\n```"));
    assert!(md.contains("---"));
    assert!(md.contains("| a | b |"));
}

/// A code block whose body quotes a three-backtick fence is wrapped in a
/// four-backtick one, or the body would close the block on its own line.
#[test]
fn a_code_body_holding_a_fence_gets_a_longer_fence() {
    let (mut ws, root) = with_doc("Doc");
    let mut code = Node::code("a\n```\nb").unwrap();
    code.meta.language = Some("md".into());
    ws.insert(Parent::Node(root), code).unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    // The same bytes the clipboard's plain-text flavor writes for this body:
    // both come from `koru_core::markdown::fenced_code`, and the editor's
    // clipboard test pins this literal too.
    assert!(md.contains("````md\na\n```\nb\n````\n"), "{md}");
    assert_eq!(
        md.matches("````").count(),
        2,
        "exactly one opening and one closing fence of four: {md}"
    );
}

/// ...and a four-backtick run in the body needs five.
#[test]
fn a_code_body_holding_four_backticks_gets_five() {
    let (mut ws, root) = with_doc("Doc");
    ws.insert(Parent::Node(root), Node::code("````\nraw\n````").unwrap())
        .unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("`````\n````\nraw\n````\n`````\n"), "{md}");
    assert!(
        md.contains(&koru_core::markdown::fenced_code("", "````\nraw\n````")),
        "the exporter writes exactly the shared helper's block: {md}"
    );
}

/// Two boxes: one with the default look around one line, one with a chosen
/// icon and tint around a line, a paragraph, a list and a heading. Each is
/// a quote led by its icon (the default when none is set), its first line
/// on the icon's line, the rest as further lines of the same quote.
fn boxed_doc() -> WorkspaceDoc {
    let (mut ws, root) = with_doc("Doc");
    let plain = ws
        .insert(Parent::Node(root), Node::callout(None, None).unwrap())
        .unwrap();
    ws.insert(Parent::Node(plain), Node::paragraph("plain").unwrap())
        .unwrap();
    let head = ws
        .insert(
            Parent::Node(root),
            Node::callout(Some("⚠️"), Some("red")).unwrap(),
        )
        .unwrap();
    ws.insert(Parent::Node(head), Node::paragraph("careful").unwrap())
        .unwrap();
    ws.insert(Parent::Node(head), Node::paragraph("Second line").unwrap())
        .unwrap();
    let list = ws
        .insert(Parent::Node(head), Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(list, Some(ListStyle::Bullet)).unwrap();
    let one = ws
        .insert(Parent::Node(list), Node::paragraph("one").unwrap())
        .unwrap();
    let nested = ws
        .insert(Parent::Node(one), Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(nested, Some(ListStyle::Bullet)).unwrap();
    ws.insert(Parent::Node(nested), Node::paragraph("deeper").unwrap())
        .unwrap();
    ws.insert(Parent::Node(list), Node::paragraph("two").unwrap())
        .unwrap();
    let mut title = Node::paragraph("Title").unwrap();
    title.meta.heading = Some(Heading::H2);
    ws.insert(Parent::Node(head), title).unwrap();
    ws.insert(Parent::Node(root), Node::paragraph("after").unwrap())
        .unwrap();
    ws
}

const BOXED_MARKDOWN: &str = "# Doc\n\n> 💡 plain\n\n> ⚠️ careful\n>\n> Second line\n>\n> - one\n>   - deeper\n> - two\n>\n> ## Title\n\nafter\n";

#[test]
fn a_callout_leaves_as_one_quote_led_by_its_icon_with_its_lines_inside() {
    let md = render_document(&boxed_doc(), &NoBlobs, "x.assets")
        .unwrap()
        .markdown;
    assert_eq!(md, BOXED_MARKDOWN);
}

/// The round trip: the box the importer reads back out of that quote has
/// the same shape — head, then its lines as children — and exports to the
/// same text.
#[test]
fn a_callout_round_trips_through_markdown_with_its_lines_inside() {
    use koru_core::Kind;
    let body = BOXED_MARKDOWN.strip_prefix("# Doc\n\n").unwrap();
    let context = koru_import::ImportContext {
        target_profile: None,
        target_workspace: None,
        occupied_titles: &std::collections::BTreeSet::new(),
        blobs: &koru_import::NoBlobSink,
        tags: &koru_import::NoTagSink,
    };
    let (_, snapshot, warnings) = koru_import_markdown::build_document_from_text(
        WorkspaceId::new(),
        "Doc",
        "doc.md",
        body,
        &context,
        &koru_import::NoProgress,
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
    let root = doc.children(Parent::Root).unwrap()[0];
    let shape = |parent: NodeId| -> Vec<(Kind, String, Option<String>)> {
        doc.children(Parent::Node(parent))
            .unwrap()
            .into_iter()
            .map(|id| {
                let node = doc.read(id).unwrap();
                (
                    node.kind,
                    node.content.as_text().unwrap_or_default().to_owned(),
                    node.meta.icon,
                )
            })
            .collect()
    };
    // Markdown has to write an emoji to mark the box, so a box with no icon
    // set comes back with the default stored — and draws, and exports,
    // exactly as before. The box carries no text; its lines are under it.
    assert_eq!(
        shape(root),
        vec![
            (Kind::Callout, String::new(), Some("💡".into())),
            (Kind::Callout, String::new(), Some("⚠️".into())),
            (Kind::Paragraph, "after".into(), None),
        ]
    );
    let boxes = doc.children(Parent::Node(root)).unwrap();
    assert_eq!(
        shape(boxes[0]),
        vec![(Kind::Paragraph, "plain".into(), None)]
    );
    let head = boxes[1];
    let inside = shape(head);
    assert_eq!(inside[0], (Kind::Paragraph, "careful".into(), None));
    assert_eq!(inside[1], (Kind::Paragraph, "Second line".into(), None));
    assert_eq!(inside[2].0, Kind::ListBlock);
    assert_eq!(inside[3], (Kind::Paragraph, "Title".into(), None));
    assert_eq!(inside.len(), 4, "{inside:?}");
    let list = doc.children(Parent::Node(head)).unwrap()[2];
    assert_eq!(
        shape(list),
        vec![
            (Kind::Paragraph, "one".into(), None),
            (Kind::Paragraph, "two".into(), None),
        ]
    );

    let again = render_document(&doc, &NoBlobs, "x.assets")
        .unwrap()
        .markdown;
    assert_eq!(again, BOXED_MARKDOWN);
}

/// A box whose first line is not prose — a list — leads with the icon
/// alone, so the list is not written onto the icon's line, and comes back
/// as the same box around the same list.
#[test]
fn a_callout_that_opens_with_a_list_leads_with_its_icon_alone() {
    use koru_core::Kind;
    let (mut ws, root) = with_doc("Doc");
    let callout = ws
        .insert(Parent::Node(root), Node::callout(None, None).unwrap())
        .unwrap();
    let list = ws
        .insert(Parent::Node(callout), Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(list, Some(ListStyle::Bullet)).unwrap();
    ws.insert(Parent::Node(list), Node::paragraph("one").unwrap())
        .unwrap();
    ws.insert(Parent::Node(list), Node::paragraph("two").unwrap())
        .unwrap();
    const LED_BY_THE_ICON_ALONE: &str = "> 💡\n>\n> - one\n> - two\n";
    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert_eq!(md, format!("# Doc\n\n{LED_BY_THE_ICON_ALONE}"));

    let context = koru_import::ImportContext {
        target_profile: None,
        target_workspace: None,
        occupied_titles: &std::collections::BTreeSet::new(),
        blobs: &koru_import::NoBlobSink,
        tags: &koru_import::NoTagSink,
    };
    let (_, snapshot, _) = koru_import_markdown::build_document_from_text(
        WorkspaceId::new(),
        "Doc",
        "doc.md",
        LED_BY_THE_ICON_ALONE,
        &context,
        &koru_import::NoProgress,
    )
    .unwrap();
    let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
    let root = doc.children(Parent::Root).unwrap()[0];
    let body = doc.children(Parent::Node(root)).unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(doc.read(body[0]).unwrap().kind, Kind::Callout);
    let inside = doc.children(Parent::Node(body[0])).unwrap();
    assert_eq!(inside.len(), 1, "only the list is inside");
    assert_eq!(doc.read(inside[0]).unwrap().kind, Kind::ListBlock);
}

#[test]
fn checkbox_list_and_task_tree() {
    let (mut ws, root) = with_doc("Tasks");
    let block = ws
        .insert(Parent::Node(root), Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(block, Some(ListStyle::Checkbox)).unwrap();
    let item = ws
        .insert(Parent::Node(block), Node::paragraph("todo").unwrap())
        .unwrap();
    ws.set_done(item, true).unwrap();

    let task = ws
        .insert(Parent::Node(root), Node::task("outline").unwrap())
        .unwrap();
    ws.insert(Parent::Node(task), Node::task("child").unwrap())
        .unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("- [x] todo"), "{md}");
    assert!(md.contains("- [ ] outline"), "{md}");
    assert!(md.contains("  - [ ] child"), "{md}");
}

#[test]
fn letter_list_is_skipped() {
    let (mut ws, root) = with_doc("L");
    let block = ws
        .insert(Parent::Node(root), Node::list_block().unwrap())
        .unwrap();
    ws.set_list_style(block, Some(ListStyle::Letter)).unwrap();
    ws.insert(Parent::Node(block), Node::paragraph("a").unwrap())
        .unwrap();
    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("<!-- letter/roman list omitted -->"));
    assert!(!md.contains("\na\n") && !md.contains("- a"));
}

#[test]
fn nested_document_children_are_skipped() {
    let (mut ws, root) = with_doc("Parent");
    ws.insert(Parent::Node(root), Node::paragraph("keep").unwrap())
        .unwrap();
    ws.insert(Parent::Node(root), Node::document("Child page").unwrap())
        .unwrap();
    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("keep"));
    assert!(!md.contains("Child page"));
}

#[test]
fn bold_and_link_pill() {
    let (mut ws, root) = with_doc("R");
    let p = ws
        .insert(Parent::Node(root), Node::paragraph("hello").unwrap())
        .unwrap();
    ws.set_mark(p, 0..5, Mark::Bold, true).unwrap();
    let link = Attr::labelled("link", AttrValue::Url("https://example.com".into()), "ex");
    let end = ws.read(p).unwrap().content.as_text().unwrap().len();
    ws.insert_pill(p, end, &link).unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("**hello**"), "{md}");
    assert!(md.contains("[ex](https://example.com)"), "{md}");
}

#[test]
fn document_ref_pill_is_koru_wiki_link() {
    let (mut ws, root) = with_doc("R");
    let target = koru_core::DocumentId::new();
    let p = ws
        .insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    let attr = Attr::new(
        "document",
        AttrValue::Document(koru_core::DocumentReference::new(target)),
    );
    ws.insert_pill(p, 0, &attr).unwrap();
    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains(&format!("[[koru://doc/{target}]]")), "{md}");
}

/// A deep link carries its block. Without the fragment the round trip loses
/// the half that makes it a deep link, silently.
#[test]
fn a_document_pill_with_a_node_exports_the_fragment() {
    let (mut ws, root) = with_doc("R");
    let target = koru_core::DocumentId::new();
    let block = koru_core::NodeId::new();
    let p = ws
        .insert(Parent::Node(root), Node::paragraph("").unwrap())
        .unwrap();
    let attr = Attr::new(
        "document",
        AttrValue::Document(koru_core::DocumentReference::at(target, block)),
    );
    ws.insert_pill(p, 0, &attr).unwrap();

    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;

    assert!(
        md.contains(&format!("[[koru://doc/{target}#{block}]]")),
        "{md}"
    );
}

/// The composer's message: paragraphs are lines, typed Markdown travels as
/// typed, and a document pill is the same token a document export writes.
#[test]
fn a_message_joins_paragraphs_with_one_newline_and_keeps_typed_markdown() {
    let (mut ws, root) = with_doc("");
    let target = koru_core::DocumentId::new();
    let first = ws
        .insert(
            Parent::Node(root),
            Node::paragraph("Compare my_var * 2 with ").unwrap(),
        )
        .unwrap();
    let attr = Attr::labelled(
        "document",
        AttrValue::Document(koru_core::DocumentReference::new(target)),
        "Budget",
    );
    let end = ws.read(first).unwrap().content.as_text().unwrap().len();
    ws.insert_pill(first, end, &attr).unwrap();
    ws.insert(Parent::Node(root), Node::paragraph("```rust").unwrap())
        .unwrap();
    ws.insert(
        Parent::Node(root),
        Node::paragraph("# not a heading").unwrap(),
    )
    .unwrap();

    let text = message_text(&ws, root).unwrap();

    assert_eq!(
        text,
        format!("Compare my_var * 2 with [[koru://doc/{target}]]\n```rust\n# not a heading")
    );
}

/// Mixed right-to-left and left-to-right text is bytes to the exporter: a
/// message in Arabic with a Latin word round-trips exactly.
#[test]
fn a_message_keeps_mixed_arabic_and_latin_exactly() {
    let (mut ws, root) = with_doc("");
    ws.insert(
        Parent::Node(root),
        Node::paragraph("مرحبا Koru، كيف حالك؟").unwrap(),
    )
    .unwrap();
    assert_eq!(message_text(&ws, root).unwrap(), "مرحبا Koru، كيف حالك؟");
}

#[test]
fn canvas_is_commented_out() {
    let (mut ws, root) = with_doc("C");
    let canvas = Node::new(
        Kind::Canvas,
        Content::blob(BlobHash("b3:dead".into()), "image/png", None, None),
    )
    .unwrap();
    ws.insert(Parent::Node(root), canvas).unwrap();
    let md = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(md.contains("<!-- canvas omitted -->"));
}

struct OneBlob {
    hash: String,
    bytes: Vec<u8>,
}

impl BlobSource for OneBlob {
    fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, ExportError> {
        if hash.0 == self.hash {
            Ok(self.bytes.clone())
        } else {
            Err(ExportError::new("missing"))
        }
    }
}

#[test]
fn image_writes_sidecar_and_markdown_link() {
    let hash = BlobHash("b3:abc123def456".into());
    let (mut ws, root) = with_doc("Pic");
    let mut img = Node::image(hash.clone(), "image/png", Some(10), Some(10)).unwrap();
    img.meta = NodeMeta {
        name: Some("logo".into()),
        ..Default::default()
    };
    ws.insert(Parent::Node(root), img).unwrap();

    let blobs = OneBlob {
        hash: hash.0.clone(),
        bytes: vec![1, 2, 3, 4],
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Pic.md");
    let doc = ExportDocument {
        id: koru_core::DocumentId::new(),
        title: "Pic".into(),
        doc: ws,
    };
    let result = MarkdownExporter
        .export(&[doc], &path, &blobs, &NoProgress)
        .unwrap();
    assert_eq!(result.files.len(), 1);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("![logo](./Pic.assets/"), "{text}");
    let assets: Vec<_> = fs::read_dir(dir.path().join("Pic.assets"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(assets.len(), 1);
    assert_eq!(fs::read(&assets[0]).unwrap(), vec![1, 2, 3, 4]);
}

#[test]
fn multi_export_names_unique_files_in_folder() {
    let (ws_a, _) = with_doc("Same");
    let (ws_b, _) = with_doc("Same");
    let dir = tempfile::tempdir().unwrap();
    let docs = [
        ExportDocument {
            id: koru_core::DocumentId::new(),
            title: "Same".into(),
            doc: ws_a,
        },
        ExportDocument {
            id: koru_core::DocumentId::new(),
            title: "Same".into(),
            doc: ws_b,
        },
    ];
    let result = MarkdownExporter
        .export(&docs, dir.path(), &NoBlobs, &NoProgress)
        .unwrap();
    assert_eq!(result.files.len(), 2);
    assert!(dir.path().join("Same.md").exists());
    assert!(dir.path().join("Same (2).md").exists());
}

// ------------------------------------------------------------------ P1.0: the run pill

/// **A delegation pill exports as a document link naming the run — fresh and
/// reopened alike.**
///
/// Every read decodes the stored record, so what the exporter sees is whatever
/// the reader hands it. P1.0 measured the bare-node spelling `koru://<run>`,
/// because a build that knew no `koru.run` role decoded the record as
/// `AttrValue::Ref`. P1.15 put `koru.run` in
/// `attributes::DOCUMENT_REFERENCE_ROLES`, so the reader hands back the typed
/// document reference and the exporter spells `koru://doc/<run>` — the same
/// spelling the `document` role gets, and the one `koru_link` parses back into
/// a document. **The stored bytes are identical either way**; only the text a
/// human-readable export writes has changed, and an older build exporting the
/// same document still writes the bare-node form, which names the same run.
#[test]
fn a_run_pill_exports_as_a_document_link_naming_the_run() {
    let (mut ws, root) = with_doc("Plan");
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("Delegated ").unwrap())
        .unwrap();
    let run = koru_core::DocumentId::new();
    ws.insert_pill(
        line,
        "Delegated ".len(),
        &Attr::new(
            "koru.run",
            AttrValue::Document(koru_core::DocumentReference::new(run)),
        ),
    )
    .unwrap();

    let expected = format!("[[koru://doc/{}]]", run.as_uuid());
    let fresh = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(fresh.contains(&expected), "{fresh}");

    let reopened = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
    let later = render_document(&reopened, &NoBlobs, "x.assets")
        .unwrap()
        .markdown;
    assert_eq!(later, fresh, "a reopened document exports the same text");

    // The `document` role spells the same link, which is the point: both roles
    // are document references and both export as one.
    let (mut ws, root) = with_doc("Plan");
    let line = ws
        .insert(Parent::Node(root), Node::paragraph("See ").unwrap())
        .unwrap();
    ws.insert_pill(
        line,
        "See ".len(),
        &Attr::new(
            "document",
            AttrValue::Document(koru_core::DocumentReference::new(run)),
        ),
    )
    .unwrap();
    let document_role = render_document(&ws, &NoBlobs, "x.assets").unwrap().markdown;
    assert!(
        document_role.contains(&format!("[[koru://doc/{}]]", run.as_uuid())),
        "{document_role}"
    );
}
