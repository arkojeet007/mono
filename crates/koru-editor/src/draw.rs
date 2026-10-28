// SPDX-License-Identifier: MPL-2.0

//! Drawing a body of paragraphs: the row loop, and what is inside a row.
//!
//! One layer above [`koru_draw::scene`], which knows nothing about nodes. This knows
//! about paragraphs, runs and pills, and nothing about which view is showing
//! them.
//!
//! The checkbox and the drag grip are here rather than with the outline, and
//! that is deliberate: they are gated on `Paragraph::is_task()`, not on which
//! view is drawing. A task list inside a document gets checkboxes for the same
//! reason, without this file learning what a document is. Structure a view
//! invents on top — a breadcrumb, group chips — has no such per-row gate and
//! belongs to that view.

use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, PositionedLayoutItem};
use peniko::{Color, Fill};
use std::collections::HashMap;
use std::ops::Range;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Rect};

use koru_core::AttrId;
use koru_core::Mark;
use koru_core::node::{Align, Kind};

use crate::body::Body;
use crate::components::draw::draw_overlay;
use crate::editor::Editing;
use crate::paragraph::Paragraph;
use crate::rowlayout;
use crate::view_state::{
    Hover, LinkChip, LinkChipHit, SearchHighlight, SpellingUnderline, ViewState,
};
use koru_core::NodeId;
use koru_draw::geometry::{checkbox_x, marker_right_x};
use koru_draw::pill::{PillCache, draw as draw_pill_component};
use koru_draw::scene::draw_status_bar;
use koru_draw::scene::{draw_decorations, draw_glyph_run, draw_layout, fill, rect, rrect, stroke};
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, ViewMode};
use koru_draw::{ImageCache, icons};

#[allow(clippy::too_many_arguments)]
pub fn draw_document(
    scene: &mut Scene,
    root: Affine,
    outline: &Body,
    engine: &mut TextEngine,
    images: &ImageCache,
    image_resize: Option<NodeId>,
    scroll: f32,
    caret_on: bool,
    hover: Option<Hover>,
    selected_pill: Option<(usize, AttrId)>,
    search_flash: Option<(NodeId, Range<usize>)>,
    search_highlights: &[SearchHighlight],
    spelling: &[SpellingUnderline],
    vw: f32,
    vh: f32,
    active: bool,
    insert_button: bool,
    table_source: Option<NodeId>,
    table_scroll: &HashMap<NodeId, f32>,
    table_cell: Option<(NodeId, usize, usize)>,
    table_controls: Option<(NodeId, f32)>,
    copy_icon: &koru_ui::CopyAffordance,
    tags: &crate::chips::TagLookup,
    lists: &crate::chips::ListLookup,
    reminder_chips: &HashMap<NodeId, Vec<crate::reminders::InlineChip>>,
) {
    // Hidden rows are not in `outline.rows` at all — the filtering happens when
    // rows are derived from the document, so drawing never has to know about it.
    //
    // Rows outside the window are skipped rather than encoded and thrown away
    // by the clip: a frame should cost what is on screen, not what is in the
    // document. The index is the *real* one, since a row's identity — focus,
    // hover — is its position in the body rather than its position in the loop.
    let visible = rowlayout::visible_range(outline, scroll, vh);
    for i in visible.clone() {
        let row = &outline.rows[i];
        let oy = row.y - scroll;
        let ox = row.text_x();
        // A diagram row is one thing to a person — they hover a drawing, not
        // a fill and a button — and one record to the inspector: everything
        // painted for it below, in either of its states, nests under this
        // scope, which lives to the end of the row's turn. Nothing while the
        // inspector is closed.
        let _diagram_scope = row
            .diagram
            .as_ref()
            .map(|state| diagram_block_scope(root, row, state, vw, oy));
        // An image row is a picture (or a placeholder), not text: it has no
        // caret or text-selection rect. It is still a movable block, so its
        // grip follows the same hover and selection rules as every other row.
        if row.image.is_some() {
            let selected = i == outline.focus && active;
            let on_row = hover.and_then(Hover::row) == Some(i);
            let dragging = image_resize.is_some_and(|n| n == row.id);
            draw_image_row(scene, root, row, images, engine, vw, oy, selected, dragging);
            if selected || on_row {
                let grip_active = matches!(hover, Some(Hover::Grip(h)) if h == i);
                draw_grip(scene, root, row, oy, grip_active);
                if insert_button {
                    let plus_hovered = matches!(hover, Some(Hover::Plus(h)) if h == i);
                    draw_plus(scene, root, row, oy, plus_hovered);
                }
            }
            continue;
        }
        // A divider is a hairline, not text: no caret or text-selection rect. It
        // is still a movable block, so it wears the multi-selection background
        // (and the focused background, since it has no caret of its own) and its
        // grip follows the same hover and selection rules as every other row.
        if row.kind == koru_core::node::Kind::Divider {
            let selected = i == outline.focus && active;
            let on_row = hover.and_then(Hover::row) == Some(i);
            if row.selected || selected {
                draw_row_selection(scene, root, row, vw, oy);
            }
            draw_divider(scene, root, row, vw, oy);
            if selected || on_row {
                let grip_active = matches!(hover, Some(Hover::Grip(h)) if h == i);
                draw_grip(scene, root, row, oy, grip_active);
                if insert_button {
                    let plus_hovered = matches!(hover, Some(Hover::Plus(h)) if h == i);
                    draw_plus(scene, root, row, oy, plus_hovered);
                }
            }
            continue;
        }
        if row.kind == Kind::Table
            && table_source != Some(row.id)
            && let Some(table) = &row.table
            && table.grid.is_some()
        {
            let focused = i == outline.focus && active;
            let on_row = hover.and_then(Hover::row) == Some(i);
            // A cell click focuses the table for keyboard editing; it does not
            // select the entire block. Only the shared block-selection state
            // earns the full-row background, just like prose selected with
            // Shift+Up/Down or a selection drag.
            if row.selected {
                draw_row_selection(scene, root, row, vw, oy);
            }
            crate::table::draw(
                scene,
                root,
                row,
                table,
                engine,
                oy,
                table_scroll.get(&row.id).copied().unwrap_or(0.0),
                table_controls
                    .filter(|(id, _)| *id == row.id)
                    .map_or(0.0, |(_, alpha)| alpha),
                matches!(hover, Some(Hover::TableLayout(h)) if h == i),
                matches!(hover, Some(Hover::TableMode(h)) if h == i),
                table_cell
                    .filter(|(id, _, _)| *id == row.id)
                    .map(|(_, table_row, column)| (table_row, column)),
                search_highlights,
                spelling,
            );
            if focused || on_row {
                draw_grip(
                    scene,
                    root,
                    row,
                    oy,
                    matches!(hover, Some(Hover::Grip(h)) if h == i),
                );
            }
            continue;
        }
        // A diagram showing its picture: the drawing in place of the code
        // text, or the tinted panel alone while the first layout of this text
        // is in flight. Like an image it has no caret and no text selection;
        // like a code block it keeps the panel; its two controls — *Open*
        // and *Edit text* — ride the table controls' clock. The text, its
        // chips and its reason line are the ordinary code path below, taken
        // when the text is open or the text does not draw.
        if let Some(state) = row.diagram.as_ref().filter(|s| s.shows_picture()) {
            let selected = i == outline.focus && active;
            let on_row = hover.and_then(Hover::row) == Some(i);
            if row.selected {
                draw_row_selection(scene, root, row, vw, oy);
            }
            let alpha = table_controls
                .filter(|(id, _)| *id == row.id)
                .map_or(0.0, |(_, alpha)| alpha);
            draw_diagram_block(
                scene,
                root,
                engine,
                row,
                state,
                vw,
                oy,
                DiagramBlockFurniture {
                    selected,
                    controls_alpha: alpha,
                    hovered: match hover {
                        Some(Hover::DiagramControl { row: h, control }) if h == i => Some(control),
                        _ => None,
                    },
                    copy_glyph: copy_icon.glyph(),
                    copy_active: copy_icon.confirming(),
                    grip: (selected || on_row)
                        .then_some(matches!(hover, Some(Hover::Grip(h)) if h == i)),
                    plus: (insert_button && (selected || on_row))
                        .then_some(matches!(hover, Some(Hover::Plus(h)) if h == i)),
                },
            );
            continue;
        }
        let focused = i == outline.focus;

        // A whole row in a multi-row selection. Behind the text so the words
        // stay readable.
        if row.selected {
            draw_row_selection(scene, root, row, vw, oy);
        }

        // A code block's tinted panel goes down before the selection and the
        // text, so the selection highlight and the glyphs sit on top of it —
        // drawing it later was what hid in-block selections. The border is what
        // makes it a block rather than a tint.
        if row.kind == Kind::Code {
            let r = rowlayout::code_panel_rect(row, vw, oy);
            let panel = rrect(r.x, r.y, r.w, r.h, 6.0);
            fill(scene, root, theme::p().code_bg, &panel);
            stroke(scene, root, theme::p().panel_border, 1.0, &panel);
        }
        if row.kind == Kind::Quote {
            // One semantic blockquote copied from ChatGPT may arrive as
            // several adjacent one-line Quote nodes. Draw one rule for the
            // contiguous group: its top follows the first row's ink-trimmed
            // rule, while a multi-line quotation bridges the ordinary
            // inter-row gaps through the final row's bottom edge.
            let starts_visible_group = i == visible.start
                || i.checked_sub(1)
                    .and_then(|previous| outline.rows.get(previous))
                    .map(|previous| previous.kind)
                    != Some(Kind::Quote);
            if starts_visible_group {
                let mut end = i;
                while outline
                    .rows
                    .get(end + 1)
                    .is_some_and(|next| next.kind == Kind::Quote)
                {
                    end += 1;
                }
                let mut r = rowlayout::quote_bar_rect(row, oy);
                let end_row = &outline.rows[end];
                let end_bar = rowlayout::quote_bar_rect(end_row, end_row.y - scroll);
                let bottom = end_bar.y + end_bar.h;
                r.h = (bottom - r.y).max(r.h);
                fill(
                    scene,
                    root,
                    theme::p().panel_border,
                    &rrect(r.x, r.y, r.w, r.h, r.w * 0.5),
                );
            }
        }

        // The affordance lane fades in on hover, so the left margin stays quiet
        // until the pointer is actually on the row. The title has neither: it
        // is the node the body is rooted at, so there is nothing to drag it to
        // and nothing goes above it. Its `…` is drawn below instead, and stays.
        let on_row = hover.and_then(Hover::row) == Some(i);
        if on_row && !row.is_title {
            let active = matches!(hover, Some(Hover::Grip(h)) if h == i);
            draw_grip(scene, root, row, oy, active);
            if insert_button {
                draw_plus(
                    scene,
                    root,
                    row,
                    oy,
                    matches!(hover, Some(Hover::Plus(h)) if h == i),
                );
            }
        }
        // The page-settings `…`, in the title's own lane. On hover like the
        // grip: a mark sitting permanently beside the name reads as part of it.
        if row.is_title && (on_row || hover == Some(Hover::TitleMenu)) {
            let (slot, _) = rowlayout::gutter_affordances(row, oy);
            let hovered = hover == Some(Hover::TitleMenu);
            if hovered {
                fill(
                    scene,
                    root,
                    theme::p().hover_tint,
                    &rrect(slot.x, slot.y, slot.w, slot.h, 5.0),
                );
            }
            koru_draw::scene::dots_three(
                scene,
                root,
                slot,
                if hovered {
                    theme::p().text
                } else {
                    theme::p().row_hint
                },
            );
        }

        // Selection sits behind the text — and only while a line is active.
        if let Some((node, range)) = &search_flash
            && *node == row.id
        {
            let flash =
                Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                    Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
                );
            flash.geometry_with(&row.layout, |bb, _| {
                let r = Rect::new(
                    bb.x0 + ox as f64,
                    bb.y0 + oy as f64,
                    bb.x1 + ox as f64,
                    bb.y1 + oy as f64,
                );
                scene.fill(Fill::NonZero, root, theme::p().highlights[0].1, None, &r);
            });
        }

        // Text selection is suppressed on block-selected rows — the gray chip
        // is the selection; purple range on top of it is a second mode.
        if focused && active && !row.selected {
            row.selection.geometry_with(&row.layout, |bb, _| {
                let r = Rect::new(
                    bb.x0 + ox as f64,
                    bb.y0 + oy as f64,
                    bb.x1 + ox as f64,
                    bb.y1 + oy as f64,
                );
                scene.fill(Fill::NonZero, root, theme::selection_color(), None, &r);
            });
        }

        if row.is_task() {
            let hovered = matches!(hover, Some(Hover::Checkbox(h)) if h == i);
            let reminder_attention = task_has_reminder_attention(reminder_chips.get(&row.id));
            draw_checkbox(scene, root, row, oy, hovered, reminder_attention);
        }
        let hovered_pill = match hover {
            Some(Hover::Pill { row: h, attr }) if h == i => Some(attr),
            _ => None,
        };
        let selected_pill = match selected_pill {
            Some((s, attr)) if s == i => Some(attr),
            _ => None,
        };
        draw_row_content(
            scene,
            root,
            row,
            &engine.pills,
            ox,
            oy,
            hovered_pill,
            selected_pill,
            search_highlights,
            spelling,
        );
        // A diagram whose text is showing — open for editing, or closed and
        // not drawing: the hint in an emptied block, the reason the current
        // text is not drawn, and (open) the drawing below, labelled when it
        // is the last one that worked rather than the current text's.
        if let Some(state) = row.diagram.as_ref() {
            draw_diagram_under_text(scene, root, row, state, engine, ox, oy, vw);
        }
        // The attribute chips, trailing the text (roadmap A2). Nothing when
        // the node has none set — the invariant, and the common row's cost.
        if !row.chips.is_empty() {
            let hovered_chip = match hover {
                Some(Hover::Chip { row: h, chip }) if h == i => Some(chip),
                _ => None,
            };
            crate::chips::draw_row_chips(scene, root, row, engine, tags, lists, oy, hovered_chip);
        }
        // A completed task still holding unfinished work (SPEC §5's concurrent
        // residue). `task_warning_rect` answers `None` for every other row, so
        // the common row pays one field test. Document y in, screen y out —
        // the conversion the copy icon below uses.
        if let Some(warn) = rowlayout::task_warning_rect(row, engine, tags, lists) {
            let laid = engine.icon_layout(koru_draw::icons::WARNING, rowlayout::TASK_WARNING_SIDE);
            // Centred in its slot, the way every other icon glyph here is: a
            // Phosphor glyph's box is not exactly the size it was asked for, and
            // the slot is what the pointer is tested against.
            draw_layout(
                scene,
                root,
                &laid,
                warn.x + (warn.w - laid.width()) * 0.5,
                oy + (warn.y - row.y) + (warn.h - laid.height()) * 0.5,
                Some(theme::p().notice_fg),
            );
        }
        if row.kind == Kind::Table
            && let Some(table) = row.table.as_ref()
            && (table_source == Some(row.id) || table.grid.is_none())
        {
            crate::table::draw_source_control(
                scene,
                root,
                row,
                table,
                engine,
                oy,
                matches!(hover, Some(Hover::TableMode(h)) if h == i),
                table_controls
                    .filter(|(id, _)| *id == row.id)
                    .map_or(0.0, |(_, alpha)| alpha),
            );
        }
        if row.kind == Kind::Table
            && let Some(error) = row
                .table
                .as_ref()
                .and_then(|table| table.error_layout.as_ref())
        {
            draw_layout(
                scene,
                root,
                error,
                ox,
                oy + row.layout.height() + 4.0,
                Some(theme::p().text_dim),
            );
        }
        // A list marker (bullet/number/letter/roman) draws for list items that
        // are not checkboxes; it needs the engine, so it follows `draw_row_content`,
        // which held the immutable borrow of `engine.pills`. Lives in the gutter.
        if !row.is_task() && row.list_style.is_some() {
            draw_list_marker(scene, root, row, engine, oy);
        }

        // The language chip fades in on hover at the block's corner, on top of
        // the text. `lang_chip_rect` is in document y (for `hover_at`); here we
        // draw in screen y.
        if row.kind == Kind::Code && on_row {
            let copy = rowlayout::copy_chip_rect(row, vw);
            let (cx, cy) = (copy.x, oy + (copy.y - row.y));
            copy_icon.draw_icon(
                scene,
                root,
                engine,
                koru_draw::geometry::Rect::new(cx, cy, copy.w, copy.h),
                matches!(hover, Some(Hover::Copy(h)) if h == i),
            );
            // *Done*, left of the copy icon, while a diagram's text is open:
            // the same chip as the copy icon, wearing the check.
            if row.diagram.as_ref().is_some_and(|state| state.open) {
                let done = rowlayout::diagram_done_rect(row, vw);
                let slot =
                    koru_draw::geometry::Rect::new(done.x, oy + (done.y - row.y), done.w, done.h);
                koru_ui::icon_button::draw_with(
                    scene,
                    root,
                    engine,
                    slot,
                    icons::CHECK,
                    koru_ui::icon_button::State {
                        hovered: matches!(hover, Some(Hover::DiagramDone(h)) if h == i),
                        active: false,
                        disabled: false,
                    },
                    DONE_CHIP,
                );
            }
            let chip = rowlayout::lang_chip_rect(row, vw);
            let (cx, cy, cw, ch) = (chip.x, oy + (chip.y - row.y), chip.w, chip.h);
            let label = crate::diagram::chip_label(row.language.as_deref());
            fill(scene, root, theme::p().chip_bg, &rrect(cx, cy, cw, ch, 3.0));
            let laid = engine.simple_layout(label, theme::role::hint());
            draw_layout(
                scene,
                root,
                &laid,
                cx + 3.0,
                cy + 1.0,
                Some(theme::p().text_dim),
            );
        }

        // A group's level name is whatever the user called it, so it is drawn

        // **No caret while a pill on this row is selected.** The ring says where
        // the keyboard is; a caret blinking beside it says the keyboard is
        // somewhere else as well, and Enter would appear to be ambiguous between
        // the two. Same for a block-selected row: the chip is the selection.
        if focused
            && active
            && caret_on
            && selected_pill.is_none()
            && !row.selected
            && row.selection.is_collapsed()
        {
            let bb = rowlayout::caret_geometry(row, &engine.pills, theme::caret_w());
            let (baseline, ascent, descent) =
                koru_draw::text::TextEngine::line_ink(&row.layout, bb);
            let slant = if italic_at_caret(row) {
                theme::CARET_ITALIC_SLANT_DEG
            } else {
                0.0
            };
            let quad = theme::caret_quad(bb, baseline, ascent, descent, ox, oy, slant);
            koru_draw::scene::fill_caret_quad(scene, root, theme::caret_color().color(), quad);
        }
    }

    // Reminder metadata is a final row-chrome pass so it remains visible on
    // every node shape. `reserve_reminder_space` has already extended a row
    // when this exact geometry wrapped below the authored text.
    for i in visible {
        let row = &outline.rows[i];
        let Some(reminders) = reminder_chips.get(&row.id) else {
            continue;
        };
        let rects =
            reminder_indicator_rects(row, engine, tags, lists, reminders, vw, row.y - scroll);
        for (reminder_index, (reminder, rect)) in reminders.iter().zip(rects).enumerate() {
            let hovered = matches!(
                hover,
                Some(Hover::Reminder { row: h, reminder: r })
                    if h == i && r == reminder_index
            );
            let shape = rrect(rect.x, rect.y, rect.w, rect.h, rect.h * 0.5);
            fill(
                scene,
                root,
                if hovered {
                    theme::p().hover_tint
                } else {
                    theme::p().chip_bg
                },
                &shape,
            );
            stroke(
                scene,
                root,
                theme::p().panel_border,
                theme::BORDER_W,
                &shape,
            );
            let icon = engine.icon_layout(reminder_icon(reminder), 12.0);
            draw_layout(
                scene,
                root,
                &icon,
                rect.x + 7.0,
                rect.y + (rect.h - icon.height()) * 0.5,
                Some(if reminder.paused {
                    theme::p().text_dim
                } else {
                    theme::p().accent
                }),
            );
            let layout = engine.simple_layout(&reminder.label, theme::role::meta());
            draw_layout(
                scene,
                root,
                &layout,
                rect.x + 24.0,
                rect.y + (rect.h - layout.height()) * 0.5,
                Some(theme::p().text_dim),
            );
            if reminder.handled {
                let label_x = rect.x + 24.0;
                let label_y = rect.y + (rect.h - layout.height()) * 0.5;
                let strike_y = (label_y + layout.height() * 0.52) as f64;
                stroke(
                    scene,
                    root,
                    theme::p().text_dim,
                    1.0,
                    &Line::new(
                        (label_x as f64, strike_y),
                        ((label_x + layout.width()) as f64, strike_y),
                    ),
                );
            }
        }
    }
}

