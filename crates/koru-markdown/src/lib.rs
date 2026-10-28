// SPDX-License-Identifier: MPL-2.0

//! Read-only Markdown layout and drawing using Koru design tokens.
//!
//! This crate knows no nodes, workspaces, chats, or editing. Product surfaces
//! retain their own behavior and consume the shared presentation result.

use std::borrow::Cow;
use std::ops::Range;

use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use parley::Layout;
use parley::layout::{Alignment, AlignmentOptions};
use parley::style::{FontFamily, FontStyle, FontWeight, LineHeight, OverflowWrap, StyleProperty};

pub mod table;

#[derive(Copy, Clone)]
enum StyleRole {
    Ui,
    Document,
}

#[derive(Copy, Clone)]
pub struct Styles {
    role: StyleRole,
}

impl Styles {
    pub const fn ui() -> Self {
        Self {
            role: StyleRole::Ui,
        }
    }
    pub const fn document() -> Self {
        Self {
            role: StyleRole::Document,
        }
    }
    fn metrics(self) -> theme::TextMetrics {
        match self.role {
            StyleRole::Ui => theme::role::ui().metrics,
            StyleRole::Document => theme::role::body().metrics,
        }
    }
    fn stack(self) -> &'static str {
        match self.role {
            StyleRole::Ui => theme::ui_stack(),
            StyleRole::Document => theme::body_font().stack(),
        }
    }
    fn weight(self) -> f32 {
        match self.role {
            StyleRole::Ui => theme::ui_font().weight_for(None),
            StyleRole::Document => theme::body_font().weight(),
        }
    }
}

#[derive(Clone)]
pub struct LinkSpan {
    pub range: Range<usize>,
    pub url: String,
}

pub enum Rendered {
    Text {
        text: String,
        layout: Layout<Brush>,
        links: Vec<LinkSpan>,
    },
    Table(table::TableGrid),
    Image {
        url: String,
        alt: String,
    },
    Rule,
}

pub fn render_block(
    engine: &mut TextEngine,
    markdown: &str,
    max_w: f32,
    styles: &Styles,
) -> Rendered {
    let Some(block) = koru_import_markdown::parse(markdown).into_iter().next() else {
        return text_layout(engine, String::new(), max_w, styles, Vec::new(), None);
    };
    render_parsed(engine, block, max_w, styles)
}

/// The same drawing, for a caller that has already parsed.
///
/// A surface showing a whole Markdown document parses it once and lays out
/// only the blocks that are on screen; parsing each block again here would
/// make one document's layout cost a parse per block per resize. The split
/// is where it is so that this and [`render_block`] stay **one** renderer:
/// there is no second place deciding what a heading, a bullet or a quote
/// looks like.
pub fn render_parsed(
    engine: &mut TextEngine,
    block: koru_import_markdown::Block,
    max_w: f32,
    styles: &Styles,
) -> Rendered {
    use koru_import_markdown::{Block, CalloutLine, TextKind};

    let Block::Text(block) = block else {
        return match block {
            Block::Table(source) => {
                table::layout(engine, &source, max_w, table::Sizing::Fit, styles)
                    .map(Rendered::Table)
                    .unwrap_or_else(|_| {
                        text_layout(engine, source, max_w, styles, Vec::new(), None)
                    })
            }
            Block::Image(image) => Rendered::Image {
                url: image.destination,
                alt: image.alt,
            },
            Block::Rule => Rendered::Rule,
            // A box's edges draw as empty lines, which is what sets the box
            // off to the eye; the lines inside carry their place in it, and
            // the first its icon.
            Block::CalloutStart { .. } | Block::CalloutEnd => {
                text_layout(engine, String::new(), max_w, styles, Vec::new(), None)
            }
            Block::Text(_) => unreachable!(),
        };
    };

    let mut prefix = String::new();
    // The first line of a callout box leads with its icon; the lines after
    // it sit in under that.
    match &block.callout {
        Some(CalloutLine::First { icon }) => {
            prefix.push_str(koru_core::callout::icon_of(icon.as_deref()));
            prefix.push(' ');
        }
        Some(CalloutLine::Inside) => prefix.push_str("   "),
        None => {}
    }
    if let Some(list) = block.list {
        prefix.push_str(&"  ".repeat(list.depth as usize));
        prefix.push_str(match block.task {
            Some(true) => "\u{2611} ",
            Some(false) => "\u{2610} ",
            None => "\u{2022} ",
        });
    }
    if matches!(block.kind, TextKind::Quote) {
        prefix.push_str("\u{2502} ");
    }
    let prefix_len = prefix.len();
    let text = prefix + &block.text;
    let links = block
        .links
        .iter()
        .map(|link| LinkSpan {
            range: prefix_len + link.start..prefix_len + link.end,
            url: link.url.clone(),
        })
        .collect::<Vec<_>>();
    text_layout(
        engine,
        text,
        max_w,
        styles,
        links,
        Some((block, prefix_len)),
    )
}

