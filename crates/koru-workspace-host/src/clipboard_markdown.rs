// SPDX-License-Identifier: MPL-2.0

//! CommonMark clipboard ingestion.
//!
//! Format detection belongs to the platform edge; semantic parsing is shared
//! with file import in `koru-import-markdown` so Koru has one Markdown reader.

use crate::{
    RichBlock, RichListSpec, RichTableBlock, RichTextBlock, RichTextKind, RichTextLink, RichTextRun,
};

pub(crate) fn explicit_markdown() -> Option<(String, String)> {
    let payload = koru_clipboard::system()
        .raw(koru_clipboard::RawFormat::Markdown)
        .ok()
        .flatten()?;
    let text = String::from_utf8(payload.bytes).ok()?;
    (!text.is_empty()).then_some((payload.format, text))
}

pub(crate) fn format_names() -> Vec<String> {
    koru_clipboard::system().format_names().unwrap_or_default()
}

/// Conservative compatibility gate for producers that expose Markdown as
/// plain text only. One structural construct or two inline constructs keep an
/// ordinary asterisk in prose from changing paste behavior.
pub(crate) fn looks_like_markdown(text: &str) -> bool {
    let mut structural = 0_u8;
    let mut inline = 0_u8;
    let mut in_fence = false;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            structural = structural.saturating_add(2);
        }
        if line.starts_with('#') && {
            let count = line.chars().take_while(|ch| *ch == '#').count();
            (1..=6).contains(&count) && line.as_bytes().get(count) == Some(&b' ')
        } {
            structural = structural.saturating_add(2);
        }
        if line.starts_with("> ")
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("+ ")
            || ordered_list_prefix(line)
        {
            structural = structural.saturating_add(2);
        }
        if reference_definition(line) || markdown_link_syntax(line) || table_delimiter_line(line) {
            structural = structural.saturating_add(2);
        }
        inline = inline.saturating_add((line.matches("**").count() >= 2) as u8);
        inline = inline.saturating_add((line.matches("__").count() >= 2) as u8);
        inline = inline.saturating_add((line.matches('`').count() >= 2) as u8);
        inline = inline.saturating_add((line.matches("~~").count() >= 2) as u8);
    }
    structural >= 2 || inline >= 2 || (structural >= 1 && inline >= 1) || in_fence
}

fn table_delimiter_line(line: &str) -> bool {
    let cells: Vec<_> = line.trim().trim_matches('|').split('|').collect();
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let dashes = cell.trim().trim_matches(':');
            !dashes.is_empty() && dashes.bytes().all(|byte| byte == b'-')
        })
}

fn ordered_list_prefix(line: &str) -> bool {
    let digits = line
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    digits > 0
        && matches!(line.as_bytes().get(digits), Some(b'.' | b')'))
        && line.as_bytes().get(digits + 1) == Some(&b' ')
}

fn markdown_link_syntax(line: &str) -> bool {
    line.find('[').is_some_and(|open| {
        line[open + 1..]
            .find(']')
            .is_some_and(|close| matches!(line.as_bytes().get(open + close + 2), Some(b'(' | b'[')))
    })
}

fn reference_definition(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with('[')
        && line
            .find("]: ")
            .or_else(|| line.find("]:"))
            .is_some_and(|close| close > 1)
}