/// One authority for reminder-pill painting, hover and presses. Pills begin
/// after the last line's authored text and any attribute chips, then continue
/// in the same inline flow instead of occupying a viewport-right lane.
pub(crate) fn reminder_indicator_rects(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &crate::chips::TagLookup,
    lists: &crate::chips::ListLookup,
    reminders: &[crate::reminders::InlineChip],
    layout_w: f32,
    y: f32,
) -> Vec<koru_draw::geometry::Rect> {
    const GAP: f32 = koru_draw::chip::CHIP_GAP;
    const H: f32 = 24.0;
    if reminders.is_empty() {
        return Vec::new();
    }
    let Some(anchor) = reminder_flow_anchor(row, engine, tags, lists, layout_w, y) else {
        return Vec::new();
    };
    let widths = reminders
        .iter()
        .map(|reminder| {
            let label = engine.simple_layout(&reminder.label, theme::role::meta());
            label.width() + 31.0
        })
        .collect::<Vec<_>>();
    let left = row.text_x();
    let right = rowlayout::text_right(row, layout_w);
    let mut out = Vec::with_capacity(widths.len());
    let mut next = 0usize;

    // Start- and centre-aligned prose use any room left on the authored
    // line. End-aligned prose already ends at the content boundary, so its
    // reminder line begins below and is aligned against the same right edge.
    if row.align != Align::End {
        let mut x = anchor.x.max(left);
        while let Some(width) = widths.get(next).copied() {
            if x + width > right && x > left {
                break;
            }
            out.push(koru_draw::geometry::Rect::new(x, anchor.y, width, H));
            x += width + GAP;
            next += 1;
        }
    }

    let line_bottom = row
        .layout
        .lines()
        .last()
        .map_or(anchor.y + H, |line| y + line.metrics().block_max_coord);
    let mut line_y = line_bottom.max(anchor.y + H) + GAP;
    while next < widths.len() {
        let start = next;
        let mut used = 0.0_f32;
        while let Some(width) = widths.get(next).copied() {
            let candidate = if next == start {
                width
            } else {
                used + GAP + width
            };
            if next > start && candidate > right - left {
                break;
            }
            used = candidate;
            next += 1;
        }
        let mut x = match row.align {
            Align::Start => left,
            Align::Center => left + ((right - left - used) * 0.5).max(0.0),
            Align::End => (right - used).max(left),
        };
        for width in &widths[start..next] {
            out.push(koru_draw::geometry::Rect::new(x, line_y, *width, H));
            x += *width + GAP;
        }
        line_y += H + GAP;
    }
    out
}

pub(crate) fn reminder_icon(reminder: &crate::reminders::InlineChip) -> char {
    if reminder.paused {
        icons::PAUSE
    } else if reminder.snoozed {
        icons::BELL_Z
    } else {
        icons::BELL
    }
}

/// Reserve the vertical space used by wrapped reminder pills and shift every
/// later row by that exact amount. `rowlayout::reflow` remains the authority
/// for authored content; this is its host-metadata continuation and uses the
/// same geometry that drawing and input consume.
pub(crate) fn reserve_reminder_space(
    body: &mut Body,
    engine: &mut TextEngine,
    tags: &crate::chips::TagLookup,
    lists: &crate::chips::ListLookup,
    reminder_chips: &HashMap<NodeId, Vec<crate::reminders::InlineChip>>,
    layout_w: f32,
) {
    let mut shift = 0.0_f32;
    for row in &mut body.rows {
        row.y += shift;
        let Some(reminders) = reminder_chips.get(&row.id) else {
            continue;
        };
        let rects = reminder_indicator_rects(row, engine, tags, lists, reminders, layout_w, row.y);
        let bottom = rects
            .iter()
            .map(|rect| rect.y + rect.h)
            .fold(row.y + row.height, f32::max);
        let extra = (bottom - (row.y + row.height)).max(0.0);
        row.height += extra;
        shift += extra;
    }
}

/// The start of the reminder-pill flow, also used as the anchor when creation
/// or a global handoff has no existing reminder pill to name.
pub(crate) fn reminder_flow_anchor(
    row: &Paragraph,
    engine: &mut TextEngine,
    tags: &crate::chips::TagLookup,
    lists: &crate::chips::ListLookup,
    layout_w: f32,
    y: f32,
) -> Option<koru_draw::geometry::Rect> {
    const GAP: f32 = koru_draw::chip::CHIP_GAP;
    const H: f32 = 24.0;
    if row.height < H {
        return None;
    }
    let (line_top, line_h, offset, advance) =
        row.layout
            .lines()
            .last()
            .map_or((0.0, H, 0.0, 0.0), |line| {
                let metrics = line.metrics();
                (
                    metrics.block_min_coord,
                    metrics.block_max_coord - metrics.block_min_coord,
                    metrics.offset,
                    metrics.advance,
                )
            });
    let x = crate::chips::chip_boxes(row, engine, tags, lists)
        .last()
        .map_or(row.text_x() + offset + advance + GAP, |chip| {
            chip.rect.x + chip.rect.w + GAP
        })
        .min(rowlayout::text_right(row, layout_w));
    let y = y + line_top + (line_h - H) * 0.5;
    Some(koru_draw::geometry::Rect::new(x, y, 1.0, H))
}

/// Whether the caret sits in italic (run mark or armed pending italic).
fn italic_at_caret(row: &Paragraph) -> bool {
    if let (Some(marks), _) = row.armed() {
        if marks.has(Mark::Italic) {
            return true;
        }
    }
    let at = row.caret();
    for run in &row.runs {
        if !run.italic {
            continue;
        }
        let r = run.range();
        // Prefer the run the next keystroke joins (left of caret when mid-text).
        if at > r.start && at <= r.end {
            return true;
        }
        if at == r.start && at < row.text.len() {
            return true;
        }
    }
    false
}

