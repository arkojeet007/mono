// SPDX-License-Identifier: MPL-2.0

//! Editable table projection for `Kind::Table` rows.
//!
//! Canonical state is still Markdown in the node. This module owns the cached
//! grid geometry and painting; source parsing/serialisation stays in
//! `koru-blocks::table`, and no table state is duplicated into the document.

use parley::Layout;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;

use koru_blocks::table::{ParsedTable, SyntaxError, parse};
use koru_core::TableLayout;
use koru_draw::geometry::Rect as GeoRect;
use koru_draw::scene::{draw_layout, fill, rrect};
use koru_draw::text::TextEngine;
use koru_draw::theme::{self, Brush};
use koru_markdown::Styles;
use koru_markdown::table::{self, CellOverlay, Sizing, TableGrid};

pub const CONTROL_SIZE: f32 = 26.0;
const CONTROL_GAP: f32 = 4.0;

/// Structural commands exposed by a table's context menu. They live beside
/// parsing, layout and hit-testing so the document host only routes a choice;
/// it never learns Markdown row arithmetic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Edit {
    ToggleView,
    InsertRowAbove,
    InsertRowBelow,
    DeleteRow,
    InsertColumnBefore,
    InsertColumnAfter,
    DeleteColumn,
}

/// The width button names its next action. A Compact table offers expansion;
/// an Extended table offers fitting back to the document measure.
pub const fn layout_control_icon(layout: TableLayout) -> char {
    match layout {
        TableLayout::Compact => koru_draw::icons::TABLE_EXTEND,
        TableLayout::Extended => koru_draw::icons::TABLE_COMPACT,
    }
}

/// Validate table source through the same parser the grid uses.
pub fn parse_source(source: &str) -> Result<ParsedTable, Vec<SyntaxError>> {
    parse(source)
}

pub struct TableVisual {
    pub grid: Option<TableGrid>,
    pub errors: Vec<SyntaxError>,
    pub error_layout: Option<Layout<Brush>>,
    pub layout: TableLayout,
}

impl TableVisual {
    pub fn build(
        engine: &mut TextEngine,
        source: &str,
        available: f32,
        layout: TableLayout,
    ) -> Self {
        let sizing = match layout {
            TableLayout::Compact => Sizing::Fit,
            TableLayout::Extended => Sizing::Natural,
        };
        let rendered = table::layout(engine, source, available, sizing, &Styles::document());
        let Err(errors) = rendered else {
            return Self {
                grid: Some(rendered.unwrap()),
                errors: Vec::new(),
                error_layout: None,
                layout,
            };
        };
        let first = &errors[0];
        let error_layout = engine.simple_layout(
            &format!("Line {}: {}", first.line, first.message),
            theme::role::hint(),
        );
        Self {
            grid: None,
            errors: errors.clone(),
            error_layout: Some(error_layout),
            layout,
        }
    }
}

pub fn control_rect(row: &crate::paragraph::Paragraph, screen_y: f32) -> GeoRect {
    let viewport_width = row
        .table
        .as_ref()
        .and_then(|visual| visual.grid.as_ref())
        .map_or(0.0, |grid| grid.viewport_width);
    let x = row.text_x() + (viewport_width - CONTROL_SIZE * 2.0 - CONTROL_GAP).max(0.0);
    GeoRect::new(x, screen_y, CONTROL_SIZE, CONTROL_SIZE)
}

/// The source/grid switch sits beside the width control in grid mode. In
/// source mode it moves to the far edge so it never covers the Markdown the
/// user is editing at the start of the first line.
pub fn mode_control_rect(
    row: &crate::paragraph::Paragraph,
    visual: &TableVisual,
    screen_y: f32,
) -> GeoRect {
    let viewport_width = visual.grid.as_ref().map_or(0.0, |grid| grid.viewport_width);
    let x = row.text_x() + (viewport_width - CONTROL_SIZE).max(0.0);
    GeoRect::new(x, screen_y, CONTROL_SIZE, CONTROL_SIZE)
}

/// Cell under a document-local point. Row zero is the header.
pub fn cell_at(
    row: &crate::paragraph::Paragraph,
    visual: &TableVisual,
    x: f32,
    y: f32,
    scroll_x: f32,
) -> Option<(usize, usize)> {
    table::cell_at(visual.grid.as_ref()?, (row.text_x(), row.y), x, y, scroll_x)
}

