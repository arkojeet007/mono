// SPDX-License-Identifier: MPL-2.0

//! A pie chart's scene from merman's layout: the slices as arcs about a
//! centre, the percent of each on it, the legend beside and the title above.
//!
//! merman's pie layout is complete — the centre and radius, every slice's
//! angles and label point, every legend row's place — so unlike the sequence
//! layout it needs no patch. This module shifts it to the scene's origin,
//! places the text with the host's measurer, and grows the bounds to hold
//! the title and the legend labels, which merman sizes with its own font.

use std::f64::consts::TAU;

use kurbo::{Point, Rect, Vec2};
use merman::diagrams::pie::PieDiagramRenderModel;
use merman_render::model::PieDiagramLayout;

use crate::layout::Fail;
use crate::measure::{TEXT_PX, TextMeasure};
use crate::scene::{DiagramScene, Label, PieLegendItem, PieScene, PieSlice, Role, SceneBody};
use crate::{Counts, Kind};

/// The legend's colour swatch, a square: Mermaid's `legendRectSize`.
pub const LEGEND_SWATCH: f64 = 18.0;
/// Air between a swatch and its label.
const LEGEND_GAP: f64 = 6.0;
/// Air between the title's bottom and the pie's top.
const TITLE_GAP: f64 = 12.0;
/// Air around the whole drawing, so a label at its edge is never cut.
const MARGIN: f64 = 8.0;

/// Build the scene of a pie chart from merman's layout and the model the
/// layout came from, measuring every label with `measure`.
pub(crate) fn build(
    model: &PieDiagramRenderModel,
    layout: PieDiagramLayout,
    counts: Counts,
    measure: &dyn TextMeasure,
) -> Result<DiagramScene, Fail> {
    let origin = layout
        .bounds
        .as_ref()
        .map_or(Vec2::ZERO, |b| Vec2::new(b.min_x, b.min_y));
    let centre = Point::new(layout.center_x - origin.x, layout.center_y - origin.y);
    let radius = layout.outer_radius.max(0.0);
    let mut extent = Rect::from_center_size(centre, (radius * 2.0, radius * 2.0));

    // The slices: merman's angles run clockwise from twelve o'clock, in
    // radians, and its label point is relative to the centre.
    let mut slices = Vec::with_capacity(layout.slices.len());
    for (index, slice) in layout.slices.iter().enumerate() {
        let text = Label::placed(
            vec![format!("{}%", slice.percent)],
            Point::new(centre.x + slice.text_x, centre.y + slice.text_y),
            measure,
            Role::Text,
        );
        extent = extent.union(text.rect);
        slices.push(PieSlice {
            label: slice.label.clone(),
            value: slice.value,
            percent: slice.percent.max(0) as u32,
            start: slice.start_angle,
            end: if slice.is_full_circle {
                slice.start_angle + TAU
            } else {
                slice.end_angle
            },
            series: (index % 256) as u8,
            text,
        });
    }

    // The legend: a swatch per slice down the right, the label after it,
    // relative to the centre like everything merman places. Under
    // `showData` the label carries the value in brackets, as Mermaid's
    // does; merman builds that text only to measure it and hands back the
    // bare name.
    let mut legend = Vec::with_capacity(layout.legend_items.len());
    for (index, item) in layout.legend_items.iter().enumerate() {
        let swatch = Rect::from_origin_size(
            (centre.x + layout.legend_x, centre.y + item.y),
            (LEGEND_SWATCH, LEGEND_SWATCH),
        );
        let text = if model.show_data {
            format!("{} [{}]", item.label, item.value)
        } else {
            item.label.clone()
        };
        let width = f64::from(measure.measure(&text, TEXT_PX).width);
        let label = Label::placed(
            vec![text],
            Point::new(swatch.x1 + LEGEND_GAP + width / 2.0, swatch.center().y),
            measure,
            Role::Text,
        );
        extent = extent.union(swatch).union(label.rect);
        legend.push(PieLegendItem {
            swatch,
            label,
            series: (index % 256) as u8,
        });
    }

    // The title, centred above the pie.
    let title = layout
        .title
        .clone()
        .or_else(|| model.title.clone())
        .filter(|title| !title.trim().is_empty())
        .map(|title| {
            let height = f64::from(measure.measure(&title, TEXT_PX).height);
            Label::placed(
                vec![title],
                Point::new(centre.x, centre.y - radius - TITLE_GAP - height / 2.0),
                measure,
                Role::Text,
            )
        });
    if let Some(title) = &title {
        extent = extent.union(title.rect);
    }

    // Everything moves so the drawing starts at the origin with its margin.
    extent = extent.inflate(MARGIN, MARGIN);
    let fix = Vec2::new(-extent.x0, -extent.y0);
    let mut scene = PieScene {
        title,
        centre: centre + fix,
        radius,
        slices,
        legend,
    };
    for slice in &mut scene.slices {
        slice.text.translate(fix);
    }
    for item in &mut scene.legend {
        item.swatch = item.swatch + fix;
        item.label.translate(fix);
    }
    if let Some(title) = &mut scene.title {
        title.translate(fix);
    }
    Ok(DiagramScene {
        kind: Kind::Pie,
        bounds: Rect::new(0.0, 0.0, extent.width(), extent.height()),
        counts,
        acc_title: model.acc_title.clone(),
        acc_descr: model.acc_descr.clone(),
        body: SceneBody::Pie(scene),
    })
}