/// **The missing-image placeholder's second line: the reason, not a guess**
/// (D259).
///
/// This used to be one sentence with one variation — *"waiting for a device
/// with this image to come online"*, rung 1's peer answer — whatever the
/// actual reason was. On hardware the actual reason was
/// `mode_refuses_transport`: the bytes were on Koru Cloud and **this device**
/// had never been switched on for that Workspace. So the placeholder sent
/// somebody to restart a machine they did not need, for a state that is one
/// switch away.
///
/// `hold` is the blob engine's own stable code, carried through the Store's
/// failure beside its `Code` and recorded on the image cache. Each one gets
/// the sentence it actually means, and the transport-mode case says plainly
/// that *this device* is not using Koru Cloud here — because that is the
/// half a person can act on, and naming another device instead is the bug.
///
/// A code this build has not heard of is shown as itself rather than
/// translated into a guess: it is a stable identifier a support conversation
/// can search for.
///
/// **No layout of its own.** One string, handed to the single wrapped layout
/// the placeholder already builds — no second pass, and nothing here reaches
/// `Editing::reflow`.
pub fn missing_image_detail(
    lang: koru_i18n::Lang,
    hold: Option<&str>,
    waiting_on: Option<&[String]>,
) -> String {
    use koru_i18n::Key;
    if let Some(hold) = hold {
        let key = match hold {
            // The contract's §7 spelling. **The one that must not mention
            // another device**: the bytes are reachable, this device is not
            // asking for them, and the remedy is a switch in Manage Profiles.
            "mode_refuses_transport" => Some(Key::ImageHeldByMode),
            "network_metered" => Some(Key::ImageHeldByMetered),
            "network_low_data" => Some(Key::ImageHeldByLowData),
            "network_offline" => Some(Key::ImageHeldByOffline),
            "paused_by_person" => Some(Key::ImageHeldByYou),
            "cloud_access_unavailable" => Some(Key::ImageHeldByCloudOff),
            "object_too_large" => Some(Key::ImageHeldTooLarge),
            _ => None,
        };
        return match key {
            Some(key) => koru_i18n::t(lang, key).to_owned(),
            None => koru_i18n::fill(koru_i18n::t(lang, Key::ImageHeldUnknown), &[hold]),
        };
    }
    match waiting_on {
        Some(names) if !names.is_empty() => koru_i18n::fill(
            koru_i18n::t(lang, Key::ImageWaitForNamedDevices),
            &[&names.join(" or ")],
        ),
        _ => koru_i18n::t(lang, Key::ImageWaitForAnyDevice).to_owned(),
    }
}

/// Paint an image row: the decoded picture scaled to its display size, or a
/// muted placeholder when its bytes have not been decoded here yet — a remote
/// image whose bytes are still arriving, or one that failed to decode.
///
/// When `selected` the row wears a 2px border with a small box at each corner;
/// while `dragging` a resize, a `W×H` label sits centred over it.
pub fn draw_image_row(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    images: &ImageCache,
    engine: &mut TextEngine,
    vw: f32,
    oy: f32,
    selected: bool,
    dragging: bool,
) {
    let img = row.image.as_ref().expect("an image row has an image");
    let (x, dw, dh) = rowlayout::image_display_box(img, row.align, vw);
    let _inspection = koru_draw::inspect::scope(
        "Document image",
        "unregistered",
        root,
        rect(x, oy, dw, dh),
        |record| {
            record.property(
                "implementation",
                "koru_editor::draw::draw_image_row",
                "document image renderer; not an approved Koru component",
            );
            record.property(
                "display size",
                format!("{dw:.1} × {dh:.1}"),
                "image_display_box; logical units",
            );
            if let Some(width) = img.width {
                record.property("source width", width, "stored image metadata; pixels");
            }
            if let Some(height) = img.height {
                record.property("source height", height, "stored image metadata; pixels");
            }
            record.property(
                "alignment",
                format!("{:?}", row.align),
                "document image alignment",
            );
        },
    );
    // The larger axis this draw box needs, in device pixels. vello has no
    // mipmaps, so a full-resolution picture drawn into a small box is sampled
    // pointwise every frame; `scaled` builds the mip on the CPU instead, and
    // caches it by size.
    let [a, b, c, d, _, _] = root.as_coeffs();
    let scale = a.hypot(b).max(c.hypot(d));
    let wanted = ((dw.max(dh)) as f64 * scale).ceil().max(1.0) as u32;

    let Some(brush) = images.scaled(&img.hash.0, wanted) else {
        koru_draw::inspect::current(|record| {
            record.property(
                "render state",
                "unavailable / placeholder",
                "decoded image cache",
            )
        });
        fill(scene, root, theme::p().chip_bg, &rrect(x, oy, dw, dh, 6.0));
        // A blank rectangle reads as a rendering error. This is a remote blob
        // awaiting hydration, so make both the missing image and the recovery
        // path legible instead.
        if dw >= 150.0 && dh >= 72.0 {
            let p = theme::p();
            let image = engine.icon_layout(icons::IMAGE, 28.0);
            let cross = engine.icon_layout(icons::WINDOW_CLOSE, 12.0);
            // **The reader's language**, through the process-global the
            // localization crate keeps for exactly this: a leaf surface draws
            // words too and cannot see an app's private static. Both strings
            // here were bare English until D259.
            let lang = koru_i18n::current();
            let message = engine.simple_layout(
                koru_i18n::t(lang, koru_i18n::Key::ImagePlaceholderUnavailable),
                theme::role::row(),
            );
            // The detail names the reason, or the devices it waits for, so it
            // can outgrow the box on one line: wrap it to the box's inner
            // width, and drop it before the wrapped block would overflow the
            // box vertically. **One layout**, as before — reading the hold
            // changes which string is measured, never how many are.
            const INSET: f32 = 12.0;
            let detail_text =
                missing_image_detail(lang, images.hold(&img.hash.0), images.pending(&img.hash.0));
            let detail = engine.simple_layout_wrapped(
                &detail_text,
                theme::role::meta(),
                (dw - INSET * 2.0).max(1.0),
            );
            let icon_block = image.height() + 6.0 + message.height();
            let full_block = icon_block + detail.height();
            let (block_h, detail) = if full_block <= dh - 16.0 {
                (full_block, Some(detail))
            } else {
                (icon_block, None)
            };
            let top = oy + (dh - block_h) * 0.5;
            let icon_x = x + (dw - image.width()) * 0.5;
            draw_layout(scene, root, &image, icon_x, top, Some(p.text_dim));
            draw_layout(
                scene,
                root,
                &cross,
                icon_x + image.width() - cross.width() * 0.7,
                top + image.height() - cross.height() * 0.7,
                Some(p.focus_ring),
            );
            let message_y = top + image.height() + 4.0;
            draw_layout(
                scene,
                root,
                &message,
                x + (dw - message.width()) * 0.5,
                message_y,
                Some(p.text),
            );
            if let Some(detail) = detail {
                draw_layout(
                    scene,
                    root,
                    &detail,
                    x + (dw - detail.width()) * 0.5,
                    message_y + message.height(),
                    Some(p.text_dim),
                );
            }
        }
        if selected {
            stroke(
                scene,
                root,
                theme::p().focus_ring,
                theme::BORDER_W,
                &rrect(x, oy, dw, dh, 6.0),
            );
        }
        return;
    };
    let iw = brush.image.width.max(1) as f64;
    let ih = brush.image.height.max(1) as f64;
    koru_draw::inspect::current(|record| {
        record.property("render state", "decoded image", "decoded image cache")
    });
    let transform = root
        * Affine::translate((x as f64, oy as f64))
        * Affine::scale_non_uniform(dw as f64 / iw, dh as f64 / ih);
    koru_draw::scene::draw_image(scene, transform, brush.as_ref());

    if selected {
        image_chrome(scene, root, engine, x, oy, dw, dh, dragging);
    }
}

/// Paint the drawing of `prepared` where `layout` placed it, under the root
/// transform, the block's translation and the column scale — with the scale
/// handed to the painter on its own, so every stroke is divided by it and a
/// drawing scaled down to fit keeps its line weight. Roles become the current
/// palette's colors here, at draw time, so a theme flip is a redraw.
fn paint_diagram(
    scene: &mut Scene,
    root: Affine,
    prepared: &koru_draw::diagram::PreparedDiagram,
    drawing: rowlayout::DrawingBox,
    oy: f32,
) {
    // The same stall guard the host puts around `editor.reflow`: a slow paint
    // shows up in `KORU_JANK` like a slow reflow would, and costs a load of a
    // flag when the switch is off.
    let _watch = koru_core::timing::Watch::start("diagram.paint");
    let transform = root
        * Affine::translate((drawing.x as f64, (oy + drawing.y) as f64))
        * Affine::scale(drawing.scale as f64);
    koru_draw::diagram::paint(prepared, theme::p(), transform, drawing.scale as f64, scene);
}

/// The inspector's one record for a diagram row: *Diagram block*, spanning
/// the code panel whatever the block shows — the picture, or the text with
/// its reason and the drawing under it — so the panel, the glyphs, the
/// drawing, the controls and the grip all nest under it. `draw_document`
/// opens it at the top of the row's turn and drops it at the end; drawn
/// in parts before, the panel and the text landed beside the record as
/// loose primitives in the tree. Records nothing while the inspector is
/// closed: the closure is never evaluated then.
fn diagram_block_scope(
    root: Affine,
    row: &Paragraph,
    state: &crate::diagram::DiagramRowState,
    vw: f32,
    oy: f32,
) -> koru_draw::inspect::Scope {
    let r = rowlayout::code_panel_rect(row, vw, oy);
    koru_draw::inspect::scope(
        "Diagram block",
        "unregistered",
        root,
        rect(r.x, r.y, r.w, r.h),
        |record| {
            diagram_inspection(record, "koru_editor::draw::diagram_block_scope", state);
        },
    )
}

/// What the F12 inspector sees on a diagram row, recorded inside the block's
/// scope and nowhere else — so it costs nothing while the inspector is closed,
/// because `inspect::scope` never evaluates its closure then.
///
/// Which of the three states the block is in; the status behind a failure,
/// which is **the one place the engine's English `message` and `code` are
/// shown** — an inspector reads them, a page never does; whether the drawing
/// is the current text's or the last one that worked; whether the text is
/// open; and the engine's process-wide counters, so a slow or refused layout
/// can be seen from the row it happened on.
fn diagram_inspection(
    record: &mut koru_draw::inspect::Record,
    implementation: &'static str,
    state: &crate::diagram::DiagramRowState,
) {
    record.property(
        "implementation",
        implementation,
        "diagram block renderer; not an approved Koru component",
    );
    let phase = if state.pending {
        "pending"
    } else if state.reason.is_some() || state.empty {
        "failed"
    } else if state.drawing.is_some() {
        "drawn"
    } else {
        "failed"
    };
    record.property(
        "state",
        phase,
        "koru_editor::diagram::DiagramRowState, mirrored from the view's entry",
    );
    if let Some(status) = &state.reason {
        record.property(
            "status",
            diagram_status_text(status),
            "koru_diagram::Status; the engine's English message and code, shown here only",
        );
    } else if state.empty {
        record.property(
            "status",
            "empty",
            "koru_diagram::Status; the text is empty or whitespace",
        );
    }
    record.property(
        "text",
        if state.open { "open" } else { "closed" },
        "ViewState::diagram_text_open; a reading position, per view",
    );
    record.property(
        "drawing",
        match (&state.drawing, state.last_good) {
            (None, _) => "none",
            (Some(_), true) => "last good",
            (Some(_), false) => "current text",
        },
        "DiagramRowState::drawing; the last good drawing shows only under the open text",
    );
    let counters = koru_diagram::Counters::snapshot();
    record.property(
        "inspections",
        format!(
            "{} (draws {}, not parsed {}, kind not drawn {}, too large {}, empty {})",
            counters.inspections,
            counters.draws,
            counters.not_parsed,
            counters.kind_not_drawn,
            counters.too_large,
            counters.empty
        ),
        "koru_diagram::Counters::snapshot(); process-wide since launch",
    );
    record.property(
        "layouts",
        format!(
            "{} (cancelled {}, timed out {}, failed {})",
            counters.layouts,
            counters.layout_cancelled,
            counters.layout_timed_out,
            counters.layout_failed
        ),
        "koru_diagram::Counters::snapshot(); process-wide since launch",
    );
    record.property(
        "layout ms",
        format!(
            "last {}, max {}",
            counters.layout_ms_last, counters.layout_ms_max
        ),
        "koru_diagram::Counters::snapshot(); wall time of a layout, whatever it answered",
    );
}

/// A status as the inspector shows it: the engine's English message and
/// code for a text it could not read, the kind or the count otherwise.
fn diagram_status_text(status: &koru_diagram::Status) -> String {
    use koru_diagram::Status;
    match status {
        Status::Draws(_) => "draws".to_owned(),
        Status::NotParsed {
            line,
            message,
            code,
        } => format!("not parsed: line {line}: {message} ({code})"),
        Status::KindNotDrawn { kind: Some(kind) } => format!("kind not drawn: {kind}"),
        Status::KindNotDrawn { kind: None } => "kind not drawn: no diagram keyword".to_owned(),
        Status::TooLarge { kind, count } => format!("too large: {kind:?}, {count}"),
        Status::Empty => "empty".to_owned(),
    }
}