fn text_layout(
    engine: &mut TextEngine,
    text: String,
    max_w: f32,
    styles: &Styles,
    links: Vec<LinkSpan>,
    parsed: Option<(koru_import_markdown::TextBlock, usize)>,
) -> Rendered {
    use koru_import_markdown::TextKind;
    let (size, line_height, stack, weight) = if let Some((block, _)) = parsed.as_ref() {
        if matches!(block.kind, TextKind::Code { .. }) {
            let style = theme::role::ui();
            (
                style.metrics.size,
                style.metrics.line_height,
                theme::code_stack(),
                theme::code_font().weight_for(None),
            )
        } else if let Some(level) = block.heading.map(|heading| heading.level()) {
            let style = theme::role::section_heading();
            (
                style.metrics.size,
                style.metrics.line_height,
                theme::headings_stack(),
                theme::headings_font().weight_for(Some(level)),
            )
        } else {
            let metrics = styles.metrics();
            (
                metrics.size,
                metrics.line_height,
                styles.stack(),
                styles.weight(),
            )
        }
    } else {
        let metrics = styles.metrics();
        (
            metrics.size,
            metrics.line_height,
            styles.stack(),
            styles.weight(),
        )
    };
    let mut builder = engine
        .layout_cx
        .ranged_builder(&mut engine.font_cx, &text, 1.0, true);
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(stack),
    )));
    builder.push_default(StyleProperty::FontSize(size));
    builder.push_default(StyleProperty::LineHeight(LineHeight::Absolute(line_height)));
    builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
    koru_draw::text::push_axes(&mut builder, size, weight);
    builder.push_default(StyleProperty::Brush(Brush(theme::p().text)));
    if let Some((block, prefix_len)) = parsed {
        for run in block.runs {
            let range = prefix_len + run.start..prefix_len + run.end;
            if run.marks.bold {
                builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
            }
            if run.marks.italic {
                builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            }
            if run.marks.underline {
                builder.push(StyleProperty::Underline(true), range.clone());
            }
            if run.marks.strike {
                builder.push(StyleProperty::Strikethrough(true), range.clone());
            }
            if run.marks.code {
                builder.push(
                    StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
                        theme::code_stack(),
                    ))),
                    range,
                );
            }
        }
    }
    for link in &links {
        builder.push(StyleProperty::Underline(true), link.range.clone());
        builder.push(
            StyleProperty::Brush(Brush(theme::p().accent)),
            link.range.clone(),
        );
    }
    let mut layout = builder.build(&text);
    layout.break_all_lines(Some(max_w));
    layout.align(Alignment::Start, AlignmentOptions::default());
    Rendered::Text {
        text,
        layout,
        links,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_link_offsets_follow_display_text() {
        let mut engine = TextEngine::new();
        let Rendered::Text { text, links, .. } = render_block(
            &mut engine,
            "> [Koru](https://koru.test)",
            300.0,
            &Styles::ui(),
        ) else {
            panic!("text")
        };
        assert_eq!(text, "\u{2502} Koru");
        assert_eq!(&text[links[0].range.clone()], "Koru");
    }

    /// A box's first line leads with its icon, the lines inside it sit in
    /// under that, and the box's edges are empty lines.
    #[test]
    fn a_callout_box_reads_as_its_icon_led_first_line_with_its_lines_inside() {
        use koru_import_markdown::{Block, parse};
        let mut engine = TextEngine::new();
        let blocks = parse("> 💡 Keep [this](https://koru.test)\n>\n> inside\n>\n> - item");
        let texts: Vec<_> = blocks
            .into_iter()
            .map(
                |block| match render_parsed(&mut engine, block, 300.0, &Styles::ui()) {
                    Rendered::Text { text, links, .. } => {
                        let linked = links
                            .first()
                            .map(|link| text[link.range.clone()].to_owned());
                        (text, linked)
                    }
                    _ => panic!("text"),
                },
            )
            .collect();
        assert_eq!(
            texts,
            vec![
                (String::new(), None),
                ("💡 Keep this".to_owned(), Some("this".to_owned())),
                ("   inside".to_owned(), None),
                ("   \u{2022} item".to_owned(), None),
                (String::new(), None),
            ]
        );
        assert!(matches!(
            render_parsed(&mut engine, Block::CalloutEnd, 300.0, &Styles::ui()),
            Rendered::Text { text, .. } if text.is_empty()
        ));
    }

    #[test]
    fn tables_images_rules_and_invalid_tables_keep_their_meaning() {
        let mut engine = TextEngine::new();
        assert!(matches!(
            render_block(&mut engine, "| A |\n| --- |", 200.0, &Styles::ui()),
            Rendered::Table(_)
        ));
        assert!(matches!(
            render_block(&mut engine, "| A | B |\n| --- |", 200.0, &Styles::ui()),
            Rendered::Text { .. }
        ));
        assert!(matches!(
            render_block(
                &mut engine,
                "![alt](https://example.test/a.png)",
                200.0,
                &Styles::ui()
            ),
            Rendered::Image { .. }
        ));
        assert!(matches!(
            render_block(&mut engine, "---", 200.0, &Styles::ui()),
            Rendered::Rule
        ));
    }
}