/// Cell that owns a context-menu request anywhere across the table block.
///
/// A rendered Extended table may occupy only the left side of its document
/// measure. That unused width is still part of the table row, so a right-click
/// there should open the table menu rather than the generic document menu.
/// Clamp to the nearest cell for command targeting; ordinary left-clicks keep
/// using [`cell_at`] and therefore do not select a cell from empty space.
pub fn context_cell_at(
    row: &crate::paragraph::Paragraph,
    visual: &TableVisual,
    x: f32,
    y: f32,
    scroll_x: f32,
) -> Option<(usize, usize)> {
    let grid = visual.grid.as_ref()?;
    if grid.row_heights.is_empty() || grid.columns.is_empty() {
        return None;
    }

    let mut top = row.y;
    let row_index = grid
        .row_heights
        .iter()
        .enumerate()
        .find_map(|(index, height)| {
            let hit = y < top + *height;
            top += *height;
            hit.then_some(index)
        })
        .unwrap_or(grid.row_heights.len() - 1);

    let mut left = row.text_x() - scroll_x;
    let column = if x < left {
        0
    } else {
        grid.columns
            .iter()
            .enumerate()
            .find_map(|(index, width)| {
                let hit = x < left + *width;
                left += *width;
                hit.then_some(index)
            })
            .unwrap_or(grid.columns.len() - 1)
    };
    Some((row_index, column))
}

pub fn source_range(
    visual: &TableVisual,
    row: usize,
    column: usize,
) -> Option<std::ops::Range<usize>> {
    let parsed = &visual.grid.as_ref()?.parsed;
    if row == 0 {
        parsed
            .header
            .get(column)
            .map(|cell| cell.source_range.clone())
    } else {
        parsed
            .rows
            .get(row - 1)?
            .get(column)
            .map(|cell| cell.source_range.clone())
    }
}

pub fn draw(
    scene: &mut Scene,
    root: Affine,
    row: &crate::paragraph::Paragraph,
    visual: &TableVisual,
    engine: &mut TextEngine,
    screen_y: f32,
    scroll_x: f32,
    controls_alpha: f32,
    layout_hovered: bool,
    mode_hovered: bool,
    selected: Option<(usize, usize)>,
    search_highlights: &[crate::view_state::SearchHighlight],
    spelling: &[crate::view_state::SpellingUnderline],
) {
    let Some(grid) = visual.grid.as_ref() else {
        return;
    };
    let x = row.text_x();
    let clip = vello::kurbo::Rect::new(
        x as f64,
        screen_y as f64,
        (x + grid.viewport_width) as f64,
        (screen_y + grid.height) as f64,
    );
    scene.push_clip_layer(Fill::NonZero, root, &clip);
    let overlay = EditorOverlay {
        node: row.id,
        selected,
        search_highlights,
        spelling,
    };
    table::draw_grid(scene, root, grid, x, screen_y, scroll_x, Some(&overlay));
    // Floating furniture: drawn over the grid and excluded from its height.
    // It occupies no layout space and is invisible until the shared tooltip
    // interaction clock says the pointer has rested intentionally.
    draw_control(
        scene,
        root,
        engine,
        control_rect(row, screen_y),
        layout_control_icon(visual.layout),
        layout_hovered,
        controls_alpha,
    );
    draw_control(
        scene,
        root,
        engine,
        mode_control_rect(row, visual, screen_y),
        koru_draw::icons::CODE,
        mode_hovered,
        controls_alpha,
    );
    scene.pop_layer();
}

struct EditorOverlay<'a> {
    node: koru_core::NodeId,
    selected: Option<(usize, usize)>,
    search_highlights: &'a [crate::view_state::SearchHighlight],
    spelling: &'a [crate::view_state::SpellingUnderline],
}

