// SPDX-License-Identifier: MPL-2.0

//! The SVG sink: the walk's shapes as `<path>`s, its text as glyph outlines.
//!
//! The file has to look like the screen on a machine without Koru's font
//! (and must not carry the font), so there is no `<text>` in it: every glyph
//! the layout positioned is read out of the face through skrifa as an
//! outline, scaled from font units to the run's pixel size, flipped — a
//! font's y runs up, the page's runs down — and placed where the screen
//! drew it. Numbers are written to three decimals, so the same scene gives
//! the same bytes on every machine.

use std::fmt::Write as _;

use kurbo::{Affine, PathEl, Point, Shape, Size, Stroke};
use parley::Layout;
use parley::layout::PositionedLayoutItem;
use peniko::Color;
use skrifa::instance::{LocationRef, NormalizedCoord, Size as FontSize};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

use super::walk::{Paint, Sink};
use crate::theme::Brush;

/// How finely a circle or rounded corner is flattened into curves.
const TOLERANCE: f64 = 0.1;

/// What the writer emitted, so a test can hold the `<path` count to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub shapes: usize,
    /// Glyphs with an outline: a space has none and gets no path.
    pub glyphs: usize,
}

pub(super) struct Svg {
    out: String,
    counts: Counts,
}

impl Svg {
    /// The document's head: the natural size, and the page behind it.
    pub(super) fn new(size: Size, background: Color) -> Self {
        let (w, h) = (num(size.width), num(size.height));
        let mut out = String::new();
        let _ = writeln!(
            out,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">"#
        );
        let _ = writeln!(
            out,
            r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
            hex(background)
        );
        Self {
            out,
            counts: Counts::default(),
        }
    }

    pub(super) fn finish(mut self) -> (String, Counts) {
        self.out.push_str("</svg>\n");
        (self.out, self.counts)
    }
}

impl Sink for Svg {
    fn shape(&mut self, transform: Affine, shape: &impl Shape, paint: &Paint) {
        let mut d = String::new();
        for element in shape.path_elements(TOLERANCE) {
            path_element(&mut d, element, transform);
        }
        if d.is_empty() {
            return;
        }
        let _ = write!(self.out, r#"<path d="{d}""#);
        match paint.fill {
            Some(color) => color_attrs(&mut self.out, "fill", color),
            None => self.out.push_str(r#" fill="none""#),
        }
        if let Some((color, stroke)) = &paint.stroke {
            color_attrs(&mut self.out, "stroke", *color);
            stroke_attrs(&mut self.out, stroke, scale_of(transform));
        }
        self.out.push_str("/>\n");
        self.counts.shapes += 1;
    }

    fn text(&mut self, transform: Affine, layout: &Layout<Brush>, x: f32, y: f32, color: Color) {
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let font = run.font();
                let Ok(font_ref) = skrifa::FontRef::from_index(font.data.as_ref(), font.index)
                else {
                    continue;
                };
                let outlines = font_ref.outline_glyphs();
                // parley hands the axis position back as raw 2.14 bits; the
                // same bits are what skrifa's instance takes.
                let coords: Vec<NormalizedCoord> = run
                    .normalized_coords()
                    .iter()
                    .map(|bits| NormalizedCoord::from_bits(*bits))
                    .collect();
                let size = FontSize::new(run.font_size());
                for glyph in glyph_run.positioned_glyphs() {
                    let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                        continue;
                    };
                    let mut pen = Pen {
                        d: String::new(),
                        origin: Point::new(f64::from(x + glyph.x), f64::from(y + glyph.y)),
                        transform,
                    };
                    let settings = DrawSettings::unhinted(size, LocationRef::new(&coords));
                    if outline.draw(settings, &mut pen).is_err() || pen.d.is_empty() {
                        continue;
                    }
                    let _ = write!(self.out, r#"<path d="{}""#, pen.d);
                    color_attrs(&mut self.out, "fill", color);
                    self.out.push_str("/>\n");
                    self.counts.glyphs += 1;
                }
            }
        }
    }
}

/// skrifa's pen: an outline in pixels with y up, written as path data with
/// y down, around the glyph's origin on the page.
struct Pen {
    d: String,
    origin: Point,
    transform: Affine,
}

