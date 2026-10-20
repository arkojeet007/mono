// SPDX-License-Identifier: MPL-2.0

//! Arrow geometry: the curve through merman's route, and clipping its ends to
//! the shapes it joins.
//!
//! merman's layout gives an edge as dagre gives it: a polyline whose first
//! and last points sit on the **boxes** of the nodes it joins, before any
//! clipping to the shape drawn inside the box. Mermaid's SVG then moves each
//! end onto the shape's own border and draws a basis spline through the
//! points. Both are done here, so the painter strokes the path and puts a
//! marker at its ends.

use kurbo::{BezPath, Line, ParamCurve, Point, Rect, Shape};

/// The cubic Bézier form of d3's `curveBasis` through `points`, which is
/// what Mermaid draws: the curve starts and ends on the first and last
/// points and is pulled toward the ones between. Two points are a straight
/// line; one is a lone move.
pub(crate) fn basis_spline(points: &[Point]) -> BezPath {
    let mut path = BezPath::new();
    let Some(&first) = points.first() else {
        return path;
    };
    path.move_to(first);
    let n = points.len();
    if n == 2 {
        path.line_to(points[1]);
        return path;
    }
    if n < 2 {
        return path;
    }
    let lerp = |a: Point, b: Point, wa: f64, wb: f64| {
        Point::new(
            (wa * a.x + wb * b.x) / (wa + wb),
            (wa * a.y + wb * b.y) / (wa + wb),
        )
    };
    path.line_to(lerp(points[0], points[1], 5.0, 1.0));
    for window in points.windows(3) {
        let (p0, p1, p2) = (window[0], window[1], window[2]);
        path.curve_to(
            lerp(p0, p1, 2.0, 1.0),
            lerp(p0, p1, 1.0, 2.0),
            Point::new(
                (p0.x + 4.0 * p1.x + p2.x) / 6.0,
                (p0.y + 4.0 * p1.y + p2.y) / 6.0,
            ),
        );
    }
    let (p0, p1) = (points[n - 2], points[n - 1]);
    path.curve_to(
        lerp(p0, p1, 2.0, 1.0),
        lerp(p0, p1, 1.0, 2.0),
        lerp(p0, p1, 1.0, 5.0),
    );
    path.line_to(p1);
    path
}

/// Moves the first point of `points` onto `outline`, where the line from
/// `center` (the node's) toward the second point leaves the shape: the
/// first crossing of that ray with the outline. Left alone when the route
/// has one point, when the second point is the centre, or when the ray does
/// not cross the outline.
pub(crate) fn clip_start_to_outline(points: &mut [Point], outline: &BezPath, center: Point) {
    let [first, aim, ..] = points else {
        return;
    };
    if let Some(hit) = exit_point(outline, center, *aim) {
        *first = hit;
    }
}

/// [`clip_start_to_outline`] for the last point, toward the one before it.
pub(crate) fn clip_end_to_outline(points: &mut [Point], outline: &BezPath, center: Point) {
    points.reverse();
    clip_start_to_outline(points, outline, center);
    points.reverse();
}

/// Where the ray from `center` toward `aim` first crosses `outline`.
fn exit_point(outline: &BezPath, center: Point, aim: Point) -> Option<Point> {
    let direction = aim - center;
    let length = direction.hypot();
    if length < 1e-9 || outline.elements().is_empty() {
        return None;
    }
    // Long enough to cross any outline, which lies within its own bounds.
    let bounds = outline.bounding_box();
    let reach = bounds.width() + bounds.height() + length + 1.0;
    let ray = Line::new(center, center + direction * (reach / length));
    first_crossing(outline, ray).map(|t| ray.eval(t))
}

/// The smallest parameter along `line` at which it crosses a segment of
/// `path`, ignoring its start.
fn first_crossing(path: &BezPath, line: Line) -> Option<f64> {
    path.segments()
        .flat_map(|segment| segment.intersect_line(line))
        .map(|hit| hit.line_t)
        .filter(|&t| t > 1e-9)
        .min_by(f64::total_cmp)
}

/// Cuts the start of `points` at the border of `rect` the way Mermaid cuts
/// a route leaving a group: the points inside the box are dropped and the
/// route starts where it crosses the border. Left alone when no point is
/// inside, or every point is.
pub(crate) fn clip_start_to_rect(points: &mut Vec<Point>, rect: Rect) {
    let inside = |p: &Point| {
        p.x > rect.x0 + 1e-6 && p.x < rect.x1 - 1e-6 && p.y > rect.y0 + 1e-6 && p.y < rect.y1 - 1e-6
    };
    let first_outside = points.iter().position(|p| !inside(p));
    let Some(k) = first_outside else {
        return;
    };
    if k == 0 {
        return;
    }
    let segment = Line::new(points[k - 1], points[k]);
    let border = rect.to_path(0.0);
    let start = first_crossing(&border, segment).map_or(points[k - 1], |t| segment.eval(t));
    points.drain(..k - 1);
    points[0] = start;
}

/// [`clip_start_to_rect`] for the end of the route.
pub(crate) fn clip_end_to_rect(points: &mut Vec<Point>, rect: Rect) {
    points.reverse();
    clip_start_to_rect(points, rect);
    points.reverse();
}