impl CellOverlay for EditorOverlay<'_> {
    fn background(
        &self,
        scene: &mut Scene,
        root: Affine,
        cell: vello::kurbo::Rect,
        row: usize,
        col: usize,
    ) {
        if self.selected == Some((row, col)) {
            fill(scene, root, theme::selection_color(), &cell);
        }
    }

    fn under_text(
        &self,
        scene: &mut Scene,
        root: Affine,
        cell: vello::kurbo::Rect,
        row: usize,
        col: usize,
        layout: &Layout<Brush>,
    ) {
        for highlight in self.search_highlights.iter().filter(|highlight| {
            highlight.node == self.node && highlight.table_cell == Some((row, col))
        }) {
            crate::draw::draw_search_range(
                scene,
                root,
                layout,
                highlight,
                cell.x0 as f32 + table::CELL_PAD_X,
                cell.y0 as f32 + table::CELL_PAD_Y,
            );
        }
        for underline in self.spelling.iter().filter(|underline| {
            underline.node == self.node && underline.table_cell == Some((row, col))
        }) {
            crate::draw::draw_spelling_range(
                scene,
                root,
                layout,
                &underline.range,
                cell.x0 as f32 + table::CELL_PAD_X,
                cell.y0 as f32 + table::CELL_PAD_Y,
            );
        }
    }
}

/// Draw the grid switch while the canonical Markdown is visible. This is
/// deliberately outside `draw`: the source row follows the ordinary text path
/// so its selection and caret remain native editor behavior.
pub fn draw_source_control(
    scene: &mut Scene,
    root: Affine,
    row: &crate::paragraph::Paragraph,
    visual: &TableVisual,
    engine: &mut TextEngine,
    screen_y: f32,
    hovered: bool,
    alpha: f32,
) {
    draw_control(
        scene,
        root,
        engine,
        mode_control_rect(row, visual, screen_y),
        koru_draw::icons::TABLE,
        hovered,
        alpha,
    );
}

/// One floating block control: a rounded chip with a glyph, faded in by the
/// shared tooltip clock's `alpha`. A table's two controls and a diagram's
/// *Edit text* are all this, so a control looks the same on every block.
pub(crate) fn draw_control(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    control: GeoRect,
    glyph: char,
    hovered: bool,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    fill(
        scene,
        root,
        if hovered {
            theme::p().hover_tint
        } else {
            theme::p().chip_bg
        }
        .multiply_alpha(alpha),
        &rrect(control.x, control.y, control.w, control.h, 4.0),
    );
    let icon = engine.icon_layout(glyph, 16.0);
    draw_layout(
        scene,
        root,
        &icon,
        control.x + 5.0,
        control.y + 5.0,
        Some(
            if hovered {
                theme::p().text
            } else {
                theme::p().text_dim
            }
            .multiply_alpha(alpha),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_modes_offer_distinct_inverse_actions() {
        assert_eq!(
            layout_control_icon(TableLayout::Compact),
            koru_draw::icons::TABLE_EXTEND
        );
        assert_eq!(
            layout_control_icon(TableLayout::Extended),
            koru_draw::icons::TABLE_COMPACT
        );
        assert_ne!(
            layout_control_icon(TableLayout::Compact),
            layout_control_icon(TableLayout::Extended)
        );
    }

    #[test]
    fn compact_cells_wrap_an_unbroken_value_inside_its_column() {
        let mut engine = TextEngine::new();
        let visual = TableVisual::build(
            &mut engine,
            "| App | State |\n| --- | --- |\n| an-uninterrupted-product-name | yes |",
            120.0,
            TableLayout::Compact,
        );
        let first_body_cell = &visual.grid.as_ref().unwrap().rows[1][0];
        assert!(
            first_body_cell.lines().count() > 1,
            "an unbroken token must wrap rather than paint into the next column"
        );
    }

    #[test]
    fn build_delegates_grid_widths_to_koru_markdown() {
        let source = "| App | State |\n| --- | --- |\n| Koru | ready |";
        let mut engine = TextEngine::new();
        let visual = TableVisual::build(&mut engine, source, 240.0, TableLayout::Compact);
        let mut direct_engine = TextEngine::new();
        let direct = table::layout(
            &mut direct_engine,
            source,
            240.0,
            Sizing::Fit,
            &Styles::document(),
        )
        .unwrap();
        assert_eq!(visual.grid.as_ref().unwrap().columns, direct.columns);
    }
}
