// SPDX-License-Identifier: MPL-2.0

use std::io::Cursor;
use std::path::Path;

use koru_core::node::{AttrValue, Heading, Kind, ListStyle};

use super::*;
use crate::enex::{NoExternalEntityFetcher, parse_enex_content};

const RICH: &[u8] = include_bytes!("../fixtures/body-rich.enex");
const UNKNOWN: &[u8] = include_bytes!("../fixtures/body-unknown.enex");
const EMPTY: &[u8] = include_bytes!("../fixtures/body-empty.enex");

fn convert_fixture(bytes: &[u8]) -> (WorkspaceDoc, Vec<ImportWarning>) {
    let source = Path::new("body.enex");
    let workspace = WorkspaceId::new();
    let mut converted = None;
    parse_enex_content(
        Cursor::new(bytes),
        source,
        &NoExternalEntityFetcher,
        |note, content| {
            converted = Some(convert_note(workspace, &note.title, content, source).unwrap());
            Ok(())
        },
    )
    .unwrap();
    let converted = converted.expect("fixture contains one note");
    (
        WorkspaceDoc::from_snapshot(&converted.snapshot).expect("readable snapshot"),
        converted.warnings,
    )
}

fn convert_enml(enml: &str) -> (WorkspaceDoc, Vec<ImportWarning>) {
    let converted = convert_note(
        WorkspaceId::new(),
        "Checklist",
        enml,
        Path::new("checklist.enex"),
    )
    .expect("convert ENML");
    (
        WorkspaceDoc::from_snapshot(&converted.snapshot).expect("readable snapshot"),
        converted.warnings,
    )
}

fn title_row(document: &WorkspaceDoc) -> NodeId {
    let roots = document.children(Parent::Root).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(document.read(roots[0]).unwrap().kind, Kind::Document);
    roots[0]
}

fn body_rows(document: &WorkspaceDoc) -> Vec<NodeId> {
    document
        .children(Parent::Node(title_row(document)))
        .unwrap()
}

fn row_with_text(document: &WorkspaceDoc, text: &str) -> NodeId {
    body_rows(document)
        .into_iter()
        .find(|row| document.read(*row).unwrap().content.as_text() == Some(text))
        .unwrap_or_else(|| panic!("missing body row {text:?}"))
}

#[test]
fn unicode_paragraphs_divisions_breaks_and_headings_are_native_rows() {
    let (document, warnings) = convert_fixture(RICH);
    assert!(warnings.is_empty());

    row_with_text(&document, "English مرحبا 中文 שלום");
    row_with_text(&document, "first");
    row_with_text(&document, "second");

    let h1 = document
        .read(row_with_text(&document, "Heading one"))
        .unwrap();
    let h6 = document
        .read(row_with_text(&document, "Heading six"))
        .unwrap();
    assert_eq!(h1.meta.heading, Some(Heading::H1));
    assert_eq!(h6.meta.heading, Some(Heading::H6));
}

#[test]
fn inline_marks_use_utf8_byte_ranges_and_only_stable_highlight_tokens() {
    let (document, _) = convert_fixture(RICH);
    let row = body_rows(&document)
        .into_iter()
        .find(|row| {
            document
                .read(*row)
                .unwrap()
                .content
                .as_text()
                .is_some_and(|text| text.contains("bold世界"))
        })
        .expect("formatted paragraph");
    let runs = document.runs(row).unwrap();

    let bold = runs.iter().find(|run| run.text == "bold世界").unwrap();
    assert!(bold.bold);
    assert_eq!(bold.end.get() - bold.start.get(), "bold世界".len());
    assert!(runs.iter().find(|run| run.text == "italic").unwrap().italic);
    assert!(
        runs.iter()
            .find(|run| run.text == "under")
            .unwrap()
            .underline
    );
    assert!(runs.iter().find(|run| run.text == "strike").unwrap().strike);
    assert!(runs.iter().find(|run| run.text == "inline").unwrap().code);
    assert_eq!(
        runs.iter()
            .find(|run| run.text == "yellow")
            .unwrap()
            .style
            .highlight
            .as_deref(),
        Some("yellow")
    );
    let text = document
        .read(row)
        .unwrap()
        .content
        .as_text()
        .unwrap()
        .to_owned();
    let plain = text.find("plain").unwrap();
    assert_eq!(
        document
            .style_in(row, plain..plain + "plain".len())
            .unwrap()
            .highlight,
        None
    );
    let unstyled = text.find("unstyled").unwrap();
    let unstyled = unstyled..unstyled + "unstyled".len();
    assert!(document.marks_in(row, unstyled.clone()).unwrap().is_empty());
    assert!(document.style_in(row, unstyled).unwrap().is_empty());
}

