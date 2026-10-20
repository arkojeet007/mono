// SPDX-License-Identifier: MPL-2.0

//! Flowchart box shapes: merman's shape names, and the outline of each shape
//! inside the box merman laid out.
//!
//! merman sizes a box for its shape (a diamond is a square of side
//! `width + height` of the label, a cylinder is taller by its caps) and
//! leaves drawing the shape to the renderer. The outlines here follow
//! Mermaid's own geometry for each shape, so a box drawn by Koru fills the
//! same area as Mermaid's SVG would, and the arrows merman routed to its
//! border meet the shape where Mermaid's do.

use kurbo::{Arc, BezPath, Circle, PathEl, Point, Rect, RoundedRect, Shape, Vec2};

use crate::scene::NodeShape;

/// The flatness arcs and circles are approximated with, in logical pixels.
const TOLERANCE: f64 = 0.05;

/// The [`NodeShape`] of merman's `layout_shape` name.
///
/// Mermaid's classic syntax maps one to one. The shapes of the
/// `@{ shape: … }` syntax that have no counterpart here map to the nearest
/// one: the circle family (`small-circle`, `filled-circle`,
/// `crossed-circle`, …) to [`NodeShape::Circle`], `framed-circle` to
/// [`NodeShape::DoubleCircle`], `choice` to [`NodeShape::Diamond`],
/// `lined-cylinder` to [`NodeShape::Cylinder`], `note` to
/// [`NodeShape::Rounded`], and every other shape (documents, folders,
/// windows, icons, …) to [`NodeShape::Rect`], which is the box merman sized
/// for it. A missing name is Mermaid's default, the rectangle.
pub(crate) fn node_shape(layout_shape: Option<&str>) -> NodeShape {
    match layout_shape.unwrap_or("squareRect") {
        "roundedRect" | "rounded" | "event" | "note" => NodeShape::Rounded,
        "stadium" | "terminal" | "pill" => NodeShape::Stadium,
        "subroutine" | "fr-rect" | "subproc" | "subprocess" | "framed-rectangle" => {
            NodeShape::Subroutine
        }
        "cylinder" | "cyl" | "db" | "database" | "lin-cyl" | "disk" | "lined-cylinder" => {
            NodeShape::Cylinder
        }
        "circle" | "circ" | "sm-circ" | "small-circle" | "start" | "f-circ" | "junction"
        | "filled-circle" | "cross-circ" | "summary" | "crossed-circle" => NodeShape::Circle,
        "doublecircle" | "dbl-circ" | "double-circle" | "fr-circ" | "framed-circle" | "stop" => {
            NodeShape::DoubleCircle
        }
        "diamond" | "question" | "diam" | "decision" | "choice" => NodeShape::Diamond,
        "hexagon" | "hex" | "prepare" => NodeShape::Hexagon,
        "lean_right" | "lean-r" | "lean-right" | "in-out" => NodeShape::LeanRight,
        "lean_left" | "lean-l" | "lean-left" | "out-in" => NodeShape::LeanLeft,
        "trapezoid" | "trap-b" | "priority" | "trapezoid-bottom" => NodeShape::TrapezoidUp,
        "inv_trapezoid" | "inv-trapezoid" | "trap-t" | "manual" | "trapezoid-top" => {
            NodeShape::TrapezoidDown
        }
        "odd" | "rect_left_inv_arrow" => NodeShape::Asymmetric,
        "text" => NodeShape::Text,
        _ => NodeShape::Rect,
    }
}