/// A closed diagram's picture: the drawing scaled to the column, or nothing
/// over the tinted panel while the layout is pending. `selected` wears the
/// image's focus ring — a drawing is picked like a picture, so it reads like
/// one — with no corner handles, since a diagram is never resized by hand.
/// What a drawn diagram's block wears besides its drawing this frame.
struct DiagramBlockFurniture {
    /// The block is the focused row of an active view: the panel's border
    /// in the focus colour.
    selected: bool,
    /// The controls clock's opacity for this block; nothing is drawn at 0.
    controls_alpha: f32,
    /// The control under the pointer, if any.
    hovered: Option<crate::diagram::Control>,
    /// What the *Copy image* control shows: the shared copy affordance's
    /// glyph, so a press turns it to the check the way a code block's does.
    copy_glyph: char,
    /// …and whether that glyph is the check right now: the control is lit
    /// while a copy is confirmed, as the viewer's button is.
    copy_active: bool,
    /// `Some(hovered)` to draw the drag grip, `None` to leave it out.
    grip: Option<bool>,
    /// The insert `+` beside the grip, when the view offers one: `Some`
    /// with its hover. A drawn block is a block like any other, so a row can
    /// be added above or below it from the margin; it had the grip alone.
    plus: Option<bool>,
}

/// A diagram showing its picture, whole: the code panel, the drawing (or
/// the empty panel while the first layout is in flight), the focus ring,
/// the controls and the grip. Painted inside the row's *Diagram block*
/// record (`diagram_block_scope`), which the caller holds open.
#[allow(clippy::too_many_arguments)]
/// The controls' bar corner: a popover's.
const DIAGRAM_BAR_RADIUS: f32 = 6.0;

/// The *Done* chip's metrics: the copy icon's size in the chip band, with
/// the chip's own corner.
const DONE_CHIP: koru_ui::icon_button::Metrics = koru_ui::icon_button::Metrics {
    icon_size: 12.0,
    padding_x: 0.0,
    padding_y: 0.0,
    inset_top: 0.0,
    inset_bottom: 0.0,
    radius: 3.0,
};

#[allow(clippy::too_many_arguments)]
fn draw_diagram_block(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    row: &Paragraph,
    state: &crate::diagram::DiagramRowState,
    vw: f32,
    oy: f32,
    furniture: DiagramBlockFurniture,
) {
    let layout = rowlayout::diagram_layout(row, state, row.height, vw);
    let r = rowlayout::code_panel_rect(row, vw, oy);
    let panel = rrect(r.x, r.y, r.w, r.h, 6.0);
    fill(scene, root, theme::p().code_bg, &panel);
    // Selected, the panel's own border takes the focus colour. It wore a
    // second box around the drawing inside the panel, which read as a
    // selection within the selection.
    let border = if furniture.selected {
        theme::p().focus_ring
    } else {
        theme::p().panel_border
    };
    stroke(scene, root, border, 1.0, &panel);
    if let (Some(prepared), Some(drawing)) = (&state.drawing, layout.drawing) {
        paint_diagram(scene, root, prepared, drawing, oy);
    }
    if furniture.controls_alpha > 0.0 {
        // The controls' rects are in document y; the screen row is at `oy`.
        let on_screen = |control: koru_draw::geometry::Rect| {
            koru_draw::geometry::Rect::new(
                control.x,
                oy + (control.y - row.y),
                control.w,
                control.h,
            )
        };
        // The bar and its buttons fade as one group, the way a tooltip
        // does: an alpha on each paint would show the panel's shadow through
        // it and let a glyph arrive ahead of the bar it sits on.
        let bar = on_screen(rowlayout::diagram_controls_bar(row, vw));
        let panel = koru_draw::scene::Panel::popover(DIAGRAM_BAR_RADIUS);
        let fading = furniture.controls_alpha < 1.0;
        if fading {
            let b = koru_draw::scene::panel_bounds(bar.x, bar.y, bar.w, bar.h, panel.elevation);
            koru_draw::scene::fade_layer(
                scene,
                root,
                furniture.controls_alpha,
                &rect(b.x, b.y, b.w, b.h),
            );
        }
        {
            let _bar = koru_draw::scene::floating_panel_scope(
                scene, root, bar.x, bar.y, bar.w, bar.h, &panel,
            );
            for control in crate::diagram::Control::ALL {
                let glyph = match control {
                    crate::diagram::Control::Open => icons::MAGNIFYING_GLASS_PLUS,
                    crate::diagram::Control::Copy => furniture.copy_glyph,
                    crate::diagram::Control::Save => icons::DOWNLOAD,
                    crate::diagram::Control::Edit => icons::PENCIL,
                };
                koru_ui::icon_button::draw_with(
                    scene,
                    root,
                    engine,
                    on_screen(rowlayout::diagram_control_rect(row, vw, control)),
                    glyph,
                    koru_ui::icon_button::State {
                        hovered: furniture.hovered == Some(control),
                        active: control == crate::diagram::Control::Copy && furniture.copy_active,
                        disabled: false,
                    },
                    koru_ui::icon_button::Metrics::COMPACT,
                );
            }
        }
        if fading {
            scene.pop_layer();
        }
    }
    if let Some(grip_active) = furniture.grip {
        draw_grip(scene, root, row, oy, grip_active);
    }
    if let Some(plus_hovered) = furniture.plus {
        draw_plus(scene, root, row, oy, plus_hovered);
    }
}

/// What a diagram shows under — or in place of — its visible text, painted
/// inside the row's *Diagram block* record (`diagram_block_scope`), which
/// the caller holds open.
///
/// The hint stands in for the text of an emptied block, set the way the
/// body's own placeholder is set (the row's family, step and weight) so it
/// reads as the line the first keystroke will fill. The reason and the label
/// are one hint-role line each, in `text_dim`, the way the table's error line
/// is drawn; both are short, so shaping them here costs what the table's
/// control glyph already costs per frame. The drawing itself shapes nothing:
/// that was done when the entry was built, off the loop thread.
#[allow(clippy::too_many_arguments)]
fn draw_diagram_under_text(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    state: &crate::diagram::DiagramRowState,
    engine: &mut TextEngine,
    ox: f32,
    oy: f32,
    vw: f32,
) {
    let lang = koru_i18n::current();
    let layout = rowlayout::diagram_layout(row, state, row.layout.height(), vw);
    if state.empty && row.text.is_empty() {
        let hint = engine.placeholder_layout(
            koru_i18n::t(lang, koru_i18n::Key::DiagramEmptyHint),
            row.type_step(),
            row.font_choice().stack(),
            row.weight(),
        );
        draw_layout(scene, root, &hint, ox, oy, Some(theme::p().placeholder));
    }
    if let (Some(status), Some(y)) = (&state.reason, layout.reason_y)
        && let Some(text) = crate::diagram::reason_text(lang, status)
    {
        let line = engine.simple_layout(&text, theme::role::hint());
        draw_layout(scene, root, &line, ox, oy + y, Some(theme::p().text_dim));
    }
    if let Some(y) = layout.label_y {
        let line = engine.simple_layout(
            koru_i18n::t(lang, koru_i18n::Key::DiagramLastGood),
            theme::role::hint(),
        );
        draw_layout(scene, root, &line, ox, oy + y, Some(theme::p().text_dim));
    }
    if let (Some(prepared), Some(drawing)) = (&state.drawing, layout.drawing) {
        paint_diagram(scene, root, prepared, drawing, oy);
    }
}

/// Paint a divider: a 1px hairline spanning the content column, centred in the
/// row's band. The row height itself is one line ([`rowlayout::reflow`]), so the
/// rule keeps its air above and below rather than sitting against a neighbour.
///
/// The column matches prose and images — `MARGIN_X + AFFORDANCE_W` on each side —
/// so the hairline does not run under the `+` and the drag grip.
fn draw_divider(scene: &mut Scene, root: Affine, row: &Paragraph, vw: f32, oy: f32) {
    let left = (theme::MARGIN_X + theme::AFFORDANCE_W) as f64;
    let right = (vw - theme::MARGIN_X - theme::AFFORDANCE_W) as f64;
    let mid = oy as f64 + (row.height as f64) * 0.5;
    let line = Rect::new(left, mid - 0.5, right.max(left), mid + 0.5);
    fill(scene, root, theme::p().panel_border, &line);
}

/// The multi-selection background for one row, drawn behind its content.
///
/// Geometry is [`rowlayout::row_selection_rect`] so the title's trailing action
/// can sit inside the same chip.
fn draw_row_selection(scene: &mut Scene, root: Affine, row: &Paragraph, vw: f32, oy: f32) {
    let r = rowlayout::row_selection_rect(row, vw, oy);
    fill(
        scene,
        root,
        theme::p().row_selected,
        &rrect(r.x, r.y, r.w, r.h, 8.0),
    );
}

/// The selection furniture for an image: a 2px border, a box at each corner,
/// and while a resize is live a centred `W×H` label.
fn image_chrome(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    dragging: bool,
) {
    let border = theme::p().focus_ring;
    stroke(scene, root, border, 2.0, &rect(x, y, w, h));
    let box_half = 4.0;
    for (cx, cy) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
        fill(
            scene,
            root,
            border,
            &rect(cx - box_half, cy - box_half, box_half * 2.0, box_half * 2.0),
        );
    }

    if dragging {
        let label = format!("{} × {}", w.round() as u32, h.round() as u32);
        let layout = engine.simple_layout(&label, theme::role::meta());
        let lw = layout.width();
        let lh = layout.height();
        let pad = 5.0;
        // A muted chip so the digits read over any picture.
        fill(
            scene,
            root,
            theme::p().status_bg,
            &rrect(
                x + w / 2.0 - lw / 2.0 - pad,
                y + h / 2.0 - lh / 2.0 - pad,
                lw + pad * 2.0,
                lh + pad * 2.0,
                4.0,
            ),
        );
        draw_layout(
            scene,
            root,
            &layout,
            x + w / 2.0 - lw / 2.0,
            y + h / 2.0 - lh / 2.0,
            Some(theme::p().text),
        );
    }
}

/// Walk the laid-out line items. Glyph runs are drawn as glyphs; inline boxes
/// are drawn as pills via `rowlayout::visit_pills`, so what is painted and what
/// is clickable are guaranteed to be the same rect.
#[allow(clippy::too_many_arguments)]
pub fn draw_row_content(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    pills: &PillCache,
    ox: f32,
    oy: f32,
    hovered_pill: Option<AttrId>,
    selected_pill: Option<AttrId>,
    search_highlights: &[SearchHighlight],
    spelling: &[SpellingUnderline],
) {
    // Backgrounds go down first, as bands behind the glyphs. parley has no
    // background-colour property — it draws text, not boxes — so this is the
    // caller's job, the same way underline and strikethrough rects are. Inline
    // code sits beneath highlights so a span can be both.
    draw_inline_code(scene, root, row, ox, oy);
    draw_highlights(scene, root, row, ox, oy);
    draw_search_highlights(scene, root, row, ox, oy, search_highlights);

    for line in row.layout.lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(gr) = item {
                draw_glyph_run(scene, root, &gr, ox, oy, gr.style().brush.0);
                draw_decorations(scene, root, &gr, ox, oy);
            }
        }
    }
    // **The title's own strike used to be drawn here, by hand, and is gone.**
    //
    // It was written when `build_row`'s rule was `done && is_task()`, which a
    // `Kind::Document` title never satisfies — so the title needed a strike of
    // its own. `build_row` now asks the *lifecycle* (`Paragraph::struck`), and
    // that covers the title like any other row: parley strikes it, and
    // `draw_decorations` above paints it.
    //
    // Leaving it was two bugs. A completed page-task was struck **twice**, by
    // two lines a pixel or so apart; and this one read `row.done` alone, which
    // `remove_from_tasks` deliberately leaves standing (the checkbox survives a
    // row ceasing to be a task) — so a page removed from Tasks kept a strike
    // for ever, with no Reopen left anywhere to take it off.

    // Over the glyphs, like a decoration — a squiggle a descender could hide
    // is a squiggle on exactly the words (`egg`, `typo`) that need it.
    draw_spelling(scene, root, row, ox, oy, spelling);

    rowlayout::visit_pills(row, pills, |attr, info, visual, x, y, _, _| {
        draw_pill_component(
            scene,
            root,
            info.kind,
            info.trashed,
            info.dangling,
            visual,
            ox + x,
            oy + y,
            // A dangling reference reads like a completed row: still there, but
            // greyed out. It is an ordinary state, not an error.
            row.done || info.dangling,
            hovered_pill == Some(attr),
            selected_pill == Some(attr),
        );
    });
}

/// Paint the background band behind every highlighted run.
///
/// Geometry comes from `Selection`, which already knows how to turn a byte
/// range into rects that follow wrapping — so a highlight spanning a line break
/// is drawn correctly without this function knowing anything about lines.
pub fn draw_highlights(scene: &mut Scene, root: Affine, row: &Paragraph, ox: f32, oy: f32) {
    for run in &row.runs {
        let Some(name) = run.style.highlight.as_deref() else {
            continue;
        };
        let colour = theme::highlight(name);
        let range = run.range();
        let selection =
            Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
            );
        selection.geometry_with(&row.layout, |bb, _| {
            let r = Rect::new(
                bb.x0 + ox as f64,
                bb.y0 + oy as f64,
                bb.x1 + ox as f64,
                bb.y1 + oy as f64,
            );
            scene.fill(Fill::NonZero, root, colour, None, &r);
        });
    }
}

