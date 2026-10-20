// SPDX-License-Identifier: MPL-2.0

//! Laying out a [`Row`] from its Loro text runs.
//!
//! This replaced a builder that parsed `**bold**` out of a string and walked a
//! `BTreeMap<usize, Pill>` to place inline boxes. The input is now [`TextRun`]s
//! straight from `LoroText::to_delta()` — text plus the marks covering it — so
//! there is no parsing step at all, and a run tagged with a `pill` is exactly
//! where an inline box goes.

use std::borrow::Cow;

use koru_core::AttrId;
use koru_core::node::{Align, Kind};
use koru_core::text::ANCHOR_LEN;
use parley::BoundingBox;
use parley::editing::Cursor;
use parley::layout::{Alignment, AlignmentOptions, PositionedLayoutItem};
use parley::style::{FontFamily, FontStyle, FontWeight, LineHeight, StyleProperty};
use parley::{InlineBox, InlineBoxKind};
use peniko::Color;

use crate::body::Body;
use crate::paragraph::{Paragraph, QUOTE_BAR_W, QUOTE_INSET_X};
use koru_draw::geometry::{Rect, checkbox_x, text_x};
use koru_draw::pill::{PillCache, PillInfo, PillTextStyle, PillVisual};
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};

/// Horizontal room used by a row's authored text and trailing metadata.
///
/// Reminder geometry calls this same function so Full, Narrow, and Page mode
/// agree on the content boundary. Keeping a second margin calculation in the
/// draw path is how reminder pills escaped the document column.
pub(crate) fn text_available_width(row: &Paragraph, viewport_w: f32) -> f32 {
    let mut avail = (viewport_w - row.text_x() - theme::MARGIN_X - theme::AFFORDANCE_W).max(80.0);
    if row.kind == Kind::Code {
        avail = (avail - row.lang_chip_w - 6.0 - COPY_CHIP_SIDE - COPY_CHIP_GAP).max(80.0);
    }
    avail
}

pub(crate) fn text_right(row: &Paragraph, viewport_w: f32) -> f32 {
    row.text_x() + text_available_width(row, viewport_w)
}

/// How many rows this process has shaped, ever.
///
/// **A work counter, and the source of a row's shaping stamp.** Two things want
/// it, and both want the same property — that a number is never handed out
/// twice:
///
/// - A test reads [`shapings`] before and after an interaction and says whether
///   anything was laid out at all. That is the measurement `guide/contributing/PRINCIPLES.md`
///   asks for — a ratio somebody can assert on — where a stopwatch would only
///   say how fast this machine is.
/// - [`Paragraph::shaped_at`] carries the value the row was last given, so a
///   cache beside a view can tell "the layout I already projected" from "a
///   layout that happens to look the same". A per-row generation could not:
///   `Body::rebuild` throws every [`Paragraph`] away, so a fresh row would
///   start again at one and collide with the entry left by the row it replaced.
///
/// Relaxed ordering: nothing synchronises on it, and the only requirement is
/// that two shapings never share a number, which `fetch_add` gives on its own.
static SHAPINGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many rows have been shaped in this process. See [`SHAPINGS`].
pub fn shapings() -> u64 {
    SHAPINGS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The next shaping stamp. One per [`build_row`] call that reaches a row.
fn next_shaping() -> u64 {
    SHAPINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Rebuild one row's parley layout.
pub fn build_row(engine: &mut TextEngine, row: &mut Paragraph, viewport_w: f32) {
    // Stamped before the early returns, not after the shaping: an image row and
    // a divider are *laid out* by this call even though neither shapes text,
    // and a projection cache keyed on the stamp has to see them change too.
    row.shaped = next_shaping();
    // An image row has no text to shape. Its height comes from its aspect ratio
    // in `reflow`, and its pixels from the image cache at draw time.
    if row.image.is_some() {
        row.dirty = false;
        return;
    }
    // A divider has no text either — it is a hairline, not prose. Its height is
    // a fixed band set in `reflow`, and its pixels are one filled rect at draw.
    if row.kind == koru_core::node::Kind::Divider {
        row.dirty = false;
        return;
    }
    // Pills and tabs have to be measured before their boxes are pushed, and measuring
    // needs the engine mutably, so it happens up front in its own pass.
    let (step, weight) = (row.type_step(), row.weight());
    // The language chip is measured up front too, so the code block's text can
    // reserve a right gutter for it — the chip then sits in that gutter on the
    // top line, clear of the code.
    if row.kind == Kind::Code {
        let label = crate::diagram::chip_label(row.language.as_deref());
        row.lang_chip_w = engine.simple_layout(label, theme::role::hint()).width() + 6.0;
    }
    let line_box = step.size * step.line_height;
    let pill_style = PillTextStyle::new(step.size, line_box);
    let tab_width = row
        .text
        .contains('\t')
        .then(|| engine.measure_advance("    ", row.font_choice().stack(), step.size, weight));
    let mut sizes: Vec<(AttrId, f32, f32)> = Vec::new();
    for (id, info) in &row.pills {
        let (w, h) =
            engine.measure_pill_with_style(info.kind, &info.label, info.url.as_deref(), pill_style);
        sizes.push((*id, w, h));
    }
    let size_of = |id: AttrId| {
        sizes
            .iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, w, h)| (*w, *h))
            .unwrap_or((0.0, 0.0))
    };

    // The right margin matches the left one *including* the affordance lane,
    // or a Page-mode sheet would sit visibly off-centre: the lane widened the
    // left edge and nothing widened the right.
    // A code block keeps a right gutter for the copy icon and language chip
    // so the first line wraps before them instead of running under them.
    let avail = text_available_width(row, viewport_w);
    let font_cx = &mut engine.font_cx;
    let layout_cx = &mut engine.layout_cx;

    // Layout text is the document text plus a trailing primary-font ZWSP.
    // Parley inline boxes only contribute *ascent* (bottom on the baseline), so
    // a box strut cannot pin descent. Without a real primary-font cluster,
    // emoji-only lines take Segoe's descent, then jump when a letter adds the
    // body font's — baseline = f(ascent, descent) under absolute line-height.
    // The ZWSP is not stored in `row.text`, so **the layout is three bytes
    // longer than the text** and an index taken *from the layout* — anything
    // parley hands back — may sit that far past the end. Pressing End is the
    // everyday way to get one. `Paragraph::set_selection_from_layout` is the
    // only thing allowed to bridge the two, and `range()`/`caret()` clamp on
    // the way out ([ADR-75](../../../docs/decisions/README.md)).
    //
    // **It has to stay a suffix.** Clamping works only because indices are
    // extended, not shifted; a leading or interior layout-only character would
    // misalign every index and the clamp would hide it.
    let text_len = row.text.len();
    let mut shape = String::with_capacity(text_len + LAYOUT_STRUT.len());
    shape.push_str(&row.text);
    shape.push_str(LAYOUT_STRUT);

    let mut b = layout_cx.ranged_builder(font_cx, &shape, 1.0, true);
    b.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(row.font_choice().stack()),
    )));
    // Size, leading and weight all come from the row's step on the type scale,
    // which is the body's for a paragraph and the ramp's for a heading.
    b.push_default(StyleProperty::FontSize(step.size));
    // Absolute rather than relative to whichever fallback font shaped a run.
    // A colour-emoji-only line otherwise inherits Segoe UI Emoji's taller
    // metrics, then visibly shrinks as soon as normal document text is typed.
    b.push_default(StyleProperty::LineHeight(LineHeight::Absolute(
        step.size * step.line_height,
    )));
    // The row's *own* font decides its weight, not the app's — this is the
    // document side, and it stays that way while per-paragraph fonts and
    // colours grow into real attributes. `push_axes` gets the step's size too,
    // so `opsz` follows: a 30px heading shaped with 16px letterfit is exactly
    // what that function exists to prevent.
    koru_draw::text::push_axes(&mut b, step.size, weight);
    // Only a task shows completion; a paragraph has no such state, and a
    // struck-through document line would be wrong rather than merely odd.
    //
    // **Which completion, though.** This read `row.done` — the checkbox — and
    // SPEC §5 lets the box and the lifecycle disagree: a task with structural
    // children keeps a box that is only a box, so *Complete* from its menu set
    // the canonical lifecycle and left the line unstruck. The lifecycle is the
    // authority everywhere else (the Tasks page dims and strikes from it, the
    // menu offers Complete or Reopen from it), and now here.
    //
    // A checkbox-styled list item that is **not** a task has no lifecycle to
    // read, and its box is the whole of what it means. That arm keeps `done`.
    let struck = row.struck();
    if struck {
        b.push_default(StyleProperty::Brush(Brush(theme::p().text_dim)));
        // parley computes the decoration geometry; `render` fills the rect.
        b.push_default(StyleProperty::Strikethrough(true));
        b.push_default(StyleProperty::StrikethroughBrush(Some(Brush(
            theme::p().text_dim,
        ))));
    } else {
        b.push_default(StyleProperty::Brush(Brush(theme::p().text)));
    }

    // Never paint the layout-only strut character (some faces still emit a
    // glyph for ZWSP).
    b.push(
        StyleProperty::Brush(Brush(Color::TRANSPARENT)),
        text_len..shape.len(),
    );

    // Box strut: pins a full line-box *ascent* so empty carets and the title
    // placeholder share one baseline. Descent is the ZWSP's job (boxes cannot
    // contribute it — bottom sits on the baseline).
    b.push_inline_box(InlineBox {
        id: text_strut_box_id(),
        kind: InlineBoxKind::InFlow,
        index: 0,
        width: 0.0,
        height: line_box,
    });

    for run in &row.runs {
        let range = run.range();
        if run.bold {
            b.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
        }
        if run.italic {
            b.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
        }
        if run.underline {
            b.push(StyleProperty::Underline(true), range.clone());
            b.push(
                StyleProperty::UnderlineBrush(Some(Brush(theme::p().text))),
                range.clone(),
            );
        }
        // A completed row is already struck through as a whole, so a mark on a
        // range would only fight with it.
        if run.strike && !struck {
            b.push(StyleProperty::Strikethrough(true), range.clone());
            b.push(
                StyleProperty::StrikethroughBrush(Some(Brush(theme::p().text))),
                range.clone(),
            );
        }
        if let Some(size) = run.style.size {
            b.push(StyleProperty::FontSize(size), range.clone());
        }
        if let Some(script) = run.style.script {
            // parley has no baseline-shift property, so this asks the font for
            // its own designed superior/inferior glyphs — better typography
            // than scaling and nudging. A font without the feature renders
            // normally: wrong, but never broken.
            b.push(
                StyleProperty::FontFeatures(script.feature().into()),
                range.clone(),
            );
        }
        // Inline code: mono. The background rect behind it is painted in
        // `draw.rs`, the way a highlight band is — parley has no background
        // property.
        if run.code {
            b.push(
                StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(theme::code_stack()))),
                range.clone(),
            );
        }

        let Some(attr) = run.pill else { continue };
        // The anchor is default-ignorable so the shaper should hide it anyway;
        // a transparent brush guarantees it even if a font disagrees.
        b.push(
            StyleProperty::Brush(Brush(Color::TRANSPARENT)),
            range.clone(),
        );
        let (width, height) = size_of(attr);
        b.push_inline_box(InlineBox {
            // The renderer maps a box back to its pill through this id.
            id: attr_box_id(attr),
            kind: InlineBoxKind::InFlow,
            index: run.start.get(),
            width,
            height,
        });
    }

    if let Some(tab_width) = tab_width {
        for (index, _) in row.text.match_indices('\t') {
            let range = index..index + 1;
            // Parley recognises a tab as whitespace but currently shapes its
            // unsupported control glyph. Make that source character a
            // zero-sized transparent run, then give it a four-space in-flow
            // box. Its line height stays at the row's normal absolute height:
            // after a lone tab this is the only run Parley can use for cursor
            // geometry, and a relative line height multiplied by font size
            // zero makes the caret zero-height too. The stored text and every
            // byte index remain the real `\t`.
            b.push(StyleProperty::FontSize(0.0), range.clone());
            b.push(
                StyleProperty::LineHeight(LineHeight::Absolute(step.size * step.line_height)),
                range.clone(),
            );
            b.push(
                StyleProperty::Brush(Brush(Color::TRANSPARENT)),
                range.clone(),
            );
            b.push_inline_box(InlineBox {
                id: tab_box_id(index),
                kind: InlineBoxKind::InFlow,
                index,
                width: tab_width,
                height: 0.0,
            });
        }
    }

    // Syntax colour for a code block: derived from the text and the language at
    // layout time and pushed as foreground brushes — the same path a mark takes
    // without touching the CRDT. Computed by `koru-blocks` so `syntect` is
    // linked in one place; runs after the marks so it wins the foreground on
    // code (weight and other properties from marks still apply).
    if row.kind == Kind::Code {
        for (range, colour) in
            koru_blocks::highlight(&row.text, row.language.as_deref(), theme::is_dark())
        {
            b.push(StyleProperty::Brush(Brush(colour)), range);
        }
    }

    // The dictation transcript, spliced into `row.text` above and drawn as its
    // own run: dimmed and underlined, the way a keyboard IME marks provisional
    // characters. Pushed last so it wins over marks and over a code block's
    // syntax colour — what is on screen there is not authored text yet.
    // Nothing about it reaches Loro ([ADR-248](../../../docs/decisions/ADR-226-250.md#adr-248-voicecomposition--dictation-is-an-ephemeral-composition-and-only-the-final-transcript-is-a-transaction)).
    if let Some(provisional) = &row.composition {
        let span = provisional.layout_range();
        if !span.is_empty() {
            b.push(
                StyleProperty::Brush(Brush(theme::p().text_dim)),
                span.clone(),
            );
            b.push(StyleProperty::Underline(true), span.clone());
            b.push(
                StyleProperty::UnderlineBrush(Some(Brush(theme::p().text_dim))),
                span,
            );
        }
    }

    b.build_into(&mut row.layout, &shape);
    row.layout.break_all_lines(Some(avail));
    // Alignment needs a width to align *against*, which `break_all_lines` has
    // just established.
    let align = match row.align {
        Align::Start => Alignment::Start,
        Align::Center => Alignment::Center,
        Align::End => Alignment::End,
    };
    row.layout.align(align, AlignmentOptions::default());

    if row.kind == Kind::Table {
        let table_width =
            (viewport_w - row.text_x() - theme::MARGIN_X - theme::AFFORDANCE_W).max(120.0);
        row.table = Some(crate::table::TableVisual::build(
            engine,
            &row.text,
            table_width,
            row.table_layout,
        ));
    } else {
        row.table = None;
    }

    // Byte indices may have moved under the selection; parley re-derives it.
    // Clamp off the trailing layout-only ZWSP so carets never sit past `text`.
    row.selection = row.selection.refresh(&row.layout);
    row.clamp_selection_to_text();
    row.dirty = false;
}