/// The closed silhouette of `shape` inside `rect`: what an arrow is clipped
/// to. Empty for [`NodeShape::Text`].
pub(crate) fn silhouette(shape: NodeShape, rect: Rect) -> BezPath {
    let (w, h) = (rect.width(), rect.height());
    let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
    let (cx, cy) = (rect.center().x, rect.center().y);
    match shape {
        NodeShape::Rect | NodeShape::Subroutine => rect.to_path(TOLERANCE),
        // Mermaid's `rx: 5`.
        NodeShape::Rounded => RoundedRect::from_rect(rect, 5.0).to_path(TOLERANCE),
        NodeShape::Stadium => RoundedRect::from_rect(rect, h / 2.0).to_path(TOLERANCE),
        NodeShape::Cylinder => {
            let ry = cylinder_cap(w);
            let mut path = BezPath::new();
            path.move_to((x0, y0 + ry));
            // Over the top cap, down the right side, under the bottom cap,
            // up the left side.
            arc_to(
                &mut path,
                (cx, y0 + ry),
                (w / 2.0, ry),
                std::f64::consts::PI,
                std::f64::consts::PI,
            );
            path.line_to((x1, y1 - ry));
            arc_to(
                &mut path,
                (cx, y1 - ry),
                (w / 2.0, ry),
                0.0,
                std::f64::consts::PI,
            );
            path.close_path();
            path
        }
        NodeShape::Circle | NodeShape::DoubleCircle => {
            Circle::new((cx, cy), w.min(h) / 2.0).to_path(TOLERANCE)
        }
        NodeShape::Diamond => polygon(&[(cx, y0), (x1, cy), (cx, y1), (x0, cy)]),
        NodeShape::Hexagon => {
            // Mermaid's shoulder: a quarter of the height.
            let m = h / 4.0;
            polygon(&[
                (x0 + m, y0),
                (x1 - m, y0),
                (x1, cy),
                (x1 - m, y1),
                (x0 + m, y1),
                (x0, cy),
            ])
        }
        // Mermaid's lean: the slanted edges span half the height.
        NodeShape::LeanRight => {
            let dx = h / 2.0;
            polygon(&[(x0 + dx, y0), (x1, y0), (x1 - dx, y1), (x0, y1)])
        }
        NodeShape::LeanLeft => {
            let dx = h / 2.0;
            polygon(&[(x0, y0), (x1 - dx, y0), (x1, y1), (x0 + dx, y1)])
        }
        NodeShape::TrapezoidUp => {
            let dx = h / 2.0;
            polygon(&[(x0 + dx, y0), (x1 - dx, y0), (x1, y1), (x0, y1)])
        }
        NodeShape::TrapezoidDown => {
            let dx = h / 2.0;
            polygon(&[(x0, y0), (x1, y0), (x1 - dx, y1), (x0 + dx, y1)])
        }
        // Mermaid's `rect_left_inv_arrow`: the notch reaches a quarter of
        // the height into the box.
        NodeShape::Asymmetric => {
            polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0 + h / 4.0, cy)])
        }
        NodeShape::Text => BezPath::new(),
    }
}

/// The outline to paint: the silhouette, then the shape's inner strokes as
/// further subpaths (see [`SceneNode::outline`](crate::scene::SceneNode)).
pub(crate) fn outline(shape: NodeShape, rect: Rect) -> BezPath {
    let mut path = silhouette(shape, rect);
    let (w, h) = (rect.width(), rect.height());
    match shape {
        // A bar 8 px inside each vertical edge.
        NodeShape::Subroutine => {
            for x in [rect.x0 + 8.0, rect.x1 - 8.0] {
                path.move_to((x, rect.y0));
                path.line_to((x, rect.y1));
            }
        }
        // The rim: the lower half of the top cap.
        NodeShape::Cylinder => {
            let ry = cylinder_cap(w);
            path.move_to((rect.x0, rect.y0 + ry));
            arc_to(
                &mut path,
                (rect.center().x, rect.y0 + ry),
                (w / 2.0, ry),
                std::f64::consts::PI,
                -std::f64::consts::PI,
            );
        }
        // Mermaid's inner circle, 5 px in.
        NodeShape::DoubleCircle => {
            let radius = (w.min(h) / 2.0 - 5.0).max(0.0);
            path.extend(Circle::new(rect.center(), radius).to_path(TOLERANCE));
        }
        _ => {}
    }
    path
}

/// Where a shape's label sits relative to the centre of its box. Mermaid
/// lowers a cylinder's label by the height of its cap and shifts a flag's
/// label right of its notch; every other shape centres it.
pub(crate) fn label_offset(shape: NodeShape, rect: Rect) -> Vec2 {
    match shape {
        NodeShape::Cylinder => Vec2::new(0.0, cylinder_cap(rect.width())),
        NodeShape::Asymmetric => Vec2::new(rect.height() / 8.0, 0.0),
        _ => Vec2::ZERO,
    }
}

/// The vertical radius of a cylinder's caps, as Mermaid derives it from the
/// width: `rx / (2.5 + w / 50)`.
fn cylinder_cap(width: f64) -> f64 {
    let rx = width / 2.0;
    rx / (2.5 + width / 50.0)
}

/// A closed polygon through `points`.
fn polygon(points: &[(f64, f64)]) -> BezPath {
    let mut path = BezPath::new();
    let mut points = points.iter().copied();
    if let Some(first) = points.next() {
        path.move_to(first);
        for point in points {
            path.line_to(point);
        }
        path.close_path();
    }
    path
}

/// Appends an elliptical arc to `path`, which must already be at the arc's
/// start: around `center` with `radii`, from `start_angle` sweeping
/// `sweep_angle` (positive clockwise on a y-down screen).
fn arc_to(
    path: &mut BezPath,
    center: impl Into<Point>,
    radii: impl Into<Vec2>,
    start_angle: f64,
    sweep_angle: f64,
) {
    let arc = Arc::new(center, radii, start_angle, sweep_angle, 0.0);
    for element in arc.append_iter(TOLERANCE) {
        if !matches!(element, PathEl::MoveTo(_)) {
            path.push(element);
        }
    }
}