/// Paint every local-search range for one prose/source row. Geometry comes
/// from Parley, so wrapped matches remain one logical result with one rect per
/// visual line.
fn draw_search_highlights(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    ox: f32,
    oy: f32,
    highlights: &[SearchHighlight],
) {
    for highlight in highlights
        .iter()
        .filter(|highlight| highlight.node == row.id && highlight.table_cell.is_none())
    {
        draw_search_range(scene, root, &row.layout, highlight, ox, oy);
    }
}

/// Paint the squiggle under every misspelled word of one prose row.
///
/// Geometry comes from `Selection` exactly as the highlight bands' does, so a
/// word wrapped across lines waves on each of its visual lines; the vertical
/// place comes from that line's own metrics, so a squiggle in a heading sits
/// as far under its larger glyphs as a body one does under its.
fn draw_spelling(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    ox: f32,
    oy: f32,
    spelling: &[SpellingUnderline],
) {
    for underline in spelling
        .iter()
        .filter(|u| u.node == row.id && u.table_cell.is_none())
    {
        draw_spelling_range(scene, root, &row.layout, &underline.range, ox, oy);
    }
}

/// One misspelled range's squiggle over one layout — shared by prose rows and
/// rendered table cells, the same split `draw_search_range` makes.
pub fn draw_spelling_range(
    scene: &mut Scene,
    root: Affine,
    layout: &parley::Layout<theme::Brush>,
    range: &Range<usize>,
    ox: f32,
    oy: f32,
) {
    if range.start >= range.end {
        return;
    }
    let color = theme::p().spell;
    let selection = Selection::from_byte_index(layout, range.start, Affinity::Downstream).extend(
        Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
    );
    selection.geometry_with(layout, |bb, line| {
        let Some(line) = layout.get(line) else {
            return;
        };
        let m = line.metrics();
        // Just under the baseline, scaled to the line's own descent so
        // headings and body text both read as underlined words.
        let top = oy + m.baseline + (m.descent * 0.4).max(1.5);
        koru_draw::scene::squiggle(
            scene,
            root,
            color,
            ox + bb.x0 as f32,
            ox + bb.x1 as f32,
            top,
        );
    });
}

/// Shared by prose and rendered table cells.
pub fn draw_search_range(
    scene: &mut Scene,
    root: Affine,
    layout: &parley::Layout<theme::Brush>,
    highlight: &SearchHighlight,
    ox: f32,
    oy: f32,
) {
    if highlight.range.start >= highlight.range.end {
        return;
    }
    let selection =
        Selection::from_byte_index(layout, highlight.range.start, Affinity::Downstream).extend(
            Cursor::from_byte_index(layout, highlight.range.end, Affinity::Upstream),
        );
    let base = theme::selection_color();
    let colour = if highlight.active {
        let [r, g, b, a] = base.components;
        Color::new([r * 0.8, g * 0.8, b * 0.8, a])
    } else {
        base
    };
    let grow = highlight
        .activated
        .map(|started| {
            let progress = (std::time::Instant::now()
                .saturating_duration_since(started)
                .as_secs_f32()
                / 0.14)
                .clamp(0.0, 1.0);
            2.0 * (std::f32::consts::PI * progress).sin().max(0.0)
        })
        .unwrap_or(0.0);
    selection.geometry_with(layout, |bb, _| {
        let rect = Rect::new(
            bb.x0 + ox as f64 - grow as f64,
            bb.y0 + oy as f64 - grow as f64,
            bb.x1 + ox as f64 + grow as f64,
            bb.y1 + oy as f64 + grow as f64,
        );
        scene.fill(Fill::NonZero, root, colour, None, &rect);
    });
}

/// Paint the rounded background behind every inline-code run — the mono span
/// that mirrors a code block at the intra-line scale. The same `code_bg` tint
/// so the two read as one feature, with a little horizontal padding so the
/// token breathes (the GitHub/Obsidian look).
pub fn draw_inline_code(scene: &mut Scene, root: Affine, row: &Paragraph, ox: f32, oy: f32) {
    let pad = 2.0;
    for run in &row.runs {
        if !run.code {
            continue;
        }
        let range = run.range();
        let selection =
            Selection::from_byte_index(&row.layout, range.start, Affinity::Downstream).extend(
                Cursor::from_byte_index(&row.layout, range.end, Affinity::Upstream),
            );
        selection.geometry_with(&row.layout, |bb, _| {
            let r = Rect::new(
                (bb.x0 + ox as f64) - pad,
                bb.y0 + oy as f64,
                (bb.x1 + ox as f64) + pad,
                bb.y1 + oy as f64,
            );
            // A rounded rect per line of the span; the radius is capped by the
            // half-height so a single line stays a pill rather than a stadium.
            let radius = ((bb.y1 - bb.y0) * 0.5).min(4.0);
            scene.fill(
                Fill::NonZero,
                root,
                theme::p().code_bg,
                None,
                &vello::kurbo::RoundedRect::from_rect(r, radius),
            );
        });
    }
}

/// The thin overlay thumb at the right edge, only when the content is taller
/// than the band.
///
/// A track is deliberately not drawn: the thumb alone is the affordance, the way
/// it is on macOS, and a track would add chrome the rest of the interface does
/// not wear. Drawn after the view's chrome and before the status bar, but the
/// thumb is bounded to the content band so it never overlaps either.
pub fn draw_scrollbar(scene: &mut Scene, root: Affine, body: &Body, view: &ViewState) {
    let Some(g) = view.scrollbar_geom(rowlayout::content_height(body)) else {
        return;
    };
    let (px, py) = view.pointer;
    let active = view.scroll_drag.is_some() || g.in_gutter(px, py);
    koru_ui::scrollbar::draw_thumb(
        scene,
        root,
        koru_draw::geometry::Rect::new(g.thumb_x, g.thumb_top, g.thumb_w, g.thumb_h),
        active,
    );
}

/// What every view of a list of paragraphs draws, in the order it has to happen.
///
/// The order is the one thing no single layer can know, which is why it is here
/// rather than repeated in each view: background, rows, drag feedback, the
/// placeholder, then whatever chrome this view wears, then the status bar and
/// the things that float over everything.
///
/// `chrome` is a callback rather than a parameter because it draws *between* two
/// layers. A view that has none passes a closure that does nothing.
pub fn frame(
    scene: &mut Scene,
    root: Affine,
    editor: &mut dyn Editing,
    images: &ImageCache,
    f: Frame<'_>,
    chrome: impl FnOnce(&mut Scene, Affine, &Body, &mut ViewState),
) {
    let (vw, vh) = editor.view().viewport;
    // Advance the smooth scroll, if one is in flight. The frame the shell
    // asked for *is* the tick — `animating` is why it asked (ADR-26).
    let now = std::time::Instant::now();
    editor.advance_scroll_anim(now);
    editor.advance_drag_autoscroll(now);
    // Body emoji long-press (and picker cell hold when that overlay is open).
    // Without this, `body_tone_press` arms on down and is cleared on up as a
    // short click — the strip never opens.
    editor.tick_overlay_press();
    // The body must be laid out before it is drawn. Most frames arrive with
    // the rows already reflowed (a key ran `settle`, or `adopt` reflowed after
    // a remote change), in which case this is a cheap scan of clean rows. But a
    // structural edit whose rebuild landed between the last `settle` and this
    // frame — a `/divider` commit, a paste — leaves new rows dirty, and without
    // this they would paint at zero height until the next keystroke reflowed.
    editor.reflow();
    let table_controls = editor
        .view()
        .table_controls_node
        .map(|id| (id, editor.view().table_controls.visibility(now)));

    // Page mode is a presentation of the same rows: laid out at the page width,
    // centred, on a gray canvas with a page sheet that **grows with the content
    // and scrolls through the viewport** (the Google-Docs continuous model, not
    // a fixed-height window). The outline never takes it, so this is the one
    // place the shared editor knows a page exists.
    let mode = editor.view().view_mode;
    let content_x = editor.view().content_x;
    let canvas_bg = editor.view().canvas_bg;
    let scroll = editor.view().scroll;
    let content_h = editor.view().content_h;
    let layout_w = mode.effective_w(vw);
    let chrome_h = editor.view().chrome_h;
    // In Page mode this is already `chrome_h + PAGE_GAP` — the sheet's top and
    // the rows' top inset agree, so `MARGIN_TOP` becomes inside-page padding.
    let top = editor.top_inset();

    // The base background fills the whole viewport the way it always did, so a
    // view with no toolbar of its own (the outline) is unchanged. An embedded
    // editor sits on its host's background, and its viewport is wider than
    // the host's box by the editor's own margins (`ViewState::embedded`).
    let embedded = editor.view().embedded.is_some();
    if !embedded {
        fill(scene, root, theme::p().bg, &rect(0.0, 0.0, vw, vh));
    }
    let status_h = editor.view().status_h;
    let area_h = (vh - chrome_h - status_h).max(0.0);
    if mode == ViewMode::Page {
        // The canvas — the area *around* the page — fills the content area and
        // is the one thing the swatch tints (not the page). The page scrolls
        // over it, so grey shows above and below the sheet as it moves.
        let canvas_colour = canvas_bg
            .map(|c| Color::from_rgba8(c[0], c[1], c[2], c[3]))
            .unwrap_or(theme::p().canvas);
        // **Tucked a pixel under the chrome, not butted against it.** The base
        // fill above is `bg` — white in the light theme — and the chrome is
        // drawn afterwards down to exactly `chrome_h`. Two fills meeting on one
        // coordinate do not quite meet at fractional DPI: the edge antialiases
        // and the white underneath shows through as a hairline across the top
        // of the canvas, which reads as a border nobody drew. The overlap is
        // covered by the chrome, so it costs nothing visible.
        let top = (chrome_h - 1.0).max(0.0);
        fill(
            scene,
            root,
            canvas_colour,
            &rect(0.0, top, vw, area_h + (chrome_h - top)),
        );
    }

    let (body, view) = editor.parts();
    let content_root = root * Affine::translate((content_x as f64, 0.0));
    // Clip the content to the content area in Page mode (so the sheet and rows
    // never paint over the toolbar or status bar), and to the column otherwise
    // (the whole viewport in Full — a no-op, so the outline is untouched).
    let clip = view.content_clip();
    let clip = rect(clip.x, clip.y, clip.w, clip.h);
    scene.push_clip_layer(Fill::NonZero, root, &clip);

    if mode == ViewMode::Page {
        // The sheet grows with the content (`content_h` already carries the
        // trailing `MARGIN_TOP`, so `content_h - top` is page height with
        // symmetric `MARGIN_TOP` padding top and bottom) and scrolls with it.
        // A short or empty document still gets a full page — the sheet fills
        // the visible area minus its grey gaps, the way Google Docs shows one.
        let page_top = top - scroll;
        // **A sheet already touching both edges gets no gap below it either.**
        // The gaps exist to show the page floating on a canvas; once the window
        // is narrow enough that the sheet spans its full width, there is no
        // canvas left to float on and the strip at the bottom is the only grey
        // in sight — an offcut rather than a margin. Both conditions matter:
        // the sides have to be flush *and* the chrome has to be the page's own
        // (`page_gap_top == 0`, which is how a standalone window says so), or a
        // tab in a narrow container window would lose its footing against the
        // toolbar above it.
        let flush = content_x <= 0.5 && view.page_gap_top <= 0.0;
        let gap_below = if flush { 0.0 } else { theme::PAGE_GAP };
        let min_h = (area_h - view.page_gap_top - gap_below).max(0.0);
        let page_h = (content_h - top).max(min_h);
        let sheet = rrect(content_x, page_top, layout_w, page_h, theme::PAGE_RADIUS);
        // The page surface is the theme `bg` — never the swatch colour — with a
        // 1px `panel_border` hairline, the same border the toolbar's bottom
        // edge wears. No shadow: the sheet reads as a panel, not a floating
        // card, which is what "same style as the toolbar border" asks for.
        fill(scene, root, theme::p().bg, &sheet);
        stroke(scene, root, theme::p().panel_border, 1.0, &sheet);
    }

    let entity_query = match &view.overlay {
        crate::components::Overlay::Entity(picker) => body
            .row(picker.trigger.node)
            .map(|row| (picker.trigger.node, picker.trigger.start..row.caret())),
        _ => None,
    };
    // Disjoint borrows: the engine mutably (layout caches), the tag table
    // immutably (chip labels resolve through it).
    let (engine, tags, lists, reminder_chips) = (
        &mut view.engine,
        &view.tags,
        &view.lists,
        &view.reminder_chips,
    );
    draw_document(
        scene,
        content_root,
        body,
        engine,
        images,
        view.image_resize.map(|r| r.node),
        view.scroll,
        f.caret_on,
        view.hover,
        view.selected_pill,
        entity_query.or_else(|| {
            view.search_flash
                .as_ref()
                .filter(|flash| flash.until > std::time::Instant::now())
                .map(|flash| (flash.node, flash.range.clone()))
        }),
        &view.search_highlights,
        &view.spelling,
        layout_w,
        vh,
        view.active,
        view.insert_button,
        view.table_source,
        &view.table_scroll,
        view.table_cell,
        table_controls,
        &view.copy,
        tags,
        lists,
        reminder_chips,
    );
    draw_drag_feedback(scene, content_root, body, view);

    // Both hints are set exactly the way the row they stand in for is set —
    // the row's family, its step on the ramp, its weight, and the same strut
    // geometry body rows use — or an empty document would advertise itself in
    // the chrome's font, visibly off from the text the first keystroke produces.
    if let Some((x, y)) = body.placeholder_at() {
        let y = y - view.scroll;
        let row = &body.rows[body.first_body_row()];
        let hint = view.engine.placeholder_layout(
            f.placeholder,
            row.type_step(),
            row.font_choice().stack(),
            row.weight(),
        );
        draw_layout(
            scene,
            content_root,
            &hint,
            x,
            y,
            Some(theme::p().placeholder),
        );
    }
    if let Some((x, y)) = body.title_placeholder_at() {
        let row = &body.rows[0];
        let hint = view.engine.placeholder_layout(
            f.title_placeholder,
            row.type_step(),
            row.font_choice().stack(),
            row.weight(),
        );
        draw_layout(
            scene,
            content_root,
            &hint,
            x,
            y - view.scroll,
            Some(theme::p().placeholder),
        );
    }
    scene.pop_layer();
    view.copy.draw_tooltip(
        scene,
        root,
        &mut view.engine,
        koru_draw::geometry::Rect::new(0.0, 0.0, vw, vh),
        now,
    );
    // Drawn after the content layer is popped, for the same reason the copy
    // tooltip is: it is viewport furniture that must not be clipped by the
    // column it hangs off.
    let viewport = koru_draw::geometry::Rect::new(0.0, 0.0, vw, vh);
    let (tooltip, engine) = (&view.task_warning, &mut view.engine);
    tooltip.draw(scene, root, engine, viewport, now);

    // Chrome, scrollbar and status bar belong to the viewport, not the column.
    chrome(scene, root, body, view);

    // The viewport's right edge is outside an embedded editor's box, so a
    // scrollbar there would hang in the host's chrome.
    if !embedded {
        draw_scrollbar(scene, root, body, view);
    }

    // **No footer, no strip of nothing.** A view that reports `status_h == 0`
    // has already been given that height back for its rows; drawing the bar
    // anyway would put it over them.
    if view.status_h > 0.0 {
        let (left, right) = f.status;
        draw_status_bar(scene, root, &mut view.engine, view.viewport, left, right);
    }

    // The link tooltip follows the viewport pointer, so it is drawn at `root`.
    // `link_at` works in content space, so the pointer is mapped first. The
    // overlay panel is viewport-space (it anchors to the title or, shifted by
    // `content_x`, to a caret), so it too is drawn at `root` — un-translated.
    let cx = f.pointer.0 - content_x;
    let over_pill = (!editor.view().overlay.is_open())
        .then(|| editor.link_pill_at(cx, f.pointer.1))
        .flatten()
        .map(|(row, attr, url)| (row, attr, url.to_owned()));
    let over_chip = editor
        .view()
        .link_chip
        .as_ref()
        .is_some_and(|chip| chip.rect.contains(f.pointer.0, f.pointer.1));
    let tooltip = over_pill.or_else(|| {
        over_chip
            .then(|| {
                editor
                    .view()
                    .link_chip
                    .as_ref()
                    .map(|chip| (chip.row, chip.attr, chip.url.clone()))
            })
            .flatten()
    });
    // The `+` says what its two gestures do. Drawn at `root` like the link
    // tooltip, but anchored to the button rather than to the pointer: it is a
    // fixed target, so a tooltip that slid around under it would be the only
    // thing on screen that moved while the pointer did not.
    if let Some(anchor) = editor.hovered_plus_rect() {
        let progress = editor.tooltip_progress();
        if progress > 0.0 {
            draw_plus_tooltip(scene, root, editor.view_mut(), anchor, progress);
        }
    }
    if let Some((row, attr, url)) = tooltip {
        if editor.tooltip_progress() > 0.0 {
            let anchor = editor.pill_viewport_rect(row, attr);
            draw_link_tooltip(
                scene,
                root,
                editor.view_mut(),
                &url,
                row,
                attr,
                anchor,
                f.pointer,
            );
        }
    } else {
        editor.view_mut().link_chip = None;
    }
    // An embedded editor's panels float over its host's chrome, so the host
    // draws them last with [`floating_panels`], above everything of its own.
    if !embedded {
        floating_panels(scene, root, editor);
    }
}