/// parley's `InlineBox::id` is a `u64`, and an `AttrId` is a UUID, so the id is
/// a hash rather than the value. Collisions would only matter between two pills
/// on the *same line*, where 64 bits is ample.
pub fn attr_box_id(attr: AttrId) -> u64 {
    let (high, low) = attr.as_uuid().as_u64_pair();
    high ^ low
}

/// A stable ID for a layout-only tab box. It never maps back to a document
/// attribute, so pill painting simply skips it.
fn tab_box_id(index: usize) -> u64 {
    0x5441_4200_0000_0000 | index as u64
}

/// Zero-width space appended only for shaping. Contributes the primary font's
/// ascent *and* descent to every line — what a CSS strut does, and what a
/// parley inline box cannot (boxes only raise ascent).
///
/// **Public because it is not only a private detail of shaping any more.**
/// Anything that reads the *layout's* own cluster ranges — as
/// [`crate::accessibility`] does, because parley's AccessKit projection slices
/// the string it shaped — has to know that the string is three bytes longer
/// than `row.text` and that the extra character is this one. See
/// [`shaped_text`].
pub const LAYOUT_STRUT: &str = "\u{200B}";

/// The exact string [`build_row`] shaped for this row.
///
/// `row.text` plus [`LAYOUT_STRUT`]. Every byte index parley hands back is an
/// index into *this*, not into `row.text` — which is why
/// `Paragraph::set_selection_from_layout` exists and why `caret()`/`range()`
/// clamp. A caller that slices `row.text` with a layout range is off by up to
/// three bytes at the end of every row, and never anywhere else, which is the
/// shape of bug that survives a test suite.
pub fn shaped_text(row: &Paragraph) -> String {
    let mut shaped = String::with_capacity(row.text.len() + LAYOUT_STRUT.len());
    shaped.push_str(&row.text);
    shaped.push_str(LAYOUT_STRUT);
    shaped
}

fn text_strut_box_id() -> u64 {
    0x5354_5255_5400_0000
}

/// How many times [`reflow`] may re-choose the band before giving up for this
/// frame.
///
/// The loop stops on its own the moment a pass shapes nothing, which is what
/// normally happens on the second pass; this only bounds a pathological
/// document. Each pass recovers a fixed *fraction* of the estimate's error —
/// the shaped part of the band is exact afterwards — so convergence is
/// geometric, and eight passes cover an estimate seven times too tall against a
/// worst measured error of 2.5×. Hitting the cap costs a bounded number of
/// (cheap) positioning passes rather than one per row, and the next frame's
/// `reflow` finishes the job because a row shaped now stays shaped.
const MAX_BAND_PASSES: usize = 8;

/// Rebuild dirty rows and recompute every vertical position.
pub fn reflow(
    engine: &mut TextEngine,
    outline: &mut Body,
    viewport_w: f32,
    top_inset: f32,
    scroll: f32,
    viewport_h: f32,
) {
    let span = koru_core::timing::Span::start("reflow");
    let mut shaped = 0usize;
    // Position first with the heights in hand, so the shape band can be found.
    // Unshaped text rows use an estimate; shaped rows their real layout.
    //
    // **Shaping changes the heights the band was chosen from**, so one pass is
    // not enough. `estimated_text_height` assumes a half-em advance; text set
    // in narrow glyphs — or one unbroken token — shapes up to 2.5× *shorter*
    // than that (measured; `an_over_estimated_body_leaves_nothing_unshaped_on_screen`
    // is the case). The band then collapses under the viewport, and rows that
    // were estimated below it rise into view still dirty: 240 px of a 600 px
    // viewport, drawn blank. So re-position, re-choose the band and shape
    // again until a pass finds nothing dirty in it. The common case — an idle
    // frame with no dirty row — still costs exactly the two positioning passes
    // it did before, because the first pass shapes nothing and breaks.
    let mut settled = false;
    for _ in 0..MAX_BAND_PASSES {
        position_rows(outline, viewport_w, top_inset, &engine.pills);
        let band = shape_band(outline, scroll, viewport_h);
        let mut this_pass = 0usize;
        for i in band {
            if outline.rows[i].dirty {
                build_row(engine, &mut outline.rows[i], viewport_w);
                this_pass += 1;
            }
        }
        shaped += this_pass;
        if this_pass == 0 {
            settled = true;
            break;
        }
    }
    // **Only the exhaustion path can leave a stale position.** A pass that
    // shaped nothing positioned against final heights and then found nothing
    // to change them, so its positions are the answer; re-running it would be
    // an O(rows) walk on every idle frame, and this is the function the whole
    // Responsiveness section of AGENTS.md is about not calling twice.
    if !settled {
        position_rows(outline, viewport_w, top_inset, &engine.pills);
    }
    let rows = outline.rows.len();
    span.stop_with(|| format!("{shaped} of {rows} rows shaped"));
}

/// Shape every row — the whole-document layout the tests assert on. The banded
/// [`reflow`](Self) would otherwise leave off-screen rows as estimates.
#[cfg(test)]
pub(crate) fn reflow_all(
    engine: &mut TextEngine,
    outline: &mut Body,
    viewport_w: f32,
    top_inset: f32,
) {
    reflow(engine, outline, viewport_w, top_inset, 0.0, f32::INFINITY);
}

/// Assign every row its `y` and `height`. A shaped row uses its parley layout;
/// a dirty text row (not yet in the shape band) uses an estimate so the scroll
/// position and shape band stay approximately right. Cheap — no shaping here.
fn position_rows(outline: &mut Body, viewport_w: f32, top_inset: f32, pills: &PillCache) {
    // Chrome above the rows — a toolbar, a breadcrumb — pushes them all down.
    let mut y = theme::MARGIN_TOP + top_inset;
    for (i, row) in outline.rows.iter_mut().enumerate() {
        let step = row.type_step();
        // A heading opens space above itself — except as the first row, where
        // there is nothing to be separated from and the gap would only push the
        // opening line away from the chrome above it.
        row.space_above = if i == 0 { 0.0 } else { step.space_above };
        y += row.space_above;
        row.y = y;
        row.height = if row.kind == koru_core::node::Kind::Divider {
            // A hairline drawn centred in a single-line band: tall enough to have
            // air around the rule, short enough not to read as an empty paragraph.
            theme::FONT_SIZE * theme::LINE_HEIGHT
        } else if let Some(img) = &row.image {
            image_row_height(img, viewport_w)
        } else if let Some(table) = &row.table {
            if row.table_source_visible {
                if let Some(error) = &table.error_layout {
                    row.layout.height() + 4.0 + error.height()
                } else {
                    row.layout.height()
                }
            } else if let Some(error) = &table.error_layout {
                row.layout.height() + 4.0 + error.height()
            } else {
                table.grid.as_ref().map_or(0.0, |grid| grid.height)
            }
        } else if let Some(diagram) = &row.diagram {
            // A diagram row is a code row whose height depends on what it is
            // showing this frame: the drawing scaled to the column, the
            // placeholder at the text's height, or the text with its reason
            // and, open, the drawing below. Decided from the mirrored state,
            // never by laying anything out here.
            let text_h = if row.dirty {
                estimated_text_height(row, viewport_w)
            } else {
                row.layout.height().max(step.size * step.line_height)
            };
            diagram_layout(row, diagram, text_h, viewport_w).height
        } else if row.dirty {
            // Not shaped yet: an estimate, refined once the row enters the band.
            estimated_text_height(row, viewport_w)
        } else {
            // The floor is this row's *own* line box, not the body's: an empty
            // H1 is a full-height heading waiting for a word, not a 24px gap.
            row.layout.height().max(step.size * step.line_height)
        };
        // Pill paint uses the full visual height while the layout box reports
        // only ascent (so the baseline stays put). Grow the *row* band to cover
        // the hanging descent — otherwise a click on the lower half of a pill
        // misses the row entirely.
        if !row.pills.is_empty() {
            let mut pill_bottom = 0.0_f32;
            visit_pills(row, pills, |_, _, _, _, y, _, h| {
                pill_bottom = pill_bottom.max(y + h);
            });
            row.height = row.height.max(pill_bottom);
        }
        // Block padding lives outside measured height. A code block's tint and
        // an image need enough room not to paint over neighbouring ink; a table
        // needs a quieter four-pixel edge so adjacent grids do not fuse into
        // one. Keeping it external preserves the exact border for hit-testing.
        let gap_above = if row.kind == Kind::Code {
            theme::CODE_PAD
        } else if row.image.is_some() {
            theme::IMAGE_PAD
        } else if row.kind == Kind::Table {
            theme::TABLE_PAD
        } else {
            0.0
        };
        row.y += gap_above;
        y += gap_above;
        // List styling must not change the document's vertical rhythm. A row
        // toggled between a bullet and prose keeps the same following gap, so
        // extracting a middle item does not move every row beneath it.
        let gap = if row.kind == Kind::Table {
            0.0
        } else {
            theme::TASK_GAP
        };
        y += row.height + gap + gap_above;
        // The title owns the air below it — prose has no depth to separate the
        // name from the first paragraph, so the gap is the separation. It is
        // outside `height` for the same reason a heading's `space_above` is
        // outside its own: `y` stays the first glyph's top and every caret and
        // hit-test keeps measuring from there.
        if row.is_title {
            y += theme::TITLE_GAP_BELOW;
        }
    }
}

/// The rows worth shaping now: the visible band plus a margin above and below,
/// so a scroll never reveals an unshaped row. Positions are already assigned.
///
/// Exactly the rows that intersect `[scroll - margin, scroll + viewport_h +
/// margin]` — no more. It used to return one row *past* the bottom edge, from
/// `last` doubling as an inclusive index and as the `rows.len()` sentinel for
/// "ran off the end"; the `+ 1` was an off-by-one rather than a deliberate
/// safety row, and nothing depended on it. What guards the bottom edge is the
/// whole-viewport margin, and — since ADR-169's estimate can be wrong in either
/// direction — [`reflow`] re-choosing the band after it shapes.
fn shape_band(outline: &Body, scroll: f32, viewport_h: f32) -> std::ops::Range<usize> {
    let rows = &outline.rows;
    if rows.is_empty() {
        return 0..0;
    }
    let margin = viewport_h.max(theme::FONT_SIZE * theme::LINE_HEIGHT * 4.0);
    let top = (scroll - margin).max(0.0);
    let bottom = scroll + viewport_h + margin;
    // The first row whose bottom edge has reached `top`. `rows.len()` when the
    // scroll has run past every row, which makes the range empty.
    let first = rows
        .iter()
        .position(|r| r.y + r.height >= top)
        .unwrap_or(rows.len());
    // Exclusive end: the first row that *starts* below `bottom`, or the end of
    // the body when none does.
    let end = rows[first..]
        .iter()
        .position(|r| r.y > bottom)
        .map_or(rows.len(), |n| first + n);
    first..end
}

/// A rough height for an unshaped text row: enough to place the scrollbar and
/// find the shape band without paying for parley. Refined once the row is shaped.
fn estimated_text_height(row: &Paragraph, viewport_w: f32) -> f32 {
    let step = row.type_step();
    let line_box = step.size * step.line_height;
    let avail = (viewport_w - row.text_x() - theme::MARGIN_X - theme::AFFORDANCE_W).max(80.0);
    // A rough glyph advance of half an em — about two characters per em.
    let chars_per_line = ((avail / step.size) * 2.0).floor().max(1.0) as usize;
    let hard_lines = row.text.bytes().filter(|&b| b == b'\n').count() + 1;
    let wrapped = (row.text.chars().count() + chars_per_line - 1) / chars_per_line;
    hard_lines.max(wrapped).max(1) as f32 * line_box
}