#[test]
fn nested_ordered_and_unordered_lists_keep_native_structure() {
    let (document, _) = convert_fixture(RICH);
    let bullet = body_rows(&document)
        .into_iter()
        .find(|row| document.read(*row).unwrap().kind == Kind::ListBlock)
        .expect("bullet list");
    assert_eq!(
        document.read(bullet).unwrap().meta.list_style,
        Some(ListStyle::Bullet)
    );
    let items = document.children(Parent::Node(bullet)).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(
        document.read(items[0]).unwrap().content.as_text(),
        Some("bullet one")
    );
    assert_eq!(
        document.read(items[1]).unwrap().content.as_text(),
        Some("bullet two")
    );
    let nested = document.children(Parent::Node(items[0])).unwrap();
    assert_eq!(nested.len(), 1);
    assert_eq!(document.read(nested[0]).unwrap().kind, Kind::ListBlock);
    assert_eq!(
        document.read(nested[0]).unwrap().meta.list_style,
        Some(ListStyle::Numbered)
    );
    let nested_items = document.children(Parent::Node(nested[0])).unwrap();
    assert_eq!(
        document.read(nested_items[0]).unwrap().content.as_text(),
        Some("nested number")
    );
}

#[test]
fn quote_code_and_divider_use_their_semantic_kinds() {
    let (document, _) = convert_fixture(RICH);
    let rows = body_rows(&document);
    let quote = rows
        .iter()
        .find(|row| document.read(**row).unwrap().kind == Kind::Quote)
        .unwrap();
    assert_eq!(
        document.read(*quote).unwrap().content.as_text(),
        Some("quoted text")
    );
    let codes: Vec<_> = rows
        .iter()
        .filter(|row| document.read(**row).unwrap().kind == Kind::Code)
        .map(|row| {
            document
                .read(*row)
                .unwrap()
                .content
                .as_text()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(codes, ["let x = 1;\nline two", "block code"]);
    assert!(
        rows.iter()
            .any(|row| document.read(*row).unwrap().kind == Kind::Divider)
    );
}

#[test]
fn http_and_mailto_become_link_pills_without_changing_targets() {
    let (document, _) = convert_fixture(RICH);
    let links: Vec<_> = body_rows(&document)
        .into_iter()
        .flat_map(|row| document.read(row).unwrap().content.attrs().to_vec())
        .filter(|attr| attr.role == "link")
        .collect();
    assert_eq!(links.len(), 3);
    assert!(links.iter().any(|attr| {
        attr.label.as_deref() == Some("Web site")
            && attr.value == AttrValue::Url("https://example.com/path".into())
    }));
    assert!(links.iter().any(|attr| {
        attr.label.as_deref() == Some("Mail us")
            && attr.value == AttrValue::Url("mailto:person@example.com".into())
    }));
    assert!(links.iter().any(|attr| {
        attr.label.as_deref() == Some("Evernote")
            && attr.value == AttrValue::Url("evernote:///view/1/s1/guid/guid/".into())
    }));
}

#[test]
fn hidden_content_is_absent_from_text_and_indexable_rows() {
    let (document, _) = convert_fixture(RICH);
    let all_text = body_rows(&document)
        .into_iter()
        .filter_map(|row| {
            document
                .read(row)
                .unwrap()
                .content
                .as_text()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("|");
    assert!(!all_text.contains("secret text"));
    assert!(all_text.contains("visible text"));
}

#[test]
fn unknown_wrappers_recurse_while_phase_eight_elements_and_unsafe_links_stay_safe() {
    let (document, warnings) = convert_fixture(UNKNOWN);
    let text = body_rows(&document)
        .into_iter()
        .filter_map(|row| {
            document
                .read(row)
                .unwrap()
                .content
                .as_text()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>()
        .join("|");

    assert!(text.contains("wrapper text"));
    assert!(text.contains("leaf text"));
    assert!(text.contains("safe caption"));
    assert!(text.contains("later table"));
    assert!(text.contains("Imported encrypted content"));
    assert!(!text.contains("later cipher"));
    assert!(
        body_rows(&document).into_iter().all(|row| document
            .read(row)
            .unwrap()
            .content
            .attrs()
            .is_empty())
    );
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| { warning.code == Some(ImportWarningCode::EvernoteUnknownElement) })
            .count(),
        2
    );
}

#[test]
fn empty_en_note_is_a_valid_readable_document() {
    let (document, warnings) = convert_fixture(EMPTY);
    assert!(warnings.is_empty());
    assert!(body_rows(&document).is_empty());
    assert_eq!(
        document
            .read(title_row(&document))
            .unwrap()
            .content
            .as_text(),
        Some("Empty note")
    );
}

#[test]
fn leading_checked_and_unchecked_todos_are_native_checkbox_items() {
    let (document, warnings) = convert_enml(
        r#"<en-note><div><en-todo checked="true"/><strong>done 世界</strong></div><p><en-todo checked="false"/>open</p></en-note>"#,
    );
    assert!(warnings.is_empty());

    let blocks: Vec<_> = body_rows(&document)
        .into_iter()
        .filter(|row| document.read(*row).unwrap().kind == Kind::ListBlock)
        .collect();
    assert_eq!(blocks.len(), 2);
    for block in &blocks {
        assert_eq!(
            document.read(*block).unwrap().meta.list_style,
            Some(ListStyle::Checkbox)
        );
    }
    let checked = document.children(Parent::Node(blocks[0])).unwrap()[0];
    let unchecked = document.children(Parent::Node(blocks[1])).unwrap()[0];
    assert_eq!(document.read(checked).unwrap().kind, Kind::Paragraph);
    assert_eq!(
        document.read(checked).unwrap().content.as_text(),
        Some("done 世界")
    );
    assert!(document.read(checked).unwrap().meta.done);
    assert!(document.runs(checked).unwrap()[0].bold);
    assert_eq!(
        document.read(unchecked).unwrap().content.as_text(),
        Some("open")
    );
    assert!(!document.read(unchecked).unwrap().meta.done);
    assert!(
        body_rows(&document)
            .into_iter()
            .all(|row| document.read(row).unwrap().kind != Kind::Task)
    );
}

#[test]
fn mid_paragraph_todo_keeps_a_glyph_and_warns() {
    let (document, warnings) =
        convert_enml(r#"<en-note><p>before <en-todo checked="true"/><em>after</em></p></en-note>"#);
    let row = row_with_text(&document, "before ☑after");
    assert!(
        document
            .runs(row)
            .unwrap()
            .iter()
            .any(|run| run.text == "after" && run.italic)
    );
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].code,
        Some(ImportWarningCode::InlineCheckboxSimplified)
    );
}

#[test]
fn two_by_two_table_is_one_native_gfm_table() {
    let (document, warnings) = convert_enml(
        "<en-note><table><tr><td>A</td><td>B</td></tr><tr><td>C</td><td>D</td></tr></table></en-note>",
    );
    assert!(warnings.is_empty());

    let table = body_rows(&document)
        .into_iter()
        .find(|row| document.read(*row).unwrap().kind == Kind::Table)
        .expect("native table");
    assert_eq!(
        document.read(table).unwrap().content.as_text(),
        Some("| A | B |\n| --- | --- |\n| C | D |")
    );
}

#[test]
fn merged_table_cells_keep_every_text_and_report_simplification() {
    let (document, warnings) = convert_enml(
        "<en-note><table><tr><td rowspan=\"2\">kept one</td><td>kept two</td></tr><tr><td colspan=\"2\">kept three</td><td>kept four</td></tr></table></en-note>",
    );
    let table = body_rows(&document)
        .into_iter()
        .find(|row| document.read(*row).unwrap().kind == Kind::Table)
        .unwrap();
    let source = document
        .read(table)
        .unwrap()
        .content
        .as_text()
        .unwrap()
        .to_owned();
    for text in ["kept one", "kept two", "kept three", "kept four"] {
        assert!(source.contains(text), "missing {text:?} from {source:?}");
    }
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning.code == Some(ImportWarningCode::EvernoteMergedTableCells))
            .count(),
        1
    );
}

#[test]
fn encrypted_content_is_an_exact_placeholder_and_never_exposes_hint_or_payload() {
    let (document, warnings) = convert_enml(
        "<en-note><p>before</p><en-crypt hint=\"private hint\">ciphertext payload</en-crypt><p>after</p></en-note>",
    );
    assert!(warnings.is_empty());
    let text = body_rows(&document)
        .into_iter()
        .filter_map(|row| {
            document
                .read(row)
                .unwrap()
                .content
                .as_text()
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    assert!(text.iter().any(|text| text == "Imported encrypted content"));
    let joined = text.join("|");
    assert!(!joined.contains("private hint"));
    assert!(!joined.contains("ciphertext payload"));
}

#[test]
fn hidden_modern_task_payload_warns_but_visible_text_stays_a_paragraph() {
    let (document, warnings) = convert_enml(
        "<en-note><div style=\"display:none\" data-evernote-task-id=\"opaque\">private payload</div><p>Readable task text</p></en-note>",
    );
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| {
                warning.code == Some(ImportWarningCode::EvernoteTaskPayloadUnsupported)
            })
            .count(),
        1
    );
    row_with_text(&document, "Readable task text");
    assert!(
        body_rows(&document)
            .into_iter()
            .all(|row| document.read(row).unwrap().kind != Kind::Task)
    );
    assert!(!body_rows(&document).into_iter().any(|row| {
        document
            .read(row)
            .unwrap()
            .content
            .as_text()
            .is_some_and(|text| text.contains("private payload"))
    }));
}

#[test]
fn evernote_link_guid_accepts_the_standard_repeated_route_only_once() {
    let guid = "01234567-89ab-cdef-0123-456789abcdef";
    assert_eq!(
        evernote_link_guid(&format!("evernote:///view/1/s1/{guid}/{guid}/")),
        Some(guid.into())
    );
    assert_eq!(
        evernote_link_guid("evernote:///view/1/s1/not-a-guid/"),
        None
    );
}

/// Evernote's callout is a `div` marked `--en-callout`, its icon in
/// `--en-emoji`; an older one has no emoji and means the 💡 default. It
/// imports as a box carrying the icon and no text of its own, with
/// everything inside the div — a first line, a second, a list, a heading —
/// under the box in order, and the text around the div stays prose.
#[test]
fn an_evernote_callout_imports_as_a_box_with_its_lines_inside() {
    let (document, warnings) = convert_enml(
        r#"<en-note><div>before</div><div style="--en-callout:true; --en-emoji:🤖;--en-requiredFeatures:&quot;[\&quot;callout\&quot;]&quot;;"><div>First <b>line</b></div><div>Second line</div><ul><li>one</li><li>two</li></ul><h2>Inside heading</h2></div><div style="--en-callout:true;"><div>Default icon</div></div><div style="--en-callout:true;"><ul><li>only a list</li></ul></div><div>after</div></en-note>"#,
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let shape = |rows: Vec<NodeId>| -> Vec<(Kind, String, Option<String>)> {
        rows.into_iter()
            .map(|row| {
                let node = document.read(row).unwrap();
                (
                    node.kind,
                    node.content.as_text().unwrap_or_default().to_owned(),
                    node.meta.icon,
                )
            })
            .collect()
    };
    let body = body_rows(&document);
    assert_eq!(
        shape(body.clone()),
        vec![
            (Kind::Paragraph, "before".into(), None),
            (Kind::Callout, String::new(), Some("🤖".into())),
            (Kind::Callout, String::new(), None),
            (Kind::Callout, String::new(), None),
            (Kind::Paragraph, "after".into(), None),
        ]
    );

    let callout = body[1];
    let inside = document.children(Parent::Node(callout)).unwrap();
    assert_eq!(
        shape(inside.clone()),
        vec![
            (Kind::Paragraph, "First line".into(), None),
            (Kind::Paragraph, "Second line".into(), None),
            (Kind::ListBlock, String::new(), None),
            (Kind::Paragraph, "Inside heading".into(), None),
        ]
    );
    assert_eq!(
        document.read(inside[3]).unwrap().meta.heading,
        Heading::from_level(2)
    );
    let items = shape(document.children(Parent::Node(inside[2])).unwrap());
    assert_eq!(items[0].1, "one");
    assert_eq!(items[1].1, "two");

    // A callout with the default icon around one line.
    assert_eq!(
        shape(document.children(Parent::Node(body[2])).unwrap()),
        vec![(Kind::Paragraph, "Default icon".into(), None)]
    );
    // A callout holding only a list: the list is inside the box, alone.
    let only_list = document.children(Parent::Node(body[3])).unwrap();
    assert_eq!(only_list.len(), 1);
    assert_eq!(document.read(only_list[0]).unwrap().kind, Kind::ListBlock);
    assert!(document.effective_schema_version() >= 5);
}