/// The panels that float over a view — an open picker, the body's skin-tone
/// strip — at the same `root` [`frame`] was given.
///
/// [`frame`] draws these itself for a view that owns its viewport. A host that
/// embeds the editor (`ViewState::embedded`) calls this after its own chrome,
/// or its toolbar and popovers would paint over the picker.
pub fn floating_panels(scene: &mut Scene, root: Affine, editor: &mut dyn Editing) {
    if let Some(panel) = editor.overlay_panel_rect() {
        let panel = (panel.x, panel.y, panel.w, panel.h);
        draw_overlay(scene, root, editor.view_mut(), panel);
    }
    // Body emoji skin-tone strip (long-press on a tone-capable grapheme).
    draw_body_tone_strip(scene, root, editor.view_mut());
}

fn draw_body_tone_strip(scene: &mut Scene, root: Affine, view: &mut crate::view_state::ViewState) {
    use crate::components::emoji_picker;
    use koru_draw::scene::{draw_layout, fill, rrect};
    let Some(strip) = view.body_tone_strip.clone() else {
        return;
    };
    let vp = view.viewport;
    let panel = emoji_picker::tone_strip_rect_window(strip.cell, strip.tones.len(), vp);
    koru_draw::scene::floating_panel(
        scene,
        root,
        panel.x,
        panel.y,
        panel.w,
        panel.h,
        &koru_draw::scene::Panel::popover(8.0),
    );
    for (i, tone) in strip.tones.iter().enumerate() {
        let Some(cell) = emoji_picker::tone_cell_rect_window(strip.cell, strip.tones.len(), vp, i)
        else {
            continue;
        };
        if strip.hover == Some(i) {
            fill(
                scene,
                root,
                theme::selection_color(),
                &rrect(cell.x + 2.0, cell.y + 2.0, cell.w - 4.0, cell.h - 4.0, 6.0),
            );
        }
        let glyph = view.engine.simple_layout(tone, theme::role::body());
        draw_layout(
            scene,
            root,
            &glyph,
            cell.x + (cell.w - glyph.width()) * 0.5,
            cell.y + (cell.h - glyph.height()) * 0.5,
            None,
        );
    }
}

/// What a view has to say about one frame that this layer cannot work out.
pub struct Frame<'a> {
    pub caret_on: bool,
    pub pointer: (f32, f32),
    /// Shown when the body is one empty row — the view's own words.
    pub placeholder: &'a str,
    /// Shown when the document's title row is empty. Ignored by a view with no
    /// title row, which is why the outline passes an empty string rather than
    /// this being an `Option` every frame has to unwrap.
    pub title_placeholder: &'a str,
    /// The status bar's two halves.
    pub status: (&'a str, &'a str),
}

/// While a row is being dragged: a line where it would land, the width of
/// the target row's selection chip ([`rowlayout::drop_line_rect`]). Nothing
/// marks where it came from — a grey band across the whole window used to,
/// which read as a selection the press had made rather than a drag it had
/// started.
pub fn draw_drag_feedback(scene: &mut Scene, root: Affine, body: &Body, view: &ViewState) {
    let Some(drag) = view.drag_row else {
        return;
    };
    let layout_w = view.view_mode.effective_w(view.viewport.0);
    if let Some(line) = rowlayout::drop_line_rect(&body.rows, drag, layout_w, view.scroll) {
        fill(
            scene,
            root,
            theme::p().drop_indicator,
            &rrect(line.x, line.y, line.w, line.h, 1.0),
        );
    }
}

/// What the insert `+` does, on hover: two lines, each opening with the
/// gesture in bold.
///
/// On the bubble's surface rather than the panel's — it belongs to a control
/// rather than to the page, and at `panel_bg` a white card in the left margin
/// reads as a piece of the document that has come loose.
pub fn draw_plus_tooltip(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    anchor: koru_draw::geometry::Rect,
    progress: f32,
) {
    const LINES: [(&str, &str); 2] = [
        ("Click", " to add below"),
        ("Alt-click", " to add a block above"),
    ];
    let pad = 9.0;
    let mut laid = Vec::with_capacity(LINES.len());
    for (lead, rest) in LINES {
        let a = view.engine.simple_layout(
            lead,
            theme::role::hint().with_weight(theme::FontWeight::SemiBold),
        );
        let b = view.engine.simple_layout(rest, theme::role::hint());
        laid.push((a, b));
    }
    let line_h = laid.iter().map(|(a, _)| a.height()).fold(0.0_f32, f32::max);
    let w = laid
        .iter()
        .map(|(a, b)| a.width() + b.width())
        .fold(0.0_f32, f32::max)
        + pad * 2.0;
    let h = line_h * LINES.len() as f32 + pad * 2.0;

    // Centred under the button, flipped above near the floor, clamped to the
    // window — the recipe every floating thing here shares.
    let (vw, vh) = view.viewport;
    let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(8.0, (vw - w - 8.0).max(8.0));
    let below = anchor.y + anchor.h + 8.0;
    let y = if below + h > vh - view.status_h {
        (anchor.y - h - 8.0).max(8.0)
    } else {
        below
    };

    // It rises the last few pixels as it fades, so it reads as coming *from*
    // the button rather than appearing beside it — the popover's idiom, at the
    // popover's speed.
    let rise = (1.0 - progress) * theme::motion::tooltip().rise;
    let group = root * Affine::translate((0.0, rise as f64));
    let panel = koru_draw::scene::Panel::tooltip(7.0);
    let opaque = progress >= 1.0;
    if !opaque {
        // vello allocates a texture per `push_layer`, so the group is skipped
        // outright once the fade is over.
        let b = koru_draw::scene::panel_bounds(x, y, w, h, panel.elevation);
        koru_draw::scene::fade_layer(scene, group, progress, &rect(b.x, b.y, b.w, b.h));
    }
    let pal = theme::p();
    koru_draw::scene::floating_panel(scene, group, x, y, w, h, &panel);
    for (i, (lead, rest)) in laid.iter().enumerate() {
        let ly = y + pad + i as f32 * line_h;
        draw_layout(scene, group, lead, x + pad, ly, Some(pal.bubble_fg));
        draw_layout(
            scene,
            group,
            rest,
            x + pad + lead.width(),
            ly,
            Some(pal.bubble_fg),
        );
    }
    if !opaque {
        scene.pop_layer();
    }
}

const LINK_CHIP_PAD: f32 = 7.0;
const LINK_CHIP_BTN: f32 = 22.0;
const LINK_CHIP_ICON: f32 = 14.0;
const LINK_CHIP_GAP: f32 = 2.0;

fn link_chip_geom(
    url_w: f32,
    url_h: f32,
    anchor: koru_draw::geometry::Rect,
    viewport: (f32, f32),
    status_h: f32,
    url: &str,
    row: usize,
    attr: koru_core::AttrId,
) -> LinkChip {
    let buttons_w = LINK_CHIP_BTN * 2.0 + LINK_CHIP_GAP;
    let w = (url_w + buttons_w + LINK_CHIP_PAD * 3.0).max(120.0);
    let h = url_h.max(LINK_CHIP_BTN) + LINK_CHIP_PAD * 2.0;
    let (vw, vh) = viewport;
    let x = (anchor.x + anchor.w * 0.5 - w * 0.5).clamp(8.0, (vw - w - 8.0).max(8.0));
    // Overlap the pill so the pointer never falls in a gap that dismisses the chip.
    let below = anchor.y + anchor.h - 4.0;
    let y = if below + h > vh - status_h {
        (anchor.y - h + 4.0).max(8.0)
    } else {
        below
    };
    let btn_y = y + (h - LINK_CHIP_BTN) * 0.5;
    let copy = koru_draw::geometry::Rect::new(
        x + w - LINK_CHIP_PAD - LINK_CHIP_BTN * 2.0 - LINK_CHIP_GAP,
        btn_y,
        LINK_CHIP_BTN,
        LINK_CHIP_BTN,
    );
    let edit = koru_draw::geometry::Rect::new(
        copy.x + LINK_CHIP_BTN + LINK_CHIP_GAP,
        btn_y,
        LINK_CHIP_BTN,
        LINK_CHIP_BTN,
    );
    LinkChip {
        rect: koru_draw::geometry::Rect::new(x, y, w, h),
        copy,
        edit,
        url: url.to_owned(),
        row,
        attr,
    }
}