/// How far the title's trailing action sits in from the selection chip.
pub const TITLE_ACTION_INSET: f32 = 8.0;
/// Hit slot for the title's trailing action — the document-context toggle.
pub const TITLE_ACTION_SIDE: f32 = 28.0;

/// The rounded chip drawn behind a block-selected row, in content-column x
/// and the same y the row is painted at (`oy`).
///
/// [`theme::ROW_SELECTED_PAD`] outside the text column on both sides. Inset
/// half a pixel top and bottom so adjacent selected blocks keep a 1px gap.
/// Drawing and the title's trailing action both ask this, so the icon cannot
/// sit past the highlight it is meant to live inside.
pub fn row_selection_rect(row: &Paragraph, layout_w: f32, oy: f32) -> Rect {
    let text_left = text_x(row.depth, row.is_task() || row.list_style.is_some());
    let text_right = layout_w - theme::MARGIN_X - theme::AFFORDANCE_W;
    let left = text_left - theme::ROW_SELECTED_PAD;
    let right = text_right + theme::ROW_SELECTED_PAD;
    Rect::new(left, oy - 0.5, (right - left).max(0.0), row.height + 1.0)
}

/// The trailing title action, inside [`row_selection_rect`], inset from its
/// right edge and centred on its height.
pub fn title_action_slot(chip: Rect) -> Rect {
    let side = TITLE_ACTION_SIDE.min(chip.w).min(chip.h);
    Rect::new(
        chip.x + chip.w - TITLE_ACTION_INSET - side,
        chip.y + (chip.h - side) * 0.5,
        side,
        side,
    )
}

/// The text column a depth-0 block sits in: left of the first glyph, right of
/// the last, and the width between. Matches prose and the code panel — the
/// affordance lane (`+` / grip) is chrome, not content.
fn content_column(viewport_w: f32) -> (f32, f32, f32) {
    let left = theme::MARGIN_X + theme::AFFORDANCE_W;
    let right = viewport_w - theme::MARGIN_X - theme::AFFORDANCE_W;
    let column = (right - left).max(160.0);
    (left, right, column)
}

/// The width and height an image is drawn at. A user resize wins; otherwise the
/// content column at the blob's aspect ratio, capped so a single image cannot
/// swallow the window. Unknown dimensions get a placeholder block.
pub fn image_display_size(image: &crate::paragraph::ImageRef, viewport_w: f32) -> (f32, f32) {
    let (_, _, column) = content_column(viewport_w);
    if let Some((w, h)) = image.display {
        return ((w as f32).min(column), h as f32);
    }
    match (image.width, image.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let dw = (w as f32).min(column);
            let dh = dw * h as f32 / w as f32;
            (dw, dh)
        }
        _ => (column, theme::FONT_SIZE * theme::LINE_HEIGHT * 6.0),
    }
}

/// The image's display box — left x, width, height — placed within the content
/// column according to the block's alignment. Width is clamped to the column, so
/// centring and right-aligning never push it past the margin.
pub fn image_display_box(
    image: &crate::paragraph::ImageRef,
    align: Align,
    viewport_w: f32,
) -> (f32, f32, f32) {
    let (w, h) = image_display_size(image, viewport_w);
    let (left, right, column) = content_column(viewport_w);
    let x = match align {
        Align::Start => left,
        Align::Center => left + (column - w) / 2.0,
        Align::End => (right - w).max(left),
    };
    (x, w, h)
}

/// The height of an image row: its display height, never below a line.
fn image_row_height(image: &crate::paragraph::ImageRef, viewport_w: f32) -> f32 {
    image_display_size(image, viewport_w)
        .1
        .max(theme::FONT_SIZE * theme::LINE_HEIGHT)
}

/// Air between a diagram's text (or its reason line) and the drawing under it.
pub const DIAGRAM_GAP: f32 = 8.0;
/// Air between a diagram's text and the reason line under it — the table's
/// error line keeps the same four pixels.
pub const DIAGRAM_LINE_GAP: f32 = 4.0;
/// How far the *Edit text* control sits in from the code panel's corner.
const DIAGRAM_CONTROL_INSET: f32 = 4.0;

/// Where a diagram's drawing goes, in content-column x and **offsets from the
/// row's own y** — the caller adds `row.y` for hit-testing or the screen y
/// for painting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawingBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// The column scale: at most 1. The painter divides every stroke by it so
    /// a drawing scaled down to fit keeps its line weight.
    pub scale: f32,
}

/// The vertical plan of a diagram row this frame, from the mirrored state.
///
/// One function answers both `position_rows` (the height) and `draw_document`
/// (where each part goes), so the row cannot reserve one height and paint
/// another. Every `y` is an offset from the row's `y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiagramLayout {
    /// The reason line under the text, when the current text does not draw.
    pub reason_y: Option<f32>,
    /// The *Showing the last drawing that worked* label over a retained
    /// drawing.
    pub label_y: Option<f32>,
    /// The drawing — in place of the text while closed, under it while open.
    pub drawing: Option<DrawingBox>,
    pub height: f32,
}

/// The width a diagram may fill: the text column from the row's first glyph
/// to the content column's right edge, with no chip gutter — a picture wears
/// no chips.
pub fn diagram_column_width(row: &Paragraph, viewport_w: f32) -> f32 {
    (viewport_w - theme::MARGIN_X - theme::AFFORDANCE_W - row.text_x()).max(80.0)
}

/// A drawing of `natural` size placed in the row's column at offset `y`:
/// natural size up to the column, scaled down to fit when wider, **never
/// scaled up** — the `image_display_box` rule. Aligned in the column the way
/// the block is.
fn drawing_box(
    row: &Paragraph,
    natural: vello::kurbo::Size,
    y: f32,
    viewport_w: f32,
) -> DrawingBox {
    let column = diagram_column_width(row, viewport_w);
    let (nw, nh) = (natural.width as f32, natural.height as f32);
    let scale = if nw > column && nw > 0.0 {
        column / nw
    } else {
        1.0
    };
    let (w, h) = (nw * scale, nh * scale);
    let left = row.text_x();
    let x = match row.align {
        Align::Start => left,
        Align::Center => left + ((column - w) * 0.5).max(0.0),
        Align::End => (left + column - w).max(left),
    };
    DrawingBox { x, y, w, h, scale }
}

/// Lay a diagram row out vertically: the text (`text_h`, already measured or
/// estimated by the caller), then whatever the state adds under it.
///
/// Closed and drawn, the drawing stands in for the text and the row is as
/// tall as the scaled drawing; closed and pending, the row is the text's
/// height — the placeholder promises the text's room and nothing more. Text
/// visible (open, or closed and failed): the text, the reason line when the
/// current text does not draw, and — open — the label (when the drawing is
/// the last good one) and the drawing. Reason and label are one line in the
/// hint role; nothing here shapes.
pub fn diagram_layout(
    row: &Paragraph,
    state: &crate::diagram::DiagramRowState,
    text_h: f32,
    viewport_w: f32,
) -> DiagramLayout {
    let line = theme::role::hint().metrics.line_height;
    if state.shows_picture() {
        let drawing = state
            .drawing
            .as_ref()
            .map(|prepared| drawing_box(row, prepared.natural_size(), 0.0, viewport_w));
        let height = drawing.map_or(text_h, |drawing| drawing.h);
        return DiagramLayout {
            reason_y: None,
            label_y: None,
            drawing,
            height,
        };
    }
    let mut y = text_h;
    let reason_y = state.reason.is_some().then(|| {
        y += DIAGRAM_LINE_GAP;
        let at = y;
        y += line;
        at
    });
    let (label_y, drawing) = match &state.drawing {
        Some(prepared) => {
            y += DIAGRAM_GAP;
            let label_y = state.last_good.then(|| {
                let at = y;
                y += line + DIAGRAM_LINE_GAP;
                at
            });
            let drawing = drawing_box(row, prepared.natural_size(), y, viewport_w);
            y += drawing.h;
            (label_y, Some(drawing))
        }
        None => (None, None),
    };
    DiagramLayout {
        reason_y,
        label_y,
        drawing,
        height: y,
    }
}

/// The *Edit text* control at a drawn diagram's top-right, in document y and
/// content-column x — inside the code panel's corner, the size of a table's
/// controls. One function for `hover_at`, `mouse_down` and the painter.
pub fn diagram_control_rect(row: &Paragraph, layout_w: f32) -> Rect {
    let side = crate::table::CONTROL_SIZE;
    let right = layout_w - theme::MARGIN_X - theme::AFFORDANCE_W + theme::CODE_PAD;
    Rect::new(
        (right - DIAGRAM_CONTROL_INSET - side).max(row.text_x()),
        row.y - theme::CODE_PAD + DIAGRAM_CONTROL_INSET,
        side,
        side,
    )
}

/// The hit-rect of the language chip at the top-right of a code block, in
/// document y and screen x (the same mix `hover_at` works in). Drawn on hover
/// and clicked to open the language picker. The width wraps the row's label
/// (measured in `build_row`), so the chip is never larger than its text plus
/// padding.
/// The tinted panel behind a code block, in content-column x and the caller's
/// vertical space (screen y when drawing, document y when hit-testing).
///
/// **It stays inside the text column.** It used to start at `MARGIN_X / 2` and
/// run the full width, which put it under the `+` and the drag grip — chrome
/// that belongs to the margin, not inside a block — and left the code itself
/// flush against the tint with nothing between them. It now begins one
/// `CODE_PAD` left of where the glyphs do and ends one right of where they
/// stop, which is what makes the padding *inner* rather than a gap somebody
/// added to the outside.
pub fn code_panel_rect(row: &Paragraph, layout_w: f32, oy: f32) -> Rect {
    let left = row.text_x() - theme::CODE_PAD;
    let right = layout_w - theme::MARGIN_X - theme::AFFORDANCE_W + theme::CODE_PAD;
    Rect::new(
        left,
        oy - theme::CODE_PAD,
        (right - left).max(0.0),
        row.height + theme::CODE_PAD * 2.0,
    )
}

/// The semantic quote rule beside `row`, in the same window-space coordinate
/// system the text painter receives. Its position derives from `text_x`, so a
/// future margin or depth change cannot separate the rule from the prose.
pub fn quote_bar_rect(row: &Paragraph, oy: f32) -> Rect {
    const PAD_ABOVE_INK: f32 = 8.0;
    let ink_top = quote_ink_top(row, oy);
    let top = (ink_top - PAD_ABOVE_INK).max(oy);
    let bottom = oy + row.height;
    Rect::new(
        row.text_x() - QUOTE_INSET_X + 4.0,
        top,
        QUOTE_BAR_W,
        (bottom - top).max(QUOTE_BAR_W),
    )
}

/// Visible top of the first shaped line in `row`.
///
/// A font line box includes leading, and that leading is not necessarily split
/// evenly above and below its ink. Quote rules need the letters, not that
/// asymmetric allocation, or they appear to tower over the first line.
pub(crate) fn quote_ink_top(row: &Paragraph, oy: f32) -> f32 {
    let Some(line) = row.layout.lines().next() else {
        return oy;
    };
    let metrics = line.metrics();
    let mut rise = 0.0f32;
    for run in line.runs() {
        let run = run.metrics();
        rise = rise.max(
            run.cap_height
                .filter(|height| *height > 0.5)
                .unwrap_or(run.ascent),
        );
    }
    if rise <= 0.5 {
        rise = metrics.ascent;
    }
    oy + metrics.baseline - rise
}

/// The hit-rect of the language chip at the top-right of a code block, in
/// document y and content-column x.
///
/// `layout_w` is the **column's** width, not the viewport's: a code block is
/// drawn inside the content column, so a chip measured against the window ends
/// up outside the panel it belongs to in every mode but Full. Both the renderer
/// and `hover_at` pass `ViewMode::effective_w`.
pub fn lang_chip_rect(row: &Paragraph, layout_w: f32) -> Rect {
    let chip_w = row.lang_chip_w.max(24.0);
    let chip_h = CODE_CHIP_H;
    // Flush with the code block panel's right edge (panel right =
    // layout_w - MARGIN_X/2), 4px in — the text has already reserved a gutter
    // so it never reaches here.
    // Inside the panel's own right edge, which is where the eye expects it.
    let right = layout_w - theme::MARGIN_X - theme::AFFORDANCE_W + theme::CODE_PAD;
    let x = (right - 4.0 - chip_w).max(theme::MARGIN_X);
    // `Rect::new` takes a width and a height. It was handed `x + chip_w` and
    // `y + chip_h`, which drew a chip as wide as the page — the grey band past
    // the end of every code block.
    Rect::new(x, row.y + 4.0, chip_w, chip_h)
}

pub const CODE_CHIP_H: f32 = 14.0;
pub const COPY_CHIP_SIDE: f32 = 14.0;
pub const COPY_CHIP_GAP: f32 = 4.0;