impl Pen {
    fn point(&self, x: f32, y: f32) -> Point {
        self.transform * Point::new(self.origin.x + f64::from(x), self.origin.y - f64::from(y))
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        let _ = write!(self.d, "M{} {}", num(p.x), num(p.y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        let _ = write!(self.d, "L{} {}", num(p.x), num(p.y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let c = self.point(cx0, cy0);
        let p = self.point(x, y);
        let _ = write!(
            self.d,
            "Q{} {} {} {}",
            num(c.x),
            num(c.y),
            num(p.x),
            num(p.y)
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let c0 = self.point(cx0, cy0);
        let c1 = self.point(cx1, cy1);
        let p = self.point(x, y);
        let _ = write!(
            self.d,
            "C{} {} {} {} {} {}",
            num(c0.x),
            num(c0.y),
            num(c1.x),
            num(c1.y),
            num(p.x),
            num(p.y)
        );
    }

    fn close(&mut self) {
        self.d.push('Z');
    }
}

/// One element of a shape's path, transformed, as path data.
fn path_element(d: &mut String, element: PathEl, transform: Affine) {
    let at = |p: Point| transform * p;
    let _ = match element {
        PathEl::MoveTo(p) => {
            let p = at(p);
            write!(d, "M{} {}", num(p.x), num(p.y))
        }
        PathEl::LineTo(p) => {
            let p = at(p);
            write!(d, "L{} {}", num(p.x), num(p.y))
        }
        PathEl::QuadTo(c, p) => {
            let (c, p) = (at(c), at(p));
            write!(d, "Q{} {} {} {}", num(c.x), num(c.y), num(p.x), num(p.y))
        }
        PathEl::CurveTo(c0, c1, p) => {
            let (c0, c1, p) = (at(c0), at(c1), at(p));
            write!(
                d,
                "C{} {} {} {} {} {}",
                num(c0.x),
                num(c0.y),
                num(c1.x),
                num(c1.y),
                num(p.x),
                num(p.y)
            )
        }
        PathEl::ClosePath => {
            d.push('Z');
            Ok(())
        }
    };
}

/// `fill`/`stroke` as `#rrggbb`, with an `-opacity` only when there is one.
fn color_attrs(out: &mut String, attribute: &str, color: Color) {
    let _ = write!(out, r#" {attribute}="{}""#, hex(color));
    let alpha = color.components[3];
    if alpha < 1.0 {
        let _ = write!(out, r#" {attribute}-opacity="{}""#, num(f64::from(alpha)));
    }
}

/// The stroke's width, caps, join and dashes. The path's points carry
/// `transform` already, so a width in the shape's units is multiplied by the
/// transform's scale to stay in the page's.
fn stroke_attrs(out: &mut String, stroke: &Stroke, scale: f64) {
    let _ = write!(out, r#" stroke-width="{}""#, num(stroke.width * scale));
    let cap = match stroke.start_cap {
        kurbo::Cap::Butt => "butt",
        kurbo::Cap::Round => "round",
        kurbo::Cap::Square => "square",
    };
    let join = match stroke.join {
        kurbo::Join::Miter => "miter",
        kurbo::Join::Round => "round",
        kurbo::Join::Bevel => "bevel",
    };
    let _ = write!(out, r#" stroke-linecap="{cap}" stroke-linejoin="{join}""#);
    if !stroke.dash_pattern.is_empty() {
        let dashes: Vec<String> = stroke
            .dash_pattern
            .iter()
            .map(|dash| num(dash * scale))
            .collect();
        let _ = write!(out, r#" stroke-dasharray="{}""#, dashes.join(" "));
    }
}

/// The uniform scale of `transform`: the root of its determinant's size.
fn scale_of(transform: Affine) -> f64 {
    transform.determinant().abs().sqrt()
}

pub(super) fn hex(color: Color) -> String {
    let rgba = color.to_rgba8();
    format!("#{:02x}{:02x}{:02x}", rgba.r, rgba.g, rgba.b)
}

/// A number to three decimals, trailing zeros dropped: `384.513`, `1.5`,
/// `0`. Fixed precision is what makes two runs give the same bytes.
fn num(value: f64) -> String {
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    match trimmed {
        "-0" | "" => "0".to_owned(),
        other => other.to_owned(),
    }
}