/// Hovering a link pill shows where it goes, with copy and edit.
pub fn draw_link_tooltip(
    scene: &mut Scene,
    root: Affine,
    view: &mut ViewState,
    url: &str,
    row: usize,
    attr: koru_core::AttrId,
    anchor: Option<koru_draw::geometry::Rect>,
    pointer: (f32, f32),
) {
    let buttons_w = LINK_CHIP_BTN * 2.0 + LINK_CHIP_GAP;
    let max_w = (view.viewport.0 - 24.0 - buttons_w - LINK_CHIP_PAD).max(80.0);
    let label = view
        .engine
        .simple_layout_wrapped(url, theme::role::hint(), max_w);
    let reuse = view
        .link_chip
        .as_ref()
        .is_some_and(|chip| chip.row == row && chip.attr == attr && chip.url == url);
    let chip = if reuse {
        let mut chip = view.link_chip.clone().unwrap();
        let btn_y = chip.rect.y + (chip.rect.h - LINK_CHIP_BTN) * 0.5;
        chip.copy = koru_draw::geometry::Rect::new(
            chip.rect.x + chip.rect.w - LINK_CHIP_PAD - LINK_CHIP_BTN * 2.0 - LINK_CHIP_GAP,
            btn_y,
            LINK_CHIP_BTN,
            LINK_CHIP_BTN,
        );
        chip.edit = koru_draw::geometry::Rect::new(
            chip.copy.x + LINK_CHIP_BTN + LINK_CHIP_GAP,
            btn_y,
            LINK_CHIP_BTN,
            LINK_CHIP_BTN,
        );
        chip
    } else {
        let anchor = anchor.unwrap_or(koru_draw::geometry::Rect::new(
            pointer.0, pointer.1, 1.0, 1.0,
        ));
        link_chip_geom(
            label.width(),
            label.height(),
            anchor,
            view.viewport,
            view.status_h,
            url,
            row,
            attr,
        )
    };
    let hit = chip.hit(pointer.0, pointer.1);

    let pal = theme::p();
    let mut panel = koru_draw::scene::Panel::tooltip(6.0);
    panel.elevation = koru_draw::scene::Elevation::Raised;
    koru_draw::scene::floating_panel(
        scene,
        root,
        chip.rect.x,
        chip.rect.y,
        chip.rect.w,
        chip.rect.h,
        &panel,
    );

    let text_y = chip.rect.y + (chip.rect.h - label.height()) * 0.5;
    draw_layout(
        scene,
        root,
        &label,
        chip.rect.x + LINK_CHIP_PAD,
        text_y,
        Some(pal.bubble_fg),
    );

    for (rect, glyph, active) in [
        (chip.copy, icons::COPY, hit == LinkChipHit::Copy),
        (chip.edit, icons::PENCIL, hit == LinkChipHit::Edit),
    ] {
        if active {
            fill(
                scene,
                root,
                pal.bubble_hover,
                &rrect(rect.x, rect.y, rect.w, rect.h, 4.0),
            );
        }
        let icon = view.engine.icon_layout(glyph, LINK_CHIP_ICON);
        let ix = rect.x + (rect.w - icon.width()) * 0.5;
        let iy = rect.y + (rect.h - icon.height()) * 0.5;
        draw_layout(scene, root, &icon, ix, iy, Some(pal.bubble_fg));
    }

    view.link_chip = Some(chip);
}

pub fn draw_checkbox(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    oy: f32,
    hovered: bool,
    reminder_attention: bool,
) {
    let x = checkbox_x(row.depth);
    let y = rowlayout::center_on_first_line(row, oy, theme::CHECKBOX_SIZE);
    let size = theme::CHECKBOX_SIZE;
    let box_rect = koru_draw::geometry::Rect::new(x, y, size, size);

    if hovered {
        let pad = 4.0;
        let ring = rrect(x - pad, y - pad, size + pad * 2.0, size + pad * 2.0, 5.0);
        fill(scene, root, theme::p().hover_tint, &ring);
    }
    // The box itself is `koru_draw::checkbox` — the same recipe the Tasks page
    // draws, so one task ticked in two places looks like one thing. The ring
    // above stays here: it is about this row, not about the box.
    if row.done {
        koru_draw::checkbox::ticked(scene, root, box_rect);
    } else {
        koru_draw::checkbox::outline(scene, root, box_rect, theme::p().checkbox);
    }
    if reminder_attention {
        let radius = 3.0;
        let center = (x + size - 0.5, y + 0.5);
        let color = theme::accent_tone("red")
            .expect("the built-in red accent is always present")
            .swatch;
        fill(scene, root, color, &Circle::new(center, radius as f64));
    }
}

pub(crate) fn task_has_reminder_attention(
    chips: Option<&Vec<crate::reminders::InlineChip>>,
) -> bool {
    chips.is_some_and(|chips| chips.iter().any(|chip| chip.attention))
}

/// A list item's marker: a filled bullet, or the ordinal (`3.`, `c.`, `iii.`)
/// right-aligned to the text column. The ordinal is computed from the run of
/// consecutive same-style siblings, never stored — see `WorkspaceDoc::list_position`.
///
/// Non-interactive, like a checkbox's box is not: clicking a bullet does
/// nothing in any editor, so it draws without a hit-test.
pub fn draw_list_marker(
    scene: &mut Scene,
    root: Affine,
    row: &Paragraph,
    engine: &mut TextEngine,
    oy: f32,
) {
    let Some(style) = row.list_style else { return };
    let mid = rowlayout::first_line_mid_y(row, oy);
    let right_x = marker_right_x(row.depth);

    match style {
        koru_core::ListStyle::Bullet => {
            // Centred in the right half of the gutter, level with the text ink.
            let cx = (right_x - theme::GUTTER * 0.25) as f64;
            fill(
                scene,
                root,
                theme::p().text,
                &Circle::new((cx, mid as f64), theme::BULLET_R as f64),
            );
        }
        // Checkbox-style lists draw a checkbox, not a marker — handled by the
        // `is_task()` branch in the row loop.
        koru_core::ListStyle::Checkbox => {}
        koru_core::ListStyle::Numbered
        | koru_core::ListStyle::Letter
        | koru_core::ListStyle::Roman => {
            let label = format!("{}.", style.render(row.list_index));
            let layout = engine.simple_layout(&label, theme::role::body());
            let lw = layout.width();
            let lh = layout.height();
            draw_layout(
                scene,
                root,
                &layout,
                right_x - lw,
                mid - lh * 0.5,
                Some(theme::p().text),
            );
        }
    }
}

/// The insert button in the affordance lane, left of the grip.
///
/// A plus, not a plus-in-a-circle: the circle would be a second shape in a lane
/// that already has the grip's dots, and at 20px it closes up into a blob. Its
/// rect comes from [`rowlayout::gutter_affordances`], the one function the
/// hit-test reads too.
/// The insert `+` in the left margin: two bars of this painter's own, drawn
/// as one icon — see [`margin_icon`] — on the hover tint while the pointer
/// is over it.
///
/// Phosphor's `+` and six-dot glyphs were tried here and for the grip: at
/// this size the regular weight is a faint speckle and the Fill weight a
/// blob. These stay drawn, at the weight the margin has always had.
pub fn draw_plus(scene: &mut Scene, root: Affine, row: &Paragraph, oy: f32, hovered: bool) {
    let (r, _) = rowlayout::gutter_affordances(row, oy);
    let _icon = margin_icon("Plus icon", root, r, hovered);
    if hovered {
        fill(
            scene,
            root,
            theme::p().hover_tint,
            &rrect(r.x, r.y, r.w, r.h, 5.0),
        );
    }
    let colour = if hovered {
        theme::p().text
    } else {
        theme::p().grip
    };
    let (cx, cy) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
    let arm = 4.5;
    fill(
        scene,
        root,
        colour,
        &rect(cx - arm, cy - 0.6, arm * 2.0, 1.2),
    );
    fill(
        scene,
        root,
        colour,
        &rect(cx - 0.6, cy - arm, 1.2, arm * 2.0),
    );
}

/// The row's drag handle in the left margin: six dots, the usual "grab me"
/// idiom, drawn as one icon — see [`margin_icon`]. Its rect comes from
/// [`rowlayout::gutter_affordances`], the one function the hit-test reads
/// too.
pub fn draw_grip(scene: &mut Scene, root: Affine, row: &Paragraph, oy: f32, active: bool) {
    let (_, grip) = crate::rowlayout::gutter_affordances(row, oy);
    let _icon = margin_icon("Grip icon", root, grip, active);
    let color = if active {
        theme::p().grip_active
    } else {
        theme::p().grip
    };
    for row in 0..3 {
        for col in 0..2 {
            let cx = (grip.x + 3.0 + col as f32 * 5.0) as f64;
            let cy = (grip.y + 3.0 + row as f32 * 4.5) as f64;
            fill(scene, root, color, &Circle::new((cx, cy), 1.4));
        }
    }
}

/// What a margin icon drawn from geometry leaves in the inspector: one
/// named part, with the fills under it suspended — the way a panel's shadow
/// is one record and not its sixteen rings. The grip was six anonymous
/// fills and the `+` two; an icon is one thing to point at.
fn margin_icon(
    name: &'static str,
    root: Affine,
    slot: koru_draw::geometry::Rect,
    lit: bool,
) -> (koru_draw::inspect::Scope, koru_draw::inspect::Suspension) {
    let scope = koru_draw::inspect::scope(
        name,
        "part",
        root,
        rect(slot.x, slot.y, slot.w, slot.h),
        |r| {
            r.property("drawn", "geometry, not a font glyph", "koru_editor::draw");
            r.property("lit", lit, "view state: hovered or active");
        },
    );
    (scope, koru_draw::inspect::suspend())
}

#[cfg(test)]
mod tests {
    use super::*;

    use koru_i18n::Lang;

    #[test]
    fn the_placeholder_stays_generic_until_a_device_is_named() {
        assert_eq!(
            missing_image_detail(Lang::En, None, None),
            "Waiting for a device with this image to come online."
        );
        assert_eq!(
            missing_image_detail(Lang::En, None, Some(&[])),
            "Waiting for a device with this image to come online."
        );
    }

    #[test]
    fn the_placeholder_names_the_device_it_waits_for() {
        assert_eq!(
            missing_image_detail(Lang::En, None, Some(&["Studio".to_string()])),
            "Waiting for Studio to come online."
        );
        assert_eq!(
            missing_image_detail(
                Lang::En,
                None,
                Some(&["Studio".to_string(), "Pixel 8".to_string()])
            ),
            "Waiting for Studio or Pixel 8 to come online."
        );
    }

    /// **The placeholder names the cause, and every cause gets its own
    /// sentence** (D259).
    ///
    /// One row per `TransferHold`, plus the two holds that are not one — a
    /// Profile with Cloud sync off, and a file too large to go — because all
    /// of them arrive through the same field and a person reading a
    /// placeholder is owed the one that is true.
    #[test]
    fn every_hold_says_the_thing_it_actually_means() {
        let said = |hold: &str| missing_image_detail(Lang::En, Some(hold), None);
        assert_eq!(
            said("network_metered"),
            "Waiting — this connection is metered."
        );
        assert_eq!(
            said("network_low_data"),
            "Waiting — this device is set to use less data."
        );
        assert_eq!(said("network_offline"), "Waiting — no connection.");
        assert_eq!(
            said("paused_by_person"),
            "You paused files for this Workspace."
        );
        assert_eq!(
            said("cloud_access_unavailable"),
            "Cloud sync is off for this Profile."
        );
        assert_eq!(
            said("object_too_large"),
            "This file is too large to sync today."
        );
        // A code this build has not heard of is shown as itself: a stable
        // identifier a support conversation can search for beats a sentence
        // invented for it.
        assert_eq!(said("some_future_code"), "Not here yet: some_future_code");
    }

    /// **The transport-mode hold must not mention another device** (D259).
    ///
    /// This is the defect itself: the bytes were on Koru Cloud and this
    /// device had never been switched on for the Workspace, and the
    /// placeholder said *"waiting for a device with this image to come
    /// online"* — sending a person to restart hardware they did not need.
    /// What it has to say instead is what is true and one switch away.
    #[test]
    fn the_transport_mode_hold_blames_this_device_and_never_another_one() {
        let said = missing_image_detail(Lang::En, Some("mode_refuses_transport"), None);
        assert_eq!(
            said,
            "This device is not using Koru Cloud for this Workspace. \
             Turn Cloud sync on to fetch it."
        );
        for wrong in ["come online", "another device", "offline", "waiting for"] {
            assert!(
                !said.to_lowercase().contains(wrong),
                "the mode hold must not send anybody to another machine: {said}",
            );
        }

        // And it still does not, when a peer park named devices at the same
        // time — which is the state the hardware session was actually in.
        // Both facts are true; only one of them is something a person can do
        // anything about, and it wins.
        let with_names = missing_image_detail(
            Lang::En,
            Some("mode_refuses_transport"),
            Some(&["Studio".to_string()]),
        );
        assert_eq!(with_names, said, "the hold outranks the peer's names");
        assert!(!with_names.contains("Studio"));
    }

    /// Every hold is localized — the placeholder is a surface a person reads,
    /// and this file used to hold two bare English strings.
    #[test]
    fn every_hold_has_words_in_every_language() {
        for hold in [
            "mode_refuses_transport",
            "network_metered",
            "network_low_data",
            "network_offline",
            "paused_by_person",
            "cloud_access_unavailable",
            "object_too_large",
        ] {
            for lang in Lang::ALL {
                let said = missing_image_detail(lang, Some(hold), None);
                assert!(!said.trim().is_empty(), "{hold} is blank in {}", lang.tag());
                assert!(
                    !said.contains("{}"),
                    "{hold} in {} left a placeholder unfilled",
                    lang.tag(),
                );
            }
        }
    }

    /// Every mark the placeholder paints must land inside the image's box:
    /// the detail names devices, so one line can outgrow a narrow image, and a
    /// short one has no room for it at all.
    fn placeholder_records(
        width: u32,
        height: u32,
        waiting_on: Vec<String>,
    ) -> (Vec<koru_draw::inspect::Record>, Rect) {
        placeholder_records_held(width, height, waiting_on, None)
    }