/// The copy icon immediately left of [`lang_chip_rect`], same band.
pub fn copy_chip_rect(row: &Paragraph, layout_w: f32) -> Rect {
    let lang = lang_chip_rect(row, layout_w);
    Rect::new(
        lang.x - COPY_CHIP_GAP - COPY_CHIP_SIDE,
        lang.y,
        COPY_CHIP_SIDE,
        lang.h,
    )
}

/// The side of the unfinished-items triangle, and the air before it.
pub const TASK_WARNING_SIDE: f32 = 13.0;
pub const TASK_WARNING_GAP: f32 = 6.0;

/// Where a completed task's **unfinished-items** indicator sits, in document
/// coordinates — content-column x, document y, the same space
/// [`crate::chips::chip_boxes`] answers in.
///
/// `None` on every row that is not SPEC §5's concurrent residue, which is what
/// makes this cheap to call from the row loop and from `hover_at` alike: the
/// early return happens before anything is measured.
///
/// **One function owns this geometry**, called by the renderer and by
/// [`hover_at`](crate::Editing::hover_at), so the triangle is hoverable exactly
/// where it is drawn — the rule in `docs/features/design/DESIGN.md` that
/// [`gutter_affordances`] and `chip_boxes` already follow.
///
/// It trails the row's chips rather than sitting between them and the text: the
/// chip lane is packed left-to-right from the text's advance, so anything placed
/// inside it would either overlap a due chip or have to move one, and a chip's
/// position is `chips.rs`'s to decide. On a row with no chips it lands one
/// [`TASK_WARNING_GAP`] after the text, which is the design's reading.
/// **It is placed from the chips' end, so it moves when they do.** The
/// triangle sits one gap past the last chip's right edge, which is why it needs
/// the same tables the chips are measured with — and why widening a due chip
/// by an icon lane shifts the triangle one lane right on a completed task that
/// has a due. Not a bug; a visible consequence worth having looked at once.
pub fn task_warning_rect(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &crate::chips::TagLookup,
    lists: &crate::chips::ListLookup,
) -> Option<Rect> {
    if !row.task.is_some_and(|t| t.unfinished_children) {
        return None;
    }
    let (line_top, line_h, advance) = match row.layout.lines().last() {
        Some(line) => {
            let m = line.metrics();
            (
                m.block_min_coord,
                m.block_max_coord - m.block_min_coord,
                m.advance,
            )
        }
        None => (0.0, TASK_WARNING_SIDE, 0.0),
    };
    // Past the last chip when there is one — the chips are laid out from the
    // same advance, so asking them where they end is the only way not to
    // duplicate their packing arithmetic here.
    let after_text = row.text_x() + advance;
    let x = crate::chips::chip_boxes(row, engine, tags, lists)
        .last()
        .map(|chip| chip.rect.x + chip.rect.w)
        .unwrap_or(after_text)
        + TASK_WARNING_GAP;
    let y = row.y + line_top + (line_h - TASK_WARNING_SIDE) * 0.5;
    Some(Rect::new(x, y, TASK_WARNING_SIDE, TASK_WARNING_SIDE))
}

/// The two affordances in a row's left lane: the insert `+`, then the drag
/// grip, then the checkbox column.
///
/// **One function owns this geometry**, called by the renderer and by
/// [`hover_at`](crate::Editing::hover_at) alike, so each is clickable exactly
/// where it is drawn — the rule in `docs/features/design/DESIGN.md`, and the same shape
/// `docview::buttons` and `ViewState::scrollbar_geom` have.
///
/// `oy` is the row's vertical origin in whichever space the caller works in:
/// screen y when drawing, document y when hit-testing, the mix
/// [`lang_chip_rect`] already uses.
///
/// Height of the row's first line box — real layout metrics when shaped,
/// otherwise the type step. Shared by gutter affordances, checkbox, bullet.
pub fn first_line_h(row: &Paragraph) -> f32 {
    if let Some(line) = row.layout.lines().next() {
        let h = line.metrics().line_height;
        if h > 0.5 {
            return h;
        }
    }
    let step = row.type_step();
    step.size * step.line_height
}

/// Vertical centre for gutter markers, aligned with text **ink** (x-height /
/// cap) rather than the geometric mid of the line box — so a checkbox sits
/// with the letters, not in the padding band under them.
pub fn first_line_mid_y(row: &Paragraph, oy: f32) -> f32 {
    if let Some(line) = row.layout.lines().next() {
        let m = line.metrics();
        let mut x_height = 0.0f32;
        let mut cap_height = 0.0f32;
        for run in line.runs() {
            let rm = run.metrics();
            if let Some(x) = rm.x_height {
                x_height = x_height.max(x);
            }
            if let Some(c) = rm.cap_height {
                cap_height = cap_height.max(c);
            }
        }
        if x_height > 0.5 {
            return oy + m.baseline - x_height * 0.5;
        }
        if cap_height > 0.5 {
            return oy + m.baseline - cap_height * 0.5;
        }
    }
    oy + first_line_h(row) * 0.5
}

/// Top `y` that centres a box of height `h` on [`first_line_mid_y`].
pub fn center_on_first_line(row: &Paragraph, oy: f32, h: f32) -> f32 {
    first_line_mid_y(row, oy) - h * 0.5
}

/// **Where a row's checkbox answers a press**, in the document's own
/// coordinates. `None` on a row that draws none.
///
/// The drawn square is [`theme::CHECKBOX_SIZE`] at
/// [`checkbox_x`](koru_draw::geometry::checkbox_x); the band around it is three
/// logical pixels wider on every side, because a small box is a hard thing to
/// hit. **One function, two callers** — `Editor::checkbox_rect` resolves a press
/// against it and `koru_editor::accessibility::geometry::checkbox_bounds`
/// publishes it — so a client cannot be told the box is somewhere it does not
/// answer. `draw::draw_checkbox` inks the square *inside* it, from the same two
/// numbers.
pub fn checkbox_band(row: &Paragraph, oy: f32) -> Option<Rect> {
    if !row.is_task() {
        return None;
    }
    const PAD: f32 = 3.0;
    let size = theme::CHECKBOX_SIZE;
    Some(Rect::new(
        checkbox_x(row.depth) - PAD,
        center_on_first_line(row, oy, size) - PAD,
        size + PAD * 2.0,
        size + PAD * 2.0,
    ))
}

/// Both boxes are centred on the row's **first line ink**, so the title's `…`
/// and body `+`/grip sit level with the letters. It is the *first* line rather
/// than the whole row so a paragraph that wraps keeps its affordances beside
/// the line it starts on.
pub fn gutter_affordances(row: &Paragraph, oy: f32) -> (Rect, Rect) {
    let grip_x = (checkbox_x(row.depth) - theme::GRIP_W - 6.0).max(2.0);
    let grip = Rect::new(
        grip_x,
        center_on_first_line(row, oy, theme::GRIP_H),
        theme::GRIP_W,
        theme::GRIP_H,
    );
    let btn = theme::AFFORDANCE_BTN;
    let plus = Rect::new(
        (grip_x - 2.0 - btn).max(0.0),
        center_on_first_line(row, oy, btn),
        btn,
        btn,
    );
    (plus, grip)
}

/// The rows that intersect the visible band, as an index range.
///
/// Drawing costs a glyph run, a set of decorations and a pill visit *per row*,
/// so encoding the whole document to show twenty lines of it made a frame scale
/// with the document rather than with the window. `reflow` has already given
/// every row a `y` and a `height`, so this is a scan — and a scan of a `Vec` is
/// nothing beside the encoding it saves.
///
/// One row of margin either side. The caret has to be drawn, and
/// `scroll_to_caret` keeps it inside the band — but a margin makes that true
/// independently of the scroll clamp rather than because of it, and it costs
/// one row.
pub fn visible_range(outline: &Body, scroll: f32, height: f32) -> std::ops::Range<usize> {
    let rows = &outline.rows;
    if rows.is_empty() {
        return 0..0;
    }
    let (top, bottom) = (scroll, scroll + height);
    let first = rows
        .iter()
        .position(|r| r.y + r.height >= top)
        .unwrap_or(rows.len());
    // `position` from `first`, so a document taller than the band does not walk
    // its whole length looking for the end.
    let last = rows[first..]
        .iter()
        .position(|r| r.y > bottom)
        .map_or(rows.len(), |n| first + n);

    (first.saturating_sub(1)..(last + 1).min(rows.len())).clamp_empty()
}

/// `a..b` with `b` never below `a`, so a range past the end is empty rather
/// than a panic waiting for somebody to index with it.
trait ClampEmpty {
    fn clamp_empty(self) -> Self;
}

impl ClampEmpty for std::ops::Range<usize> {
    fn clamp_empty(self) -> Self {
        let end = self.end.max(self.start);
        self.start..end
    }
}

pub fn content_height(outline: &Body) -> f32 {
    outline
        .rows
        .iter()
        .map(|r| r.y + r.height + theme::MARGIN_TOP)
        .fold(theme::MARGIN_TOP, f32::max)
}

/// Visit every pill in a row with its rect in the row's local coordinates.
///
/// Rendering, hover and click-to-edit all go through this, so what is drawn and
/// what is clickable cannot drift apart. The vertical position is derived from
/// the line's baseline. The inline layout box reports only the pill's ascent;
/// painting and hit-testing use the visual's full height so the descent remains
/// below the shared baseline without shifting the line itself.
pub fn visit_pills(
    row: &Paragraph,
    cache: &PillCache,
    mut f: impl FnMut(AttrId, &PillInfo, &PillVisual, f32, f32, f32, f32),
) {
    let step = row.type_step();
    let style = PillTextStyle::new(step.size, step.size * step.line_height);
    for line in row.layout.lines() {
        let baseline = line.metrics().baseline;
        for item in line.items() {
            let PositionedLayoutItem::InlineBox(bx) = item else {
                continue;
            };
            let Some((attr, info)) = row.pills.iter().find(|(id, _)| attr_box_id(**id) == bx.id)
            else {
                continue;
            };
            let Some(visual) =
                cache.get_with_style(info.kind, &info.label, info.url.as_deref(), style)
            else {
                continue;
            };
            let top = baseline - visual.inline_ascent;
            f(*attr, info, visual, bx.x, top, bx.width, visual.height);
        }
    }
}

/// The pill under a point given in the row's local coordinates.
pub fn pill_at(row: &Paragraph, cache: &PillCache, lx: f32, ly: f32) -> Option<AttrId> {
    // A 1px pad: a leading pill's left edge sits on the text origin, and a
    // bullet/affordance click that grazes it was missing by a device pixel and
    // falling through to caret placement that then drew *after* the pill.
    const PAD: f32 = 1.0;
    let mut hit = None;
    visit_pills(row, cache, |attr, _, _, x, y, w, h| {
        if lx >= x - PAD && lx <= x + w + PAD && ly >= y - PAD && ly <= y + h + PAD {
            hit = Some(attr);
        }
    });
    hit
}

/// Caret box in row-local layout space, corrected at pill boundaries.
///
/// Parley places a caret at the start of a leading inline pill on the pill's
/// *right* edge (the anchor is zero-width and shares index 0 with the baseline
/// strut). With a space or letter before the pill the left edge is reachable;
/// without one — bare paragraph start, or a list item whose text is only a
/// pill — every "before" geometry looked like "after". Pin the x to the pill's
/// left at the anchor and to its right just past it.
pub fn caret_geometry(row: &Paragraph, cache: &PillCache, width: f32) -> BoundingBox {
    let focus = row.selection.focus();
    let at = focus.index().min(row.text.len());
    pin_caret_to_pill(row, cache, at, focus.geometry(&row.layout, width), width)
}

/// Like [`caret_geometry`] for an arbitrary byte index (tests, sticky column).
pub fn caret_geometry_at(row: &Paragraph, cache: &PillCache, at: usize, width: f32) -> BoundingBox {
    let at = at.min(row.text.len());
    let cursor = Cursor::from_byte_index(&row.layout, at, parley::layout::Affinity::Downstream);
    pin_caret_to_pill(row, cache, at, cursor.geometry(&row.layout, width), width)
}