/// Parse Markdown into the shell's rich-paste transport using the same
/// semantic blocks the file importer consumes.
pub fn parse(text: &str) -> Vec<RichBlock> {
    koru_import_markdown::parse(text)
        .into_iter()
        .map(|block| match block {
            koru_import_markdown::Block::Table(source) => {
                RichBlock::Table(RichTableBlock { source })
            }
            koru_import_markdown::Block::Rule => RichBlock::Text(RichTextBlock::from_text("—")),
            koru_import_markdown::Block::CalloutStart { icon, tint } => {
                RichBlock::CalloutStart { icon, tint }
            }
            koru_import_markdown::Block::CalloutEnd => RichBlock::CalloutEnd,
            koru_import_markdown::Block::Image(image) => {
                let label = if image.alt.is_empty() {
                    image.destination.clone()
                } else {
                    image.alt
                };
                let end = label.len();
                RichBlock::Text(RichTextBlock {
                    text: label,
                    links: (!image.destination.is_empty())
                        .then(|| RichTextLink {
                            start: 0,
                            end,
                            url: image.destination,
                        })
                        .into_iter()
                        .collect(),
                    ..Default::default()
                })
            }
            koru_import_markdown::Block::Text(block) => {
                let task_prefix = block
                    .task
                    .map(|done| if done { "[x] " } else { "[ ] " })
                    .unwrap_or("");
                let shift = task_prefix.len();
                RichBlock::Text(RichTextBlock {
                    text: format!("{task_prefix}{}", block.text),
                    list: block.list.map(|list| RichListSpec {
                        style: list.style,
                        depth: list.depth,
                    }),
                    heading: block.heading,
                    kind: match block.kind {
                        koru_import_markdown::TextKind::Paragraph => RichTextKind::Paragraph,
                        koru_import_markdown::TextKind::Quote => RichTextKind::Quote,
                        koru_import_markdown::TextKind::Code { language } => {
                            RichTextKind::Code { language }
                        }
                    },
                    runs: block
                        .runs
                        .into_iter()
                        .map(|run| RichTextRun {
                            start: run.start + shift,
                            end: run.end + shift,
                            marks: run.marks,
                        })
                        .collect(),
                    links: block
                        .links
                        .into_iter()
                        .map(|link| RichTextLink {
                            start: link.start + shift,
                            end: link.end + shift,
                            url: link.url,
                        })
                        .collect(),
                })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::{Heading, ListStyle};

    fn text_blocks(markdown: &str) -> Vec<RichTextBlock> {
        parse(markdown)
            .into_iter()
            .filter_map(|block| match block {
                RichBlock::Text(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn conservative_detection_needs_real_markdown_evidence() {
        assert!(!looks_like_markdown("ordinary prose * with punctuation"));
        assert!(!looks_like_markdown("one **bold** phrase"));
        assert!(looks_like_markdown("# Heading"));
        assert!(looks_like_markdown("See [Koru](https://example.com)."));
        assert!(looks_like_markdown("**one** and `two`"));
        assert!(looks_like_markdown("[1]: https://example.com \"Source\""));
        assert!(looks_like_markdown("| --- | ---: |"));
    }

    #[test]
    fn tables_remain_one_source_block_with_references() {
        let blocks = parse(
            "before\n\n| App | Notes |\n| --- | ---: |\n| **Koru** | [Docs][1] |\n\nafter\n\n[1]: https://example.com \"Docs\"",
        );
        let table = blocks
            .iter()
            .find_map(|block| match block {
                RichBlock::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert!(table.source.contains("[1]: https://example.com \"Docs\""));
    }

    /// A callout box pastes as its start marker carrying the look, the
    /// blocks inside it, and the end marker the document nests them by.
    #[test]
    fn a_callout_box_carries_its_start_and_its_end() {
        let blocks = parse("> 💡 Keep it\n>\n> inside\n>\n> - one\n\nafter");
        assert_eq!(blocks.len(), 6);
        assert!(matches!(
            &blocks[0],
            RichBlock::CalloutStart { icon: Some(icon), tint: None } if icon == "💡"
        ));
        assert!(
            matches!(&blocks[1], RichBlock::Text(t) if t.kind == RichTextKind::Paragraph && t.text == "Keep it")
        );
        assert!(
            matches!(&blocks[2], RichBlock::Text(t) if t.kind == RichTextKind::Paragraph && t.text == "inside")
        );
        assert!(
            matches!(&blocks[3], RichBlock::Text(t) if t.list.is_some_and(|l| l.style == ListStyle::Bullet) && t.text == "one")
        );
        assert!(matches!(&blocks[4], RichBlock::CalloutEnd));
        assert!(matches!(&blocks[5], RichBlock::Text(t) if t.text == "after"));
    }

    #[test]
    fn parses_supported_blocks_marks_links_and_code() {
        let blocks = text_blocks(
            "## Heading\n\n1. **Bold** and `code` with [link](https://example.com)\n\n> Quote\n\n```rust\nfn main() {}\n```",
        );
        assert_eq!(blocks[0].heading, Some(Heading::H2));
        assert_eq!(blocks[1].list.unwrap().style, ListStyle::Numbered);
        assert!(blocks[1].runs.iter().any(|run| run.marks.bold));
        assert!(blocks[1].runs.iter().any(|run| run.marks.code));
        assert_eq!(blocks[1].links[0].url, "https://example.com");
        assert_eq!(blocks[2].kind, RichTextKind::Quote);
        assert_eq!(
            blocks[3].kind,
            RichTextKind::Code {
                language: Some("rust".into())
            }
        );
    }
}