    fn placeholder_records_held(
        width: u32,
        height: u32,
        waiting_on: Vec<String>,
        hold: Option<&str>,
    ) -> (Vec<koru_draw::inspect::Record>, Rect) {
        let node = koru_core::Node::image(
            koru_core::node::BlobHash("missing".into()),
            "image/png",
            Some(width),
            Some(height),
        )
        .unwrap();
        let mut engine = TextEngine::new();
        let mut row = Paragraph::from_node(&node, 0, Vec::new(), HashMap::new(), None, 0);
        crate::rowlayout::build_row(&mut engine, &mut row, 600.0);
        let mut images = koru_draw::image::ImageCache::new();
        images.set_pending("missing", waiting_on);
        images.set_hold("missing", hold);
        let mut scene = Scene::new();
        let root = Affine::IDENTITY;
        let capture = koru_draw::inspect::Frame::begin(1, root, rect(0.0, 0.0, 600.0, 400.0));
        draw_image_row(
            &mut scene,
            root,
            &row,
            &images,
            &mut engine,
            600.0,
            0.0,
            false,
            false,
        );
        let records = capture.finish().records;
        let at = records
            .iter()
            .position(|r| r.name == "Document image")
            .expect("the placeholder is captured");
        let bounds = records[at].rect();
        let mut inside = Vec::new();
        for (i, record) in records.iter().enumerate() {
            let mut ancestor = record.parent;
            while let Some(p) = ancestor {
                if p == at {
                    inside.push(records[i].clone());
                    break;
                }
                ancestor = records[p].parent;
            }
        }
        (inside, bounds)
    }

    #[test]
    fn a_long_placeholder_detail_wraps_to_stay_inside_the_image() {
        let waiting = vec!["Win11-internal".to_string(), "Studio".to_string()];
        let (painted, bounds) = placeholder_records(200, 140, waiting);
        let detail_lines = painted.iter().filter(|r| r.name == "text line").count();
        assert!(detail_lines >= 2, "the sentence wraps inside the box");
        for record in &painted {
            let r = record.rect();
            assert!(
                r.x0 >= bounds.x0 - 0.5
                    && r.y0 >= bounds.y0 - 0.5
                    && r.x1 <= bounds.x1 + 0.5
                    && r.y1 <= bounds.y1 + 0.5,
                "{} at {r:?} overflows the image at {bounds:?}",
                record.name
            );
        }
    }

    #[test]
    fn a_short_placeholder_drops_the_detail_before_it_overflows() {
        let waiting = vec!["Win11-internal".to_string(), "Studio".to_string()];
        let (painted, bounds) = placeholder_records(200, 80, waiting);
        for record in &painted {
            let r = record.rect();
            assert!(
                r.x0 >= bounds.x0 - 0.5
                    && r.y0 >= bounds.y0 - 0.5
                    && r.x1 <= bounds.x1 + 0.5
                    && r.y1 <= bounds.y1 + 0.5,
                "{} at {r:?} overflows the image at {bounds:?}",
                record.name
            );
        }
    }

    /// **The longest hold sentence obeys the box the way the old one did.**
    ///
    /// The mode hold is two sentences and is the one most likely to be drawn,
    /// so the existing wrap-and-drop discipline has to hold for it — which it
    /// does without a second layout pass, because reading the hold changes
    /// *which* string is measured and never how many are.
    #[test]
    fn the_longest_hold_still_wraps_and_drops_inside_the_box() {
        for (w, h) in [(200, 140), (200, 80), (400, 300)] {
            let (painted, bounds) =
                placeholder_records_held(w, h, Vec::new(), Some("mode_refuses_transport"));
            for record in &painted {
                let r = record.rect();
                assert!(
                    r.x0 >= bounds.x0 - 0.5
                        && r.y0 >= bounds.y0 - 0.5
                        && r.x1 <= bounds.x1 + 0.5
                        && r.y1 <= bounds.y1 + 0.5,
                    "{} at {r:?} overflows the {w}x{h} image at {bounds:?}",
                    record.name
                );
            }
        }
    }

    // ------------------------------------------------------------- diagrams

    /// A `mermaid` row shaped for real, with `state` mirrored onto it the way
    /// a frame's `diagram::sync` would.
    fn diagram_row(engine: &mut TextEngine, state: crate::diagram::DiagramRowState) -> Paragraph {
        let node = koru_core::Node::code("flowchart LR\n  A --> B").unwrap();
        let mut row = Paragraph::from_node(&node, 0, Vec::new(), HashMap::new(), None, 0);
        row.language = Some(crate::diagram::LANGUAGE.to_owned());
        row.diagram = Some(state);
        crate::rowlayout::build_row(engine, &mut row, 600.0);
        // `build_row` shapes; `position_rows` is what gives a row its height,
        // and the painter reads that for a placeholder's box.
        row.height = row.layout.height().max(24.0);
        row
    }

    fn property<'a>(record: &'a koru_draw::inspect::Record, name: &str) -> &'a str {
        &record
            .properties
            .iter()
            .find(|property| property.name == name)
            .unwrap_or_else(|| panic!("the block records no {name:?}"))
            .value
    }

    fn block(records: &[koru_draw::inspect::Record]) -> &koru_draw::inspect::Record {
        records
            .iter()
            .find(|record| record.name == "Diagram block")
            .expect("the diagram block is captured")
    }

    #[test]
    fn the_inspector_sees_a_waiting_picture_and_the_engines_counters() {
        let mut engine = TextEngine::new();
        let row = diagram_row(
            &mut engine,
            crate::diagram::DiagramRowState {
                open: false,
                pending: true,
                empty: false,
                reason: None,
                drawing: None,
                last_good: false,
                shown: 1,
            },
        );
        let mut scene = Scene::new();
        let root = Affine::IDENTITY;
        let capture = koru_draw::inspect::Frame::begin(1, root, rect(0.0, 0.0, 600.0, 400.0));
        let scope = diagram_block_scope(root, &row, row.diagram.as_ref().unwrap(), 600.0, 0.0);
        draw_diagram_block(
            &mut scene,
            root,
            &mut engine,
            &row,
            row.diagram.as_ref().unwrap(),
            600.0,
            0.0,
            DiagramBlockFurniture {
                selected: true,
                controls_alpha: 0.0,
                hovered: None,
                copy_glyph: icons::COPY,
                copy_active: false,
                grip: None,
                plus: None,
            },
        );
        drop(scope);
        let records = capture.finish().records;
        let block = block(&records);
        assert_eq!(property(block, "state"), "pending");
        assert_eq!(property(block, "text"), "closed");
        assert_eq!(property(block, "drawing"), "none");
        assert!(
            block
                .properties
                .iter()
                .all(|property| property.name != "status"),
            "a waiting block has no status to show"
        );
        // The engine's process-wide totals, whatever they are by now.
        assert!(property(block, "inspections").contains("(draws "));
        assert!(property(block, "layouts").contains("(cancelled "));
        assert!(property(block, "layout ms").starts_with("last "));
        assert!(
            block.rect().width() > 0.0 && block.rect().height() > 0.0,
            "the record spans the placeholder: {:?}",
            block.rect()
        );
    }

    /// The one place the engine's English is shown: an inspector reads it,
    /// a page never does.
    #[test]
    fn the_inspector_names_a_failure_in_the_engines_words() {
        let mut engine = TextEngine::new();
        let row = diagram_row(
            &mut engine,
            crate::diagram::DiagramRowState {
                open: true,
                pending: false,
                empty: false,
                reason: Some(koru_diagram::Status::NotParsed {
                    line: 7,
                    message: "unexpected token".into(),
                    code: "merman.parse.diagram_parse".into(),
                }),
                drawing: None,
                last_good: false,
                shown: 2,
            },
        );
        let mut scene = Scene::new();
        let root = Affine::IDENTITY;
        let capture = koru_draw::inspect::Frame::begin(1, root, rect(0.0, 0.0, 600.0, 400.0));
        let scope = diagram_block_scope(root, &row, row.diagram.as_ref().unwrap(), 600.0, 0.0);
        draw_diagram_under_text(
            &mut scene,
            root,
            &row,
            row.diagram.as_ref().unwrap(),
            &mut engine,
            0.0,
            0.0,
            600.0,
        );
        drop(scope);
        let records = capture.finish().records;
        let block = block(&records);
        assert_eq!(property(block, "state"), "failed");
        assert_eq!(
            property(block, "status"),
            "not parsed: line 7: unexpected token (merman.parse.diagram_parse)"
        );
        assert_eq!(property(block, "text"), "open");
        assert_eq!(property(block, "drawing"), "none");
        // The reason line is drawn inside the block.
        assert!(
            records
                .iter()
                .any(|record| record.parent.is_some() && record.name == "text line"),
            "the reason line is captured under the block"
        );
    }

    /// **Everything painted for a diagram row nests under its one record** —
    /// in both states. The picture state: the panel, the ring, the four
    /// controls and the grip; the text state: the panel, the text, the
    /// reason line. Each record in the frame other than the block itself
    /// has the block as an ancestor, so the inspector's tree shows one
    /// *Diagram block* and nothing of the row beside it.
    #[test]
    fn every_primitive_of_a_diagram_row_nests_under_its_block() {
        fn ancestors_reach(
            records: &[koru_draw::inspect::Record],
            mut at: usize,
            block: usize,
        ) -> bool {
            while let Some(parent) = records[at].parent {
                if parent == block {
                    return true;
                }
                at = parent;
            }
            false
        }
        let mut engine = TextEngine::new();
        let root = Affine::IDENTITY;

        // The picture state, every piece of furniture on.
        let row = diagram_row(
            &mut engine,
            crate::diagram::DiagramRowState {
                open: false,
                pending: true,
                empty: false,
                reason: None,
                drawing: None,
                last_good: false,
                shown: 1,
            },
        );
        let mut scene = Scene::new();
        let capture = koru_draw::inspect::Frame::begin(1, root, rect(0.0, 0.0, 600.0, 400.0));
        let scope = diagram_block_scope(root, &row, row.diagram.as_ref().unwrap(), 600.0, 0.0);
        draw_diagram_block(
            &mut scene,
            root,
            &mut engine,
            &row,
            row.diagram.as_ref().unwrap(),
            600.0,
            0.0,
            DiagramBlockFurniture {
                selected: true,
                controls_alpha: 1.0,
                hovered: Some(crate::diagram::Control::Copy),
                copy_glyph: icons::CHECK,
                copy_active: false,
                grip: Some(true),
                plus: None,
            },
        );
        drop(scope);
        let records = capture.finish().records;
        let block = records
            .iter()
            .position(|record| record.name == "Diagram block")
            .expect("the block is captured");
        assert!(
            records.len() > 8,
            "panel, ring, controls, grip: {}",
            records.len()
        );
        for (index, record) in records.iter().enumerate() {
            assert!(
                index == block || ancestors_reach(&records, index, block),
                "{} ({}) is loose beside the block",
                record.name,
                record.kind
            );
        }

        // The text state: the reason line and the panel the caller paints.
        let row = diagram_row(
            &mut engine,
            crate::diagram::DiagramRowState {
                open: true,
                pending: false,
                empty: false,
                reason: Some(koru_diagram::Status::NotParsed {
                    line: 1,
                    message: "unexpected token".into(),
                    code: "merman.parse.diagram_parse".into(),
                }),
                drawing: None,
                last_good: false,
                shown: 2,
            },
        );
        let mut scene = Scene::new();
        let capture = koru_draw::inspect::Frame::begin(2, root, rect(0.0, 0.0, 600.0, 400.0));
        let scope = diagram_block_scope(root, &row, row.diagram.as_ref().unwrap(), 600.0, 0.0);
        let r = rowlayout::code_panel_rect(&row, 600.0, 0.0);
        fill(
            &mut scene,
            root,
            theme::p().code_bg,
            &rrect(r.x, r.y, r.w, r.h, 6.0),
        );
        draw_diagram_under_text(
            &mut scene,
            root,
            &row,
            row.diagram.as_ref().unwrap(),
            &mut engine,
            0.0,
            0.0,
            600.0,
        );
        drop(scope);
        let records = capture.finish().records;
        let block = records
            .iter()
            .position(|record| record.name == "Diagram block")
            .expect("the block is captured");
        assert!(
            records.len() > 2,
            "panel and reason line: {}",
            records.len()
        );
        for (index, record) in records.iter().enumerate() {
            assert!(
                index == block || ancestors_reach(&records, index, block),
                "{} ({}) is loose beside the block",
                record.name,
                record.kind
            );
        }
    }

    /// With no inspector frame armed, the scope never evaluates its closure:
    /// the state and the counters are not even formatted.
    #[test]
    fn nothing_is_recorded_about_a_diagram_while_the_inspector_is_closed() {
        assert!(!koru_draw::inspect::enabled());
        let mut engine = TextEngine::new();
        let row = diagram_row(
            &mut engine,
            crate::diagram::DiagramRowState {
                open: false,
                pending: true,
                empty: false,
                reason: None,
                drawing: None,
                last_good: false,
                shown: 3,
            },
        );
        let mut scene = Scene::new();
        let scope = diagram_block_scope(
            Affine::IDENTITY,
            &row,
            row.diagram.as_ref().unwrap(),
            600.0,
            0.0,
        );
        draw_diagram_block(
            &mut scene,
            Affine::IDENTITY,
            &mut engine,
            &row,
            row.diagram.as_ref().unwrap(),
            600.0,
            0.0,
            DiagramBlockFurniture {
                selected: false,
                controls_alpha: 0.0,
                hovered: None,
                copy_glyph: icons::COPY,
                copy_active: false,
                grip: None,
                plus: None,
            },
        );
        drop(scope);
        assert!(!koru_draw::inspect::enabled());
    }
}