fn pin_caret_to_pill(
    row: &Paragraph,
    cache: &PillCache,
    at: usize,
    mut bb: BoundingBox,
    width: f32,
) -> BoundingBox {
    let w = width.max(0.5) as f64;
    for run in &row.runs {
        let Some(attr) = run.pill else { continue };
        let start = run.start.get();
        let end = start + ANCHOR_LEN;
        if at != start && at != end {
            continue;
        }
        let mut box_x = None;
        visit_pills(row, cache, |id, _, _, x, _, pw, _| {
            if id == attr {
                box_x = Some(if at == start {
                    x as f64
                } else {
                    (x + pw) as f64
                });
            }
        });
        if let Some(x) = box_x {
            bb.x0 = x;
            bb.x1 = x + w;
            break;
        }
    }
    bb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paragraph::ImageRef;
    use koru_draw::geometry::text_x;

    use koru_core::node::{Attr, AttrValue, BlobHash, Node};
    use koru_core::schema::{Parent, WorkspaceDoc};
    use koru_core::{Mark, WorkspaceId};

    fn outline_with(line: &str) -> (WorkspaceDoc, Body, koru_core::NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws.insert(Parent::Root, Node::task(line).unwrap()).unwrap();
        let outline = Body::new(&ws);
        (ws, outline, id)
    }

    /// **A completed task is struck through, and `Kind::Task` is not what
    /// decides it.**
    ///
    /// The rule read `row.done && row.is_task()`, and `is_task()` is
    /// `Kind::Task` or a checkbox-styled list item — it is *does this row draw
    /// a box*. A `/due` on an ordinary paragraph makes a task (SPEC §1: the
    /// marker **or** a due is the whole derivation) and leaves the kind alone,
    /// so completing it ticked nothing anybody could see and the line read
    /// exactly as it had. That is the shape acceptance reported: *when due is
    /// completed the text should be strike through*.
    ///
    /// The lifecycle is the authority on every other surface — the Tasks page
    /// dims and strikes from it, the row menu offers Complete or Reopen from it
    /// — and this was the one place still asking the box.
    #[test]
    fn a_completed_task_is_struck_even_when_its_kind_draws_no_box() {
        let struck_of = Paragraph::struck;

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws
            .insert(Parent::Root, Node::paragraph("pay the invoice").unwrap())
            .unwrap();
        // A due is one of the two ways a task is made, and the one that leaves
        // the kind a paragraph.
        koru_core::tasks::set_due(
            &mut ws,
            id,
            koru_core::time::Temporal::Date(
                koru_core::time::DateOnly::parse("2099-01-02").unwrap(),
            ),
        )
        .expect("a due, which makes it a task");

        let mut body = Body::new(&ws);
        body.refresh(&ws, id);
        let row = body.rows.iter().find(|r| r.id == id).expect("the row");
        assert!(row.task.is_some(), "a due-derived task is a task");
        assert!(!row.is_task(), "and its kind still draws no box");
        assert!(!struck_of(row), "an open task is not struck");

        koru_core::tasks::complete_task(
            &mut ws,
            id,
            koru_core::tasks::LifecycleReason::ExplicitComplete,
            crate::tasks::today(),
        )
        .expect("the canonical command");
        body.refresh(&ws, id);
        let row = body.rows.iter().find(|r| r.id == id).expect("the row");
        assert!(
            row.task.is_some_and(|t| t.completed),
            "the lifecycle says completed"
        );
        assert!(
            struck_of(row),
            "and the line is struck, though nothing here draws a box"
        );
    }

    /// A checkbox-styled list item that is **not** a task has no lifecycle to
    /// read, and its box is the whole of what it means — so that arm keeps
    /// `done`, and a plain paragraph is never struck at all.
    #[test]
    fn a_plain_row_is_never_struck_and_a_bare_checkbox_still_is() {
        let struck_of = Paragraph::struck;

        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let para = ws
            .insert(Parent::Root, Node::paragraph("just prose").unwrap())
            .unwrap();
        let mut body = Body::new(&ws);
        body.refresh(&ws, para);
        let row = body.rows.iter().find(|r| r.id == para).expect("the row");
        assert!(row.task.is_none(), "a paragraph is not a task");
        assert!(!struck_of(row), "and a struck document line would be wrong");
    }

    /// An image row sizes to the content column by its aspect ratio, never to a
    /// text line height — a square picture is taller than a line of prose.
    #[test]
    fn an_image_row_is_sized_by_aspect_ratio() {
        let img = |w, h| ImageRef {
            hash: BlobHash("h".into()),
            mime: "image/png".into(),
            width: Some(w),
            height: Some(h),
            display: None,
        };
        let viewport_w = 1000.0;

        // A 400×400 image displays at its source size (not force-filled to the
        // column), preserving the aspect ratio.
        let h = image_row_height(&img(400, 400), viewport_w);
        assert!(
            (h - 400.0).abs() < 0.5,
            "a 400px image displays at 400px, not the full column: {h}"
        );

        // An 800×200 image is 200px tall.
        assert!((image_row_height(&img(800, 200), viewport_w) - 200.0).abs() < 0.5,);

        // An image wider than the column clamps to the column width, preserving
        // the aspect ratio. The column is the text column (prose flush), not the
        // full width between outer margins.
        let column = viewport_w - 2.0 * (theme::MARGIN_X + theme::AFFORDANCE_W);
        let (dw, dh) = image_display_size(&img(2000, 500), viewport_w);
        assert!(
            (dw - column).abs() < 0.5,
            "clamps to column: {dw} ~= {column}"
        );
        assert!(
            (dh - column * 500.0 / 2000.0).abs() < 0.5,
            "preserves aspect after clamp: {dh}"
        );
    }

    /// A picture whose size is unknown still gets a row, so a not-yet-decoded
    /// remote image has somewhere to land.
    #[test]
    fn an_image_of_unknown_size_keeps_a_row() {
        let unknown = ImageRef {
            hash: BlobHash("h".into()),
            mime: "image/png".into(),
            width: None,
            height: None,
            display: None,
        };
        assert!(image_row_height(&unknown, 1000.0) > 0.0);
    }

    /// A user resize wins over the natural size: the height is whatever they set,
    /// regardless of the blob's aspect ratio.
    #[test]
    fn a_user_resize_overrides_the_natural_height() {
        let mut img = ImageRef {
            hash: BlobHash("h".into()),
            mime: "image/png".into(),
            width: Some(400),
            height: Some(400),
            display: None,
        };
        let natural = image_row_height(&img, 1000.0);
        img.display = Some((300, 120));
        assert!(
            (image_row_height(&img, 1000.0) - 120.0).abs() < 0.5,
            "display height wins, not the natural {natural}"
        );
    }

    /// An image is opaque, so `reflow` leaves air above and below it — the same
    /// pattern a code block uses — or a pill hanging off the line above would
    /// be painted over.
    #[test]
    fn an_image_row_keeps_air_above_and_below() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(Parent::Root, Node::paragraph("above").unwrap())
            .unwrap();
        let img = ws
            .insert(
                Parent::Root,
                Node::image(BlobHash("h".into()), "image/png", Some(200), Some(100)).unwrap(),
            )
            .unwrap();
        ws.insert(Parent::Root, Node::paragraph("below").unwrap())
            .unwrap();
        let mut body = Body::new(&ws);
        let mut engine = TextEngine::new();
        reflow_all(&mut engine, &mut body, 1000.0, 0.0);

        let above = body.rows.iter().find(|r| r.text == "above").unwrap();
        let image = body.rows.iter().find(|r| r.id == img).unwrap();
        let below = body.rows.iter().find(|r| r.text == "below").unwrap();

        let gap_above = image.y - (above.y + above.height);
        assert!(
            gap_above >= theme::IMAGE_PAD + theme::TASK_GAP - 0.5,
            "air above the image: {gap_above}"
        );
        let gap_below = below.y - (image.y + image.height);
        assert!(
            gap_below >= theme::IMAGE_PAD + theme::TASK_GAP - 0.5,
            "air below the image: {gap_below}"
        );
        assert!(
            (image.height - 100.0).abs() < 0.5,
            "pad is outside height, not inside: {}",
            image.height
        );
    }

    #[test]
    fn adjacent_tables_have_an_eight_pixel_boundary_outside_their_heights() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let first = ws
            .insert(
                Parent::Root,
                Node::table("| A |\n| --- |\n| one |").unwrap(),
            )
            .unwrap();
        let second = ws
            .insert(
                Parent::Root,
                Node::table("| B |\n| --- |\n| two |").unwrap(),
            )
            .unwrap();
        let mut body = Body::new(&ws);
        let mut engine = TextEngine::new();
        reflow_all(&mut engine, &mut body, 1000.0, 0.0);

        let first = body.rows.iter().find(|row| row.id == first).unwrap();
        let second = body.rows.iter().find(|row| row.id == second).unwrap();
        let boundary = second.y - (first.y + first.height);
        assert!(
            (boundary - theme::TABLE_PAD * 2.0).abs() < 0.01,
            "table boundary should be 8 px, got {boundary}"
        );
    }

    /// Alignment places the picture within the content column: left where prose
    /// starts, centred, or right where prose ends — never under the affordance
    /// lane.
    #[test]
    fn alignment_places_the_image_in_the_column() {
        use koru_core::node::Align;
        let img = ImageRef {
            hash: BlobHash("h".into()),
            mime: "image/png".into(),
            width: Some(200),
            height: Some(100),
            display: Some((200, 100)),
        };
        let vw = 1000.0;
        let left = theme::MARGIN_X + theme::AFFORDANCE_W;
        let right = vw - theme::MARGIN_X - theme::AFFORDANCE_W;
        let column = right - left;

        let (x, _, _) = image_display_box(&img, Align::Start, vw);
        assert!((x - left).abs() < 0.5, "left flush with prose");

        let (x, _, _) = image_display_box(&img, Align::Center, vw);
        assert!((x - (left + (column - 200.0) / 2.0)).abs() < 0.5, "centred");

        let (x, _, _) = image_display_box(&img, Align::End, vw);
        assert!((x - (right - 200.0)).abs() < 0.5, "right flush with prose");
    }

    /// Many rows, laid out, so the visible band has something to cut.
    fn many_rows(n: usize) -> (WorkspaceDoc, Body, TextEngine) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        for i in 0..n {
            ws.insert(Parent::Root, Node::task(&format!("row {i}")).unwrap())
                .unwrap();
        }
        let mut o = Body::new(&ws);
        let mut engine = TextEngine::new();
        reflow_all(&mut engine, &mut o, 900.0, 0.0);
        (ws, o, engine)
    }

    /// The point of the whole thing: a frame costs what is on screen, not what
    /// is in the document.
    #[test]
    fn only_the_rows_in_the_band_are_visible() {
        let (_ws, o, _e) = many_rows(200);
        let band = 300.0;
        let range = visible_range(&o, 0.0, band);

        assert!(range.start == 0);
        assert!(
            range.len() < 30,
            "a band of {band} shows a handful of rows, not {}",
            range.len()
        );
        // Everything it *did* include really does touch the band, give or take
        // the row of margin either side.
        let last = &o.rows[range.end - 2];
        assert!(last.y <= band, "the last row starts inside the band");
    }

    /// Scrolled into the middle, the range moves with it and no longer starts
    /// at zero — the case that proves rows above are being skipped.
    #[test]
    fn scrolling_moves_the_band() {
        let (_ws, o, _e) = many_rows(200);
        let deep = o.rows[100].y;
        let range = visible_range(&o, deep, 300.0);

        assert!(range.start >= 99, "rows above the fold are skipped");
        assert!(range.contains(&100));
    }

    /// A row straddling the top edge is still drawn — half a row is worse than
    /// a wasted encode.
    #[test]
    fn a_row_crossing_the_top_edge_is_included() {
        let (_ws, o, _e) = many_rows(20);
        // Halfway down row 5.
        let scroll = o.rows[5].y + o.rows[5].height * 0.5;
        assert!(visible_range(&o, scroll, 300.0).contains(&5));
    }

    /// Past the end, and an empty body, both give an empty range rather than a
    /// panic waiting for somebody to index with it.
    #[test]
    fn a_band_past_the_end_is_empty_not_a_panic() {
        let (_ws, o, _e) = many_rows(5);
        let far = o.rows.last().unwrap().y + 10_000.0;
        let range = visible_range(&o, far, 300.0);
        assert!(range.start <= range.end, "never inverted");
        assert!(o.rows.get(range.end).is_none() || range.end <= o.rows.len());

        let empty = Body::new(&WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap());
        assert_eq!(visible_range(&empty, 0.0, 300.0), 0..0);
    }

    #[test]
    fn a_plain_row_lays_out() {
        let mut engine = TextEngine::new();
        let (_ws, mut o, _) = outline_with("Reward payout flow");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        assert!(row.layout.len() >= 1, "at least one line");
        assert!(row.height > 0.0);
        assert!(!row.dirty);
    }

    /// The code-block design rests on this: a `\n` inside one node's text is a
    /// *mandatory break* in parley, producing two logical lines in one `Layout`
    /// (and a taller row) rather than needing a node per line. Today no text
    /// node but a code block can contain a newline, so this is the only place
    /// it is exercised.
    #[test]
    fn a_newline_breaks_one_layout_into_two_lines() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(Parent::Root, Node::code("line one\nline two").unwrap())
            .unwrap();
        let mut o = Body::new(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        assert_eq!(
            row.layout.len(),
            2,
            "a newline is a mandatory break, not a wrap"
        );

        // A two-line block is taller than a one-line one of the same words.
        let (_ws2, mut one, _) = outline_with("line one line two");
        reflow_all(&mut engine, &mut one, 900.0, 0.0);
        assert!(
            row.height > one.rows[0].height + 4.0,
            "two lines should be taller than one: {} vs {}",
            row.height,
            one.rows[0].height
        );

        // Cursor navigation crosses the explicit break: the caret at the start
        // of the second line sits on line index 1, so Up/Down move *inside* the
        // block rather than jumping to a neighbour row.
        let second_line_start = "line one\n".len();
        let line = parley::layout::Cluster::from_byte_index(&row.layout, second_line_start)
            .map(|c| c.path().line_index());
        assert_eq!(
            line,
            Some(1),
            "the caret after the newline is on the second line"
        );
    }

    #[test]
    fn marks_reach_the_layout_without_any_delimiters_in_the_text() {
        let mut engine = TextEngine::new();
        let (mut ws, mut o, id) = outline_with("Reward payout flow");
        ws.set_mark(id, 0..6, Mark::Bold, true).unwrap();
        o.rebuild(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        assert_eq!(row.text, "Reward payout flow", "no ** in the text");
        assert!(row.runs.iter().any(|r| r.bold && r.text == "Reward"));
    }

    /// The inline box must land at the anchor's byte offset, which is what
    /// makes a pill sit where it was typed.
    #[test]
    fn a_pill_becomes_an_inline_box_at_its_anchor() {
        let mut engine = TextEngine::new();
        let (mut ws, mut o, id) = outline_with("Due ");
        let attr = Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-02").unwrap()),
        );
        ws.insert_pill(id, 4, &attr).unwrap();
        o.rebuild(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        let boxes: Vec<_> = row
            .layout
            .lines()
            .flat_map(|l| l.items())
            .filter_map(|i| match i {
                PositionedLayoutItem::InlineBox(b) if b.id == attr_box_id(attr.id) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(boxes.len(), 1, "one pill, one box");
        assert_eq!(boxes[0].id, attr_box_id(attr.id));
        assert!(boxes[0].width > 0.0, "measured from its label");
    }

    /// **A search hit on a pill has to highlight the pill you can see.**
    ///
    /// The anchor is a `Default_Ignorable_Code_Point` the shaper hides, so a
    /// selection over its three bytes could plausibly measure nothing at all —
    /// and the find bar's highlight band is drawn straight from that geometry
    /// (`draw_search_range`). What gives the pill its extent is the inline box
    /// the test above pins; this is the half that says the two agree, and it is
    /// what makes "find a document by the name on the link to it" visible
    /// rather than merely true.
    #[test]
    fn a_selection_over_a_pills_anchor_measures_the_pill_not_the_hidden_character() {
        use parley::{Affinity, Cursor, Selection};

        let mut engine = TextEngine::new();
        let (mut ws, mut o, id) = outline_with("Due ");
        let attr = Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-02").unwrap()),
        );
        ws.insert_pill(id, 4, &attr).unwrap();
        o.rebuild(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        let anchor = 4..4 + koru_core::text::ANCHOR_LEN;
        let selection =
            Selection::from_byte_index(&row.layout, anchor.start, Affinity::Downstream).extend(
                Cursor::from_byte_index(&row.layout, anchor.end, Affinity::Upstream),
            );
        let mut width = 0.0f64;
        selection.geometry_with(&row.layout, |bb, _| width += bb.x1 - bb.x0);

        assert!(
            width > 1.0,
            "the highlight collapsed onto the hidden anchor ({width}px) rather than covering the pill"
        );
    }

    /// **A page removed from Tasks loses its strike**, and that is the whole
    /// of what `remove_from_tasks` leaving `NodeMeta::done` standing used to
    /// cost.
    ///
    /// The title's strike was drawn by hand from `row.done` alone, so a page
    /// completed and then taken off the Tasks list stayed struck for ever —
    /// there is no Reopen on a row that is not a task. The rule is the
    /// lifecycle now, in one place, and the hand-drawn line is gone.
    #[test]
    fn a_title_struck_while_completed_is_not_struck_once_it_is_no_longer_a_task() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let title = ws
            .insert(
                Parent::Root,
                Node::document("Improvement List Release").unwrap(),
            )
            .unwrap();
        koru_core::tasks::set_due(
            &mut ws,
            title,
            koru_core::time::Temporal::Date(
                koru_core::time::DateOnly::parse("2099-01-02").unwrap(),
            ),
        )
        .expect("a due makes the page a task");
        koru_core::tasks::complete_task(
            &mut ws,
            title,
            koru_core::tasks::LifecycleReason::ExplicitComplete,
            crate::tasks::today(),
        )
        .expect("completed");

        let mut body = Body::empty_over(&ws);
        body.root = Some(title);
        body.flat = true;
        body.title_row = true;
        body.rebuild(&ws);
        reflow_all(&mut engine, &mut body, 900.0, 0.0);
        let row = body.rows.iter().find(|r| r.is_title).expect("the title");
        assert!(row.struck(), "a completed page's title is struck");
        assert!(
            parley_strikes(row),
            "and parley is what draws it -- there is no second, hand-drawn line"
        );

        koru_core::tasks::remove_from_tasks(&mut ws, title).expect("off the list");
        body.rebuild(&ws);
        reflow_all(&mut engine, &mut body, 900.0, 0.0);
        let row = body.rows.iter().find(|r| r.is_title).expect("the title");
        assert!(
            row.done,
            "the checkbox mirror deliberately survives -- which is what made \
             a `done`-only rule wrong"
        );
        assert!(!row.struck(), "and the strike is gone with the task");
        assert!(!parley_strikes(row));
    }

    /// Whether the row's own layout carries a strikethrough decoration.
    fn parley_strikes(row: &Paragraph) -> bool {
        row.layout
            .lines()
            .flat_map(|line| line.items())
            .any(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => run.style().strikethrough.is_some(),
                _ => false,
            })
    }

    #[test]
    fn a_title_pill_inherits_the_title_font_size() {
        use koru_core::Ref;

        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let title = ws
            .insert(Parent::Root, Node::document("Planning Squad and ").unwrap())
            .unwrap();
        let target = ws
            .insert(Parent::Root, Node::document("Execution Squad").unwrap())
            .unwrap();
        ws.insert_pill(
            title,
            "Planning Squad and ".len(),
            &Attr::new("ref", AttrValue::Ref(Ref::new(target))),
        )
        .unwrap();

        // The same caption may already have been shaped in a body paragraph.
        // Font inheritance must select a distinct title-sized cache entry.
        let _ = engine.measure_pill(koru_draw::pill::PillKind::Document, "Execution Squad", None);

        let mut body = Body::empty_over(&ws);
        body.root = Some(title);
        body.flat = true;
        body.title_row = true;
        body.rebuild(&ws);
        reflow_all(&mut engine, &mut body, 900.0, 0.0);

        let row = body
            .rows
            .iter()
            .find(|row| row.is_title)
            .expect("title row");
        let info = row.pills.values().next().expect("document pill");
        let step = row.type_step();
        let style = PillTextStyle::new(step.size, step.size * step.line_height);
        let visual = engine
            .pills
            .get_with_style(info.kind, &info.label, info.url.as_deref(), style)
            .expect("title-sized visual");
        let shaped_size = visual
            .label
            .lines()
            .flat_map(|line| line.items())
            .find_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(run.run().font_size()),
                _ => None,
            })
            .expect("pill label glyphs");

        assert!((shaped_size - step.size).abs() < 0.01);
        assert!(shaped_size > theme::role::body().metrics.size);
    }

    /// A leading pill (no text before it) must put the caret on its *left* at
    /// the anchor and on its *right* just past — parley's raw geometry collapses
    /// both onto the right edge.
    #[test]
    fn a_leading_pill_puts_the_caret_on_the_correct_edge() {
        use koru_core::Ref;
        use koru_core::node::{Attr, AttrValue};
        use parley::editing::Selection;
        use parley::layout::Affinity;

        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let target = ws
            .insert(Parent::Root, Node::document("Brief").unwrap())
            .unwrap();
        let id = ws.insert(Parent::Root, Node::task("").unwrap()).unwrap();
        ws.insert_pill(id, 0, &Attr::new("ref", AttrValue::Ref(Ref::new(target))))
            .unwrap();
        let mut o = Body::new(&ws);
        o.rebuild(&ws);
        let ri = o
            .rows
            .iter()
            .position(|r| !r.pills.is_empty())
            .expect("pill");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);
        let row = &mut o.rows[ri];
        let mut pill = None;
        visit_pills(row, &engine.pills, |_, _, _, x, _, w, _| {
            pill = Some((x, w));
        });
        let (px, pw) = pill.expect("visual");

        row.selection = Selection::from_byte_index(&row.layout, 0, Affinity::Downstream);
        let before = caret_geometry(row, &engine.pills, theme::caret_w());
        assert!(
            (before.x0 as f32 - px).abs() < 1.0,
            "before-pill caret on left edge: x={} pill_left={px}",
            before.x0
        );

        row.selection = Selection::from_byte_index(&row.layout, ANCHOR_LEN, Affinity::Downstream);
        let after = caret_geometry(row, &engine.pills, theme::caret_w());
        assert!(
            (after.x0 as f32 - (px + pw)).abs() < 1.0,
            "after-pill caret on right edge: x={} pill_right={}",
            after.x0,
            px + pw
        );
        assert!(
            after.x0 > before.x0 + 10.0,
            "before and after must not collapse: {} vs {}",
            before.x0,
            after.x0
        );
    }

    /// A pill has descent below the text baseline. Reporting its full painted
    /// height to parley makes all of that height ascent and pushes the baseline
    /// (and therefore the surrounding text) down. The layout box reports only
    /// the real ascent, so adding a pill leaves the paragraph's line metrics
    /// unchanged.
    #[test]
    fn an_inline_pill_does_not_shift_the_text_baseline() {
        let mut engine = TextEngine::new();
        let (_plain_ws, mut plain, _) = outline_with("Needs to be done by");
        reflow_all(&mut engine, &mut plain, 900.0, 0.0);
        let plain_line = plain.rows[0].layout.get(0).unwrap();
        let plain_metrics = *plain_line.metrics();

        let (mut ws, mut with_pill, id) = outline_with("Needs to be done by ");
        let attr = Attr::new(
            "due",
            AttrValue::Date(koru_core::time::DateOnly::parse("2026-08-22").unwrap()),
        );
        ws.insert_pill(id, "Needs to be done by ".len(), &attr)
            .unwrap();
        with_pill.rebuild(&ws);
        reflow_all(&mut engine, &mut with_pill, 900.0, 0.0);
        let pill_line = with_pill.rows[0].layout.get(0).unwrap();
        let pill_metrics = *pill_line.metrics();

        assert!(
            (pill_metrics.baseline - plain_metrics.baseline).abs() < 0.01,
            "pill baseline {} must match plain text {}",
            pill_metrics.baseline,
            plain_metrics.baseline
        );
        assert!(
            (pill_metrics.line_height - plain_metrics.line_height).abs() < 0.01,
            "a pill must not make the paragraph line taller"
        );
    }

    #[test]
    fn emoji_first_text_keeps_the_same_line_box_and_baseline() {
        let mut engine = TextEngine::new();
        // Include a descender letter — body descent is a font metric, but the
        // historical jump showed up the moment Latin joined an emoji-only line.
        let (_emoji_ws, mut emoji_only, _) = outline_with("🤝😳✋");
        reflow_all(&mut engine, &mut emoji_only, 900.0, 0.0);
        let emoji_metrics = *emoji_only.rows[0].layout.get(0).unwrap().metrics();
        let emoji_glyph_y = first_emoji_glyph_y(&emoji_only.rows[0]);

        let (_mixed_ws, mut mixed, _) = outline_with("🤝😳✋g");
        reflow_all(&mut engine, &mut mixed, 900.0, 0.0);
        let mixed_metrics = *mixed.rows[0].layout.get(0).unwrap().metrics();
        let mixed_glyph_y = first_emoji_glyph_y(&mixed.rows[0]);

        assert!(
            (emoji_metrics.line_height - mixed_metrics.line_height).abs() < 0.01,
            "adding text cannot resize an emoji-first line"
        );
        assert!(
            (emoji_metrics.baseline - mixed_metrics.baseline).abs() < 0.01,
            "emoji baseline {} must match mixed text {}",
            emoji_metrics.baseline,
            mixed_metrics.baseline,
        );
        assert!(
            (emoji_glyph_y - mixed_glyph_y).abs() < 0.5,
            "emoji glyph.y must not jump when a letter is typed: {emoji_glyph_y} vs {mixed_glyph_y}"
        );
    }

    /// First painted glyph y on the line (layout space). Skips the transparent
    /// layout-only ZWSP strut at the end.
    fn first_emoji_glyph_y(row: &crate::paragraph::Paragraph) -> f32 {
        for item in row.layout.lines().flat_map(|line| line.items()) {
            if let PositionedLayoutItem::GlyphRun(gr) = item {
                if gr.style().brush.0.components[3] == 0.0 {
                    continue;
                }
                if let Some(g) = gr.positioned_glyphs().next() {
                    return g.y;
                }
            }
        }
        panic!("expected a painted glyph");
    }

    /// A stored tab stays one byte in the document, but lays out as a blank
    /// four-space box. Its source glyph run is transparent and zero-sized, so
    /// a font's unsupported-control glyph can never appear on screen.
    #[test]
    fn a_tab_is_blank_spacing_not_a_missing_glyph() {
        let mut engine = TextEngine::new();
        let (_ws, mut o, _) = outline_with("a\tb");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        assert_eq!(row.text, "a\tb", "the document retains a real tab");
        let mut tab_box = None;
        let mut tab_run_is_invisible = false;
        for item in row.layout.lines().flat_map(|line| line.items()) {
            match item {
                PositionedLayoutItem::InlineBox(b) if b.id == tab_box_id(1) => {
                    tab_box = Some(b);
                }
                PositionedLayoutItem::GlyphRun(gr) if gr.run().text_range() == (1..2) => {
                    tab_run_is_invisible =
                        gr.run().font_size() == 0.0 && gr.style().brush.0.components[3] == 0.0;
                }
                _ => {}
            }
        }

        let tab_box = tab_box.expect("the tab has a layout box");
        assert!(tab_box.width > 0.0, "the tab reserves horizontal space");
        assert_eq!(tab_box.height, 0.0, "a tab cannot move the baseline");
        assert!(
            tab_run_is_invisible,
            "the unsupported tab glyph is never painted"
        );
    }

    /// An empty line still gets a letter-height caret (ZWSP / primary metrics),
    /// not a collapsed bar — but not the full strut line box either.
    #[test]
    fn an_empty_paragraph_keeps_a_letter_height_caret() {
        let mut engine = TextEngine::new();
        let (_ws, mut o, _) = outline_with("");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        let cursor = parley::editing::Cursor::from_byte_index(
            &row.layout,
            0,
            parley::layout::Affinity::Downstream,
        );
        let bb = cursor.geometry(&row.layout, theme::caret_w());
        let (baseline, ascent, descent) = TextEngine::line_ink(&row.layout, bb);
        let caret = theme::caret_rect(bb, baseline, ascent, descent, 0.0, 0.0);
        let line = theme::FONT_SIZE * theme::LINE_HEIGHT;
        let h = caret.y1 - caret.y0;
        assert!(
            h >= f64::from(theme::FONT_SIZE) * 0.55,
            "empty-line caret has letter-like height: {caret:?}"
        );
        assert!(
            h <= f64::from(line) * 0.95,
            "empty-line caret stays under the strut line box: {caret:?}"
        );
    }

    /// Tall = cap (or ascent) + 2×descent; Medium/Short scale about Tall center.
    #[test]
    fn the_caret_uses_cap_plus_two_descents_scaled_about_tall_center() {
        let mut engine = TextEngine::new();
        let (_ws, mut o, _) = outline_with("asdfasdfpg");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        let cursor = parley::editing::Cursor::from_byte_index(
            &row.layout,
            row.text.len(),
            parley::layout::Affinity::Upstream,
        );
        let bb = cursor.geometry(&row.layout, theme::caret_w());
        let (baseline, rise, descent) = TextEngine::line_ink(&row.layout, bb);
        assert!(
            descent > 0.5,
            "a line with p/g reports descent: baseline={baseline} rise={rise} descent={descent}"
        );
        let scale = theme::caret_height().scale();
        let caret = theme::caret_rect(bb, baseline, rise, descent, 0.0, 0.0);
        let base = baseline as f64;
        let tall_top = base - rise as f64 - descent as f64;
        let tall_bot = base + descent as f64;
        let tall_h = tall_bot - tall_top;
        let tall_c = (tall_top + tall_bot) * 0.5;
        let h = caret.y1 - caret.y0;
        let line_ascent = row
            .layout
            .lines()
            .next()
            .map(|l| l.metrics().ascent as f64)
            .unwrap_or(0.0);

        assert!(caret.y0 < base && caret.y1 > base, "caret {caret:?}");
        assert!(
            (h - tall_h * scale).abs() < 2.0,
            "height ≈ (rise+2×descent)×scale: h={h} expected={}",
            tall_h * scale
        );
        let caret_mid = (caret.y0 + caret.y1) * 0.5;
        assert!(
            (caret_mid - tall_c).abs() < 2.0,
            "centered on Tall center: caret_mid={caret_mid} tall_c={tall_c}"
        );
        // Must not climb the strut-inflated line ascent.
        assert!(
            caret.y0 > base - line_ascent + 2.0,
            "top stays below strut line ascent: y0={} line_ascent_top={}",
            caret.y0,
            base - line_ascent
        );
    }

    #[test]
    fn an_italic_caret_is_slanted() {
        use parley::BoundingBox;
        let bb = BoundingBox {
            x0: 10.0,
            y0: 0.0,
            x1: 11.0,
            y1: 20.0,
        };
        let upright = theme::caret_quad(bb, 12.0, 10.0, 3.0, 0.0, 0.0, 0.0);
        let slanted =
            theme::caret_quad(bb, 12.0, 10.0, 3.0, 0.0, 0.0, theme::CARET_ITALIC_SLANT_DEG);
        assert_eq!(upright[0].0, upright[3].0, "upright top.x == bottom.x");
        assert!(
            (slanted[0].0 - slanted[3].0).abs() > 1.0,
            "italic top shifts: {slanted:?}"
        );
    }

    /// A tab is invisible, but the caret after it is not. When the tab is the
    /// only character in a paragraph there is no neighbouring text run whose
    /// metrics can lend the caret a height, so the tab's own layout style must
    /// retain a normal line box even though its glyph is zero-sized.
    #[test]
    fn a_tab_only_paragraph_keeps_a_visible_caret() {
        let mut engine = TextEngine::new();
        let (_ws, mut o, _) = outline_with("\t");
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let row = &o.rows[0];
        let cursor = parley::editing::Cursor::from_byte_index(
            &row.layout,
            1,
            parley::layout::Affinity::Downstream,
        );
        let caret = cursor.geometry(&row.layout, theme::caret_w());
        assert!(
            caret.y1 - caret.y0 > 1.0,
            "the caret after a lone tab has height: {caret:?}"
        );
        let tab = row
            .layout
            .lines()
            .flat_map(|line| line.items())
            .find_map(|item| match item {
                PositionedLayoutItem::InlineBox(b) if b.id == tab_box_id(0) => Some(b),
                _ => None,
            })
            .expect("the tab has a layout box");
        assert!(
            caret.x0 >= f64::from(tab.width - 0.5),
            "the caret sits after the tab spacing: {caret:?}, tab {tab:?}"
        );
    }

    #[test]
    fn several_pills_get_distinct_box_ids() {
        let a = AttrId::new();
        let b = AttrId::new();
        assert_ne!(attr_box_id(a), attr_box_id(b));
    }

    #[test]
    fn rows_stack_vertically_with_a_gap() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(Parent::Root, Node::task("one").unwrap()).unwrap();
        ws.insert(Parent::Root, Node::task("two").unwrap()).unwrap();
        let mut o = Body::new(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        assert!(o.rows[1].y > o.rows[0].y + o.rows[0].height - 1.0);
        assert!(content_height(&o) > o.rows[1].y);
    }

    #[test]
    fn extracting_a_middle_list_item_keeps_every_baseline_in_place() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        let ids = ["one", "two", "three", "four"].map(|text| {
            ws.insert(Parent::Node(doc), Node::paragraph(text).unwrap())
                .unwrap()
        });
        ws.wrap_in_list_block_many(&ids, koru_core::ListStyle::Bullet)
            .unwrap();
        let mut body = Body::empty_over(&ws);
        body.root = Some(doc);
        body.flat = true;
        body.rebuild(&ws);
        reflow_all(&mut engine, &mut body, 900.0, 0.0);
        let before: Vec<_> = body.rows.iter().map(|row| (row.y, row.height)).collect();

        ws.extract_list_item(ids[1]).unwrap();
        body.rebuild(&ws);
        reflow_all(&mut engine, &mut body, 900.0, 0.0);
        let after: Vec<_> = body.rows.iter().map(|row| (row.y, row.height)).collect();

        assert_eq!(before, after, "list styling does not change row rhythm");
        assert_eq!(body.rows[1].list_style, None);
    }

    /// Depth indents the text column, so a nested row lays out narrower.
    #[test]
    fn depth_shifts_the_text_column() {
        assert!(text_x(2u16, true) > text_x(0u16, true));
    }

    /// Three rows, the middle one settable, laid out and handed back.
    fn three_rows(heading: Option<koru_core::Heading>) -> Body {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        ws.insert(Parent::Root, Node::task("before").unwrap())
            .unwrap();
        let mid = ws
            .insert(Parent::Root, Node::task("a title").unwrap())
            .unwrap();
        ws.insert(Parent::Root, Node::task("after").unwrap())
            .unwrap();
        ws.set_heading(mid, heading).unwrap();
        let mut o = Body::new(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);
        o
    }

    /// The ramp has to reach parley rather than merely being pushed: a heading
    /// of the same words occupies a taller line box.
    #[test]
    fn a_heading_is_laid_out_taller_than_a_body_row_of_the_same_text() {
        let body = three_rows(None);
        let head = three_rows(Some(koru_core::Heading::H1));
        assert!(
            head.rows[1].height > body.rows[1].height + 8.0,
            "H1 {} vs body {}",
            head.rows[1].height,
            body.rows[1].height
        );
    }

    /// The height floor is the row's own line box, not the body's — an empty H1
    /// is a heading waiting for a word, not a 24px gap.
    #[test]
    fn an_empty_heading_is_a_full_height_line_not_a_body_one() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws.insert(Parent::Root, Node::task("").unwrap()).unwrap();
        ws.set_heading(id, Some(koru_core::Heading::H1)).unwrap();
        let mut o = Body::new(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let step = theme::type_step(Some(1));
        assert!(o.rows[0].height >= step.size * step.line_height - 0.5);
    }

    /// A heading pushes itself away from what came before — but not at the top
    /// of the document, where `MARGIN_TOP` is already the air.
    #[test]
    fn a_heading_opens_space_above_itself_but_never_at_the_top_of_the_document() {
        let o = three_rows(Some(koru_core::Heading::H2));
        assert_eq!(o.rows[1].space_above, theme::type_step(Some(2)).space_above);
        assert_eq!(o.rows[0].space_above, 0.0, "nothing above the first row");
        assert_eq!(o.rows[2].space_above, 0.0, "body text opens no space");

        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let first = ws
            .insert(Parent::Root, Node::task("a title").unwrap())
            .unwrap();
        ws.set_heading(first, Some(koru_core::Heading::H1)).unwrap();
        let mut top = Body::new(&ws);
        reflow_all(&mut engine, &mut top, 900.0, 0.0);
        assert_eq!(top.rows[0].space_above, 0.0);
        assert_eq!(top.rows[0].y, theme::MARGIN_TOP);
    }

    /// The space is its own field: `y` is still the first glyph's top and
    /// `height` is still the text's, so every layout geometry keeps working.
    #[test]
    fn the_space_above_a_heading_is_not_part_of_its_height_or_its_y() {
        let o = three_rows(Some(koru_core::Heading::H2));
        let gap = o.rows[1].y - (o.rows[0].y + o.rows[0].height + theme::TASK_GAP);
        assert!(
            (gap - o.rows[1].space_above).abs() < 0.01,
            "the whole extra gap is the recorded space: {gap} vs {}",
            o.rows[1].space_above
        );
        assert!((o.rows[1].height - o.rows[1].layout.height()).abs() < 0.01);
    }

    /// The two affordances tile the lane before the text and never overlap —
    /// which is the whole reason `AFFORDANCE_W` exists. At depth 0 the `+` sits
    /// in the page margin, so its left edge is the tightest case.
    #[test]
    fn the_plus_and_the_grip_fit_in_the_lane_without_overlapping() {
        for depth in [0u16, 1, 3] {
            let row = Paragraph::from_node(
                &Node::task("hello").unwrap(),
                depth,
                Vec::new(),
                Default::default(),
                None,
                0,
            );
            let (plus, grip) = gutter_affordances(&row, 0.0);
            assert!(plus.x >= 0.0, "the `+` stays on screen at depth {depth}");
            assert!(
                plus.x + plus.w <= grip.x,
                "the `+` ends before the grip at depth {depth}"
            );
            assert!(
                grip.x + grip.w <= row.text_x(),
                "and the grip ends before the text at depth {depth}"
            );
        }
    }

    /// Both boxes ride the row's origin, so hit-testing in document space and
    /// drawing in screen space are the same arithmetic with a different `oy`.
    #[test]
    fn the_lane_follows_the_row_it_belongs_to() {
        let (_ws, body, _id) = outline_with("hello");
        let (a, _) = gutter_affordances(&body.rows[0], 0.0);
        let (b, _) = gutter_affordances(&body.rows[0], 120.0);
        assert_eq!(b.x, a.x);
        assert!((b.y - a.y - 120.0).abs() < 0.01);
    }

    /// The title keeps its air below: the first paragraph starts
    /// `TITLE_GAP_BELOW` under the title's band, and the gap belongs to no
    /// row's `y` or `height` — a heading's `space_above` is the precedent.
    #[test]
    fn the_title_keeps_its_gap_below() {
        let mut engine = TextEngine::new();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("Spec").unwrap())
            .unwrap();
        ws.insert(Parent::Node(doc), Node::paragraph("one").unwrap())
            .unwrap();
        let mut o = Body::empty_over(&ws);
        o.root = Some(doc);
        o.flat = true;
        o.title_row = true;
        o.rebuild(&ws);
        reflow_all(&mut engine, &mut o, 900.0, 0.0);

        let title = &o.rows[0];
        let first = &o.rows[1];
        let gap = first.y - (title.y + title.height + theme::TASK_GAP);
        assert!(
            (gap - theme::TITLE_GAP_BELOW).abs() < 0.01,
            "the air below the title is the title's: {gap} vs {}",
            theme::TITLE_GAP_BELOW
        );
        // ...and no other pair of rows has it.
        assert!(o.rows.len() == 2, "the fixture is the title and one row");
    }

    /// The placeholder hint sits on the row's own baseline. The strut is what
    /// puts it there: parley gives a bare string the font's ascent and a row
    /// carrying the strut the full line box, so a strut-less hint floated a
    /// couple of pixels above the text it advertised — and the swap was
    /// visible on the first keystroke.
    #[test]
    fn the_placeholder_sits_on_the_rows_own_baseline() {
        let mut engine = TextEngine::new();
        let mut row = Paragraph::from_node(
            &Node::task("Title").unwrap(),
            0,
            Vec::new(),
            Default::default(),
            None,
            0,
        );
        build_row(&mut engine, &mut row, 900.0);
        let hint = engine.placeholder_layout(
            "Title",
            row.type_step(),
            row.font_choice().stack(),
            row.weight(),
        );

        let row_baseline = row.layout.lines().next().unwrap().metrics().baseline;
        let hint_baseline = hint.lines().next().unwrap().metrics().baseline;
        assert!(
            (row_baseline - hint_baseline).abs() < 0.01,
            "row {row_baseline} vs hint {hint_baseline}"
        );
    }

    fn selection_chip_row(height: f32) -> Paragraph {
        let mut row = Paragraph::from_node(
            &Node::paragraph("Title").unwrap(),
            0,
            Vec::new(),
            Default::default(),
            None,
            0,
        );
        row.height = height;
        row.is_title = true;
        row
    }

    /// The chip's right edge is the content column plus the named pad — not the
    /// page edge. The title action sat past this once because it measured the
    /// other number.
    #[test]
    fn the_selection_chip_ends_at_the_content_column() {
        let row = selection_chip_row(32.0);
        let layout_w = 800.0;
        let chip = row_selection_rect(&row, layout_w, 10.0);
        let expected_right =
            layout_w - theme::MARGIN_X - theme::AFFORDANCE_W + theme::ROW_SELECTED_PAD;
        assert!((chip.x + chip.w - expected_right).abs() < 0.01);
        assert!(
            (chip.x - (text_x(0u32, false) - theme::ROW_SELECTED_PAD)).abs() < 0.01,
            "pads the prose column, not a quote inset"
        );
    }

    /// The trailing action sits inside the chip, inset from its right edge.
    #[test]
    fn the_title_action_sits_inside_the_selection_chip() {
        let row = selection_chip_row(32.0);
        let chip = row_selection_rect(&row, 800.0, 10.0);
        let slot = title_action_slot(chip);
        assert!(slot.x >= chip.x);
        assert!(slot.x + slot.w <= chip.x + chip.w - TITLE_ACTION_INSET + 0.01);
        assert!((chip.x + chip.w - TITLE_ACTION_INSET - (slot.x + slot.w)).abs() < 0.01);
        assert!((slot.y + slot.h / 2.0 - (chip.y + chip.h / 2.0)).abs() < 0.01);
        assert_eq!(slot.w, slot.h);
        assert_eq!(slot.w, TITLE_ACTION_SIDE);
    }

    #[test]
    fn the_copy_chip_sits_left_of_the_language_chip() {
        let mut row = Paragraph::from_node(
            &Node::code("fn main() {}").unwrap(),
            0,
            Vec::new(),
            Default::default(),
            None,
            0,
        );
        row.lang_chip_w = 28.0;
        let layout_w = 800.0;
        let lang = lang_chip_rect(&row, layout_w);
        let copy = copy_chip_rect(&row, layout_w);
        assert_eq!(copy.y, lang.y);
        assert_eq!(copy.h, lang.h);
        assert!(copy.x + copy.w <= lang.x + 0.01);
        assert!((lang.x - copy.x - copy.w - COPY_CHIP_GAP).abs() < 0.01);
        assert!(copy.contains(copy.centre().0, copy.centre().1));
        assert!(!lang.contains(copy.centre().0, copy.centre().1));
    }

    // ------------------------------------------------- the band and the estimate
    //
    // Every other layout test in this file asserts through `reflow_all`, which
    // shapes the whole body — a path production never takes. These eight drive
    // the banded `reflow` that actually ships, and the two functions it is
    // built out of.

    /// A body of `n` paragraphs, each carrying `line`.
    fn body_of(ws: &mut WorkspaceDoc, n: usize, line: &str) -> Body {
        for _ in 0..n {
            ws.insert(Parent::Root, Node::paragraph(line).unwrap())
                .unwrap();
        }
        Body::new(ws)
    }

    /// Rows on a 100 px pitch, 50 px tall, already shaped — a body whose
    /// geometry is arithmetic rather than parley's, so a band can be asserted
    /// exactly.
    fn ruled_body(n: usize) -> Body {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let mut body = body_of(&mut ws, n, "row");
        for (i, row) in body.rows.iter_mut().enumerate() {
            row.y = i as f32 * 100.0;
            row.height = 50.0;
            row.dirty = false;
        }
        body
    }

    #[test]
    fn the_shape_band_of_an_empty_body_is_empty() {
        let ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let body = Body::new(&ws);
        assert!(body.rows.is_empty(), "the fixture has no rows to shape");
        assert_eq!(shape_band(&body, 0.0, 600.0), 0..0);
        assert_eq!(shape_band(&body, 9_000.0, 600.0), 0..0);
    }

    /// The band is exactly the rows the viewport-plus-margin covers — not one
    /// more. It used to return one row past the bottom edge, because `last`
    /// doubled as an inclusive index and as the "ran off the end" sentinel.
    #[test]
    fn the_shape_band_is_exactly_the_rows_the_margin_covers() {
        let body = ruled_body(40);
        // margin = max(viewport_h, four line boxes) = 400.
        let (scroll, viewport_h) = (1_000.0, 400.0);
        assert!(
            viewport_h > theme::FONT_SIZE * theme::LINE_HEIGHT * 4.0,
            "the viewport is the larger margin here"
        );
        // top = 600: row 6 is the first whose bottom (650) has reached it.
        // bottom = 1800: row 19 (y = 1900) is the first that starts below it.
        assert_eq!(shape_band(&body, scroll, viewport_h), 6..19);
        let band = shape_band(&body, scroll, viewport_h);
        assert!(
            body.rows[band.end - 1].y <= scroll + viewport_h + 400.0,
            "the last row in the band starts inside it"
        );
        assert!(
            body.rows[band.end].y > scroll + viewport_h + 400.0,
            "the row after the band starts below it"
        );
    }

    /// The top of the document, the bottom of it, a body shorter than the
    /// viewport, and a scroll that has run off the end. Whatever the band is,
    /// it indexes rows that exist.
    #[test]
    fn the_shape_band_is_always_within_the_rows() {
        for n in [0usize, 1, 3, 40] {
            let body = ruled_body(n);
            let content = n as f32 * 100.0;
            for &viewport_h in &[80.0_f32, 400.0, 5_000.0, f32::INFINITY] {
                for &scroll in &[0.0_f32, 50.0, content * 0.5, content, content + 10_000.0] {
                    let band = shape_band(&body, scroll, viewport_h);
                    assert!(
                        band.start <= band.end && band.end <= body.rows.len(),
                        "{n} rows, viewport {viewport_h}, scroll {scroll}: {band:?}"
                    );
                }
            }
        }
    }

    /// A body that fits on screen is shaped whole, and a scroll past its last
    /// row asks for nothing — there is nothing down there to draw.
    #[test]
    fn a_short_body_bands_whole_and_a_scroll_past_the_end_bands_nothing() {
        let body = ruled_body(3);
        assert_eq!(shape_band(&body, 0.0, 600.0), 0..3);
        // Every row's bottom is above `scroll - margin`, so none qualifies.
        assert_eq!(shape_band(&body, 5_000.0, 600.0), 3..3);
    }

    /// The estimate is a placeholder for a row's real height, so it has to be
    /// at least a line box for anything — an empty row included — and it has to
    /// grow with the text rather than with the byte count.
    #[test]
    fn the_height_estimate_is_never_shorter_than_one_line() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let empty = ws
            .insert(Parent::Root, Node::paragraph("").unwrap())
            .unwrap();
        let short = ws
            .insert(Parent::Root, Node::paragraph("hello").unwrap())
            .unwrap();
        let wrapping = ws
            .insert(
                Parent::Root,
                Node::paragraph("the quick brown fox ".repeat(20)).unwrap(),
            )
            .unwrap();
        // One token nothing can break on: the estimate counts characters, so it
        // still asks for several lines. Over-estimating is survivable — `reflow`
        // re-chooses the band once the row is shaped — under-estimating below a
        // line is not, because the row would have no band at all.
        let token = ws
            .insert(Parent::Root, Node::paragraph("a".repeat(400)).unwrap())
            .unwrap();
        let body = Body::new(&ws);
        let at = |id| body.rows.iter().find(|r| r.id == id).unwrap();
        let w = 900.0;

        let step = at(empty).type_step();
        let line_box = step.size * step.line_height;
        assert!(
            (estimated_text_height(at(empty), w) - line_box).abs() < 0.01,
            "an empty row is one line box, not nothing"
        );
        assert!(
            (estimated_text_height(at(short), w) - line_box).abs() < 0.01,
            "a row that cannot wrap is one line box"
        );
        assert!(
            estimated_text_height(at(wrapping), w) > line_box * 2.0,
            "400 characters in a 900 px column wrap to more than two lines"
        );
        assert!(
            estimated_text_height(at(token), w) >= line_box,
            "an unbreakable token still gets a row"
        );
    }

    /// A hard newline is a line whatever the column is, so the estimate counts
    /// them — a code block of thirty short lines is thirty lines tall, not one.
    #[test]
    fn the_height_estimate_counts_hard_newlines() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let id = ws
            .insert(Parent::Root, Node::code("fn a() {}\n".repeat(30)).unwrap())
            .unwrap();
        let body = Body::new(&ws);
        let row = body.rows.iter().find(|r| r.id == id).unwrap();
        let step = row.type_step();
        let line_box = step.size * step.line_height;
        // Thirty newlines: thirty-one lines by the rule `hard_lines + 1`.
        assert!(
            estimated_text_height(row, 900.0) >= line_box * 30.0,
            "each hard line is a line: {}",
            estimated_text_height(row, 900.0)
        );
        // A narrower column can only make it taller, never shorter.
        assert!(estimated_text_height(row, 300.0) >= estimated_text_height(row, 900.0));
    }

    /// **The invariant the banded reflow is only useful if it keeps.**
    ///
    /// `reflow` positions with estimates, chooses the band from those
    /// positions, shapes it, and positions again — and shaping is what changes
    /// the heights the band was chosen from. `estimated_text_height` assumes a
    /// half-em advance; narrow glyphs shape about half that tall, so the band
    /// collapses under the viewport and rows estimated below it rise into view
    /// unshaped. Before `reflow` re-chose the band this left 240 px of a 600 px
    /// viewport blank at every scroll below the first screen.
    ///
    /// The text here is chosen to be badly estimated, not to be exotic: `i` at
    /// this size is well under half an em, which is all it takes.
    #[test]
    fn an_over_estimated_body_leaves_nothing_unshaped_on_screen() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let narrow = "iiiiiiiiiii ".repeat(30);
        for _ in 0..300 {
            ws.insert(Parent::Root, Node::paragraph(narrow.as_str()).unwrap())
                .unwrap();
        }
        let mut engine = TextEngine::new();
        let (w, h) = (900.0_f32, 600.0_f32);
        for scroll in [0.0_f32, 1_000.0, 2_000.0, 5_000.0, 9_000.0] {
            let mut body = Body::new(&ws);
            reflow(&mut engine, &mut body, w, 0.0, scroll, h);
            let blank: Vec<usize> = body
                .rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.dirty && r.y + r.height > scroll && r.y < scroll + h)
                .map(|(i, _)| i)
                .collect();
            assert!(
                blank.is_empty(),
                "scroll {scroll}: rows {blank:?} are on screen and still unshaped"
            );
        }
    }

    /// ...and the fix for it must not turn into "shape the whole document".
    /// A three-hundred row body still leaves the far end as an estimate.
    #[test]
    fn reflow_still_leaves_the_far_end_of_the_body_unshaped() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let mut body = body_of(&mut ws, 300, &"the quick brown fox ".repeat(4));
        let mut engine = TextEngine::new();
        reflow(&mut engine, &mut body, 900.0, 0.0, 0.0, 600.0);
        let shaped = body.rows.iter().filter(|r| !r.dirty).count();
        assert!(
            shaped < body.rows.len() / 2,
            "the band is a band, not the body: {shaped} of {} shaped",
            body.rows.len()
        );
        // A second reflow at the same scroll shapes nothing new — the loop
        // stops on its first pass, which is what keeps an idle frame free.
        let before = shaped;
        reflow(&mut engine, &mut body, 900.0, 0.0, 0.0, 600.0);
        assert_eq!(
            before,
            body.rows.iter().filter(|r| !r.dirty).count(),
            "a settled body reflows without shaping anything"
        );
    }
}
