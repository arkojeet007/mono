// SPDX-License-Identifier: MPL-2.0

//! One walk over a scene for every output.
//!
//! The painter and the SVG writer have to agree on every shape, stroke width
//! and glyph position, or the saved file stops looking like the screen. They
//! agree by construction: each is a [`Sink`], and [`walk`] is the only place
//! a scene's geometry becomes paint. The vello sink encodes; the SVG sink
//! writes. Neither decides what a diagram looks like.
//!
//! Geometry first, then every line of text, so no shape is ever drawn over
//! a label. Text positions come from the scene (`LabelLine::x` and
//! `baseline`) and the first line of the shaped layout; nothing here
//! measures.

use std::sync::OnceLock;

use koru_diagram::{
    DiagramScene, FlowchartScene, Frame, Label, LabelLine, Marker, Participant, ParticipantKind,
    Role, SceneBody, SequenceScene, StrokeKind,
};
use kurbo::{
    Affine, BezPath, Cap, Circle, Join, Line, ParamCurve, ParamCurveDeriv, PathSeg, Point, Rect,
    RoundedRect, Shape, Stroke, Vec2,
};
use parley::Layout;
use peniko::{Color, Fill};
use vello::Scene;

use super::{
    ACTIVATION_ALPHA, ACTIVATION_OUTLINE_W, ACTOR_ARM, ACTOR_GAP, ACTOR_HEAD_R, ACTOR_LEG,
    ACTOR_MIN_H, ACTOR_W, ARROW_LEN, ARROW_W, BOX_OUTLINE_W, CIRCLE_R, CLUSTER_OUTLINE_W,
    CLUSTER_RADIUS, CROSS_SIZE, EDGE_DASH, EDGE_LABEL_PAD, EDGE_THICK_W, EDGE_W, FRAME_DASH,
    FRAME_OUTLINE_W, FRAME_TAG_PAD_X, FRAME_TAG_PAD_Y, LIFELINE_DASH, LIFELINE_W, NODE_OUTLINE_W,
    NOTE_OUTLINE_W, NUMBER_PAD_X, NUMBER_R, PARTICIPANT_RADIUS, PreparedDiagram, ink,
};
use crate::inspect;
use crate::theme::{Brush, Palette};

/// Where the walk's paint goes.
pub(super) trait Sink {
    /// A shape under `transform`, filled and/or stroked. The stroke's width
    /// and dashes are in the shape's own units, as vello takes them.
    fn shape(&mut self, transform: Affine, shape: &impl Shape, paint: &Paint);
    /// A shaped line of text with its top-left at `(x, y)` under `transform`,
    /// in `color`.
    fn text(&mut self, transform: Affine, layout: &Layout<Brush>, x: f32, y: f32, color: Color);
}

/// How a shape is painted.
pub(super) struct Paint {
    pub fill: Option<Color>,
    pub stroke: Option<(Color, Stroke)>,
}

impl Paint {
    fn fill(color: Color) -> Self {
        Self {
            fill: Some(color),
            stroke: None,
        }
    }

    fn stroke(color: Color, stroke: Stroke) -> Self {
        Self {
            fill: None,
            stroke: Some((color, stroke)),
        }
    }

    fn both(fill: Color, outline: Color, stroke: Stroke) -> Self {
        Self {
            fill: Some(fill),
            stroke: Some((outline, stroke)),
        }
    }
}

/// The vello sink: encodes straight into a scene.
pub(super) struct Vello<'a>(pub &'a mut Scene);

impl Sink for Vello<'_> {
    fn shape(&mut self, transform: Affine, shape: &impl Shape, paint: &Paint) {
        if let Some(color) = paint.fill {
            self.0.fill(Fill::NonZero, transform, color, None, shape);
        }
        if let Some((color, stroke)) = &paint.stroke {
            self.0.stroke(stroke, transform, *color, None, shape);
        }
    }

    fn text(&mut self, transform: Affine, layout: &Layout<Brush>, x: f32, y: f32, color: Color) {
        crate::scene::draw_layout(self.0, transform, layout, x, y, Some(color));
    }
}

/// How a line of text is inked: by its role, or on the accent badge.
#[derive(Clone, Copy)]
pub(super) enum Ink {
    Role(Role),
    OnAccent,
}

/// One placed line of a label as the walk visits it.
pub(super) struct Placed<'a> {
    pub line: &'a LabelLine,
    pub ink: Ink,
    /// A second copy this far down: a participant's label is placed in its
    /// top box and drawn again in its bottom one.
    pub mirror: Option<f64>,
}

/// Every line of text in `scene`, in one fixed order — the order
/// [`PreparedDiagram::new`] shapes them in and the painter reads them back.
pub(super) fn each_line(scene: &DiagramScene, mut f: impl FnMut(Placed<'_>)) {
    let mut label = |label: &Label, ink: Ink, mirror: Option<f64>| {
        for line in &label.lines {
            f(Placed { line, ink, mirror });
        }
    };
    let by_role = |label: &Label| Ink::Role(label.role);
    match &scene.body {
        SceneBody::Flowchart(flowchart) => {
            for cluster in &flowchart.clusters {
                if let Some(title) = &cluster.title {
                    label(title, by_role(title), None);
                }
            }
            for node in &flowchart.nodes {
                label(&node.label, by_role(&node.label), None);
            }
            for edge in &flowchart.edges {
                if let Some(text) = &edge.label {
                    label(text, by_role(text), None);
                }
            }
        }
        SceneBody::Sequence(sequence) => {
            if let Some(title) = &sequence.title {
                label(title, by_role(title), None);
            }
            for group in &sequence.boxes {
                if let Some(title) = &group.title {
                    label(title, by_role(title), None);
                }
            }
            for frame in &sequence.frames {
                label(&frame.title, by_role(&frame.title), None);
                for section in &frame.sections {
                    if let Some(text) = &section.label {
                        label(text, by_role(text), None);
                    }
                }
            }
            for participant in &sequence.participants {
                let mirror = participant.bottom.y0 - participant.top.y0;
                label(
                    &participant.label,
                    by_role(&participant.label),
                    Some(mirror),
                );
            }
            for note in &sequence.notes {
                label(&note.label, by_role(&note.label), None);
            }
            for message in &sequence.messages {
                label(&message.label, by_role(&message.label), None);
            }
            for message in &sequence.messages {
                if let Some(number) = &message.number {
                    label(number, Ink::OnAccent, None);
                }
            }
        }
    }
}

/// Paints `prepared` into `sink`: the geometry, then the text.
pub(super) fn walk<S: Sink>(
    prepared: &PreparedDiagram,
    palette: &Palette,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    let scene = prepared.scene();
    match &scene.body {
        SceneBody::Flowchart(flowchart) => {
            draw_flowchart(flowchart, palette, transform, scale, sink);
        }
        SceneBody::Sequence(sequence) => {
            draw_sequence(sequence, prepared, palette, transform, scale, sink);
        }
    }
    // Text last, over everything. Lines are read back in the order they were
    // shaped; a count that disagreed could only come from a scene mutated
    // behind its `Arc`, and the honest answer then is fewer labels, not a
    // panic in the frame.
    let mut layouts = prepared.lines.iter();
    each_line(scene, |placed| {
        let Some(layout) = layouts.next() else {
            return;
        };
        let color = match placed.ink {
            Ink::Role(role) => ink(palette, role),
            Ink::OnAccent => palette.on_accent,
        };
        let x = placed.line.x as f32;
        let top = placed.line.baseline as f32 - first_baseline(layout);
        sink.text(transform, layout, x, top, color);
        if let Some(dy) = placed.mirror {
            sink.text(transform, layout, x, top + dy as f32, color);
        }
    });
    if let SceneBody::Sequence(sequence) = &scene.body {
        let tag_ink = frame_tag_ink(palette);
        for (frame, tag) in sequence.frames.iter().zip(&prepared.frame_tags) {
            sink.text(
                transform,
                tag,
                (frame.rect.x0 + FRAME_TAG_PAD_X) as f32,
                (frame.rect.y0 + FRAME_TAG_PAD_Y) as f32,
                tag_ink,
            );
        }
    }
}

// ----------------------------------------------------------------- flowchart

fn draw_flowchart<S: Sink>(
    flowchart: &FlowchartScene,
    palette: &Palette,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    let line = palette.text;
    // Groups first, outermost first as the scene orders them, so a nested
    // group's outline sits over its parent's.
    for cluster in &flowchart.clusters {
        sink.shape(
            transform,
            &cluster.rect.to_rounded_rect(CLUSTER_RADIUS),
            &Paint::stroke(palette.panel_border, hairline(CLUSTER_OUTLINE_W, scale)),
        );
    }
    // A box: its silhouette filled, every subpath stroked. The outline is
    // filled whole with the nonzero rule, which the scene promises is the
    // silhouette's fill — the decorations are open strokes with no area, or
    // a ring inside the silhouette that fills the same color.
    for node in &flowchart.nodes {
        if node.outline.is_empty() {
            continue;
        }
        sink.shape(
            transform,
            &node.outline,
            &Paint::both(
                palette.panel_bg,
                palette.panel_border,
                hairline(NODE_OUTLINE_W, scale),
            ),
        );
    }
    for edge in &flowchart.edges {
        sink.shape(
            transform,
            &edge.path,
            &Paint::stroke(line, edge_stroke(edge.stroke, scale)),
        );
        markers(
            &edge.path,
            edge.start,
            edge.end,
            line,
            edge_width(edge.stroke),
            transform,
            scale,
            sink,
        );
    }
    // The page shows through around an arrow's label, so the line never runs
    // through the words — after every arrow, so a crossing arrow cannot
    // either.
    for edge in &flowchart.edges {
        if let Some(label) = &edge.label
            && !label.lines.is_empty()
        {
            sink.shape(
                transform,
                &label.rect.inflate(EDGE_LABEL_PAD, EDGE_LABEL_PAD),
                &Paint::fill(palette.bg),
            );
        }
    }
}

// ------------------------------------------------------------------ sequence

fn draw_sequence<S: Sink>(
    sequence: &SequenceScene,
    prepared: &PreparedDiagram,
    palette: &Palette,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    let line = palette.text;
    for group in &sequence.boxes {
        sink.shape(
            transform,
            &group.rect,
            &Paint::stroke(palette.panel_border, hairline(BOX_OUTLINE_W, scale)),
        );
    }
    for lifeline in &sequence.lifelines {
        sink.shape(
            transform,
            &Line::new((lifeline.x, lifeline.y0), (lifeline.x, lifeline.y1)),
            &Paint::stroke(palette.text_dim, dashed(LIFELINE_W, LIFELINE_DASH, scale)),
        );
    }
    // The page shows through around a frame's title and its dividers'
    // labels, so a lifeline never runs through the words — under the
    // activation bars, which a patch must not notch.
    for frame in &sequence.frames {
        let labels = std::iter::once(&frame.title).chain(
            frame
                .sections
                .iter()
                .filter_map(|section| section.label.as_ref()),
        );
        for label in labels.filter(|label| !label.lines.is_empty()) {
            sink.shape(
                transform,
                &label.rect.inflate(EDGE_LABEL_PAD, EDGE_LABEL_PAD),
                &Paint::fill(palette.bg),
            );
        }
    }
    for bar in &sequence.activations {
        sink.shape(
            transform,
            bar,
            &Paint::both(
                palette.accent.with_alpha(ACTIVATION_ALPHA),
                palette.accent,
                hairline(ACTIVATION_OUTLINE_W, scale),
            ),
        );
    }
    for participant in &sequence.participants {
        draw_participant(participant, palette, transform, scale, sink);
    }
    for note in &sequence.notes {
        sink.shape(
            transform,
            &note.rect,
            &Paint::both(
                palette.panel_bg,
                palette.panel_border,
                hairline(NOTE_OUTLINE_W, scale),
            ),
        );
    }
    for (frame, tag) in sequence.frames.iter().zip(&prepared.frame_tags) {
        sink.shape(
            transform,
            &frame.rect,
            &Paint::stroke(palette.panel_border, hairline(FRAME_OUTLINE_W, scale)),
        );
        sink.shape(
            transform,
            &frame_tag_rect(frame, tag),
            &Paint::fill(palette.panel_border),
        );
        for section in &frame.sections {
            sink.shape(
                transform,
                &Line::new((frame.rect.x0, section.y), (frame.rect.x1, section.y)),
                &Paint::stroke(
                    palette.panel_border,
                    dashed(FRAME_OUTLINE_W, FRAME_DASH, scale),
                ),
            );
        }
    }
    for message in &sequence.messages {
        sink.shape(
            transform,
            &message.path,
            &Paint::stroke(line, edge_stroke(message.stroke, scale)),
        );
        markers(
            &message.path,
            message.start,
            message.end,
            line,
            edge_width(message.stroke),
            transform,
            scale,
            sink,
        );
    }
    for message in &sequence.messages {
        if let Some(number) = &message.number {
            sink.shape(
                transform,
                &number_badge(number, scale),
                &Paint::fill(palette.accent),
            );
        }
    }
}

fn draw_participant<S: Sink>(
    participant: &Participant,
    palette: &Palette,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    let top = participant.top;
    for rect in [top, participant.bottom] {
        match participant.kind {
            ParticipantKind::Box => sink.shape(
                transform,
                &rect.to_rounded_rect(PARTICIPANT_RADIUS),
                &Paint::both(
                    palette.panel_bg,
                    palette.panel_border,
                    hairline(NODE_OUTLINE_W, scale),
                ),
            ),
            ParticipantKind::Actor => {
                // The figure stands above the label, in whatever room the
                // box leaves between its top and the label's — the same
                // room in the bottom box, which mirrors the top one.
                let room = participant.label.rect.y0 - top.y0 - ACTOR_GAP;
                let figure = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + room.max(ACTOR_MIN_H));
                draw_actor(figure, palette, transform, scale, sink);
            }
        }
    }
}

/// A stick figure filling `room`: head, body, arms, legs.
fn draw_actor<S: Sink>(room: Rect, palette: &Palette, transform: Affine, scale: f64, sink: &mut S) {
    let stroke = hairline(ACTOR_W, scale);
    let line = palette.text;
    let cx = room.center().x;
    let head = Point::new(cx, room.y0 + ACTOR_HEAD_R + ACTOR_W);
    sink.shape(
        transform,
        &Circle::new(head, ACTOR_HEAD_R),
        &Paint::both(palette.panel_bg, line, stroke.clone()),
    );
    let neck = head.y + ACTOR_HEAD_R;
    let hip = neck + (room.y1 - neck) * 0.55;
    let shoulders = neck + (hip - neck) * 0.3;
    let strokes = [
        Line::new((cx, neck), (cx, hip)),
        Line::new((cx - ACTOR_ARM, shoulders), (cx + ACTOR_ARM, shoulders)),
        Line::new((cx, hip), (cx - ACTOR_LEG, room.y1)),
        Line::new((cx, hip), (cx + ACTOR_LEG, room.y1)),
    ];
    for segment in &strokes {
        sink.shape(transform, segment, &Paint::stroke(line, stroke.clone()));
    }
}

/// The tag box a frame's keyword sits in, at the frame's top-left corner.
/// The badge under a message's number: a circle of [`NUMBER_R`] on screen
/// whatever the column scale, widened into a capsule of the same height
/// when the number with [`NUMBER_PAD_X`] either side does not fit in it.
pub(super) fn number_badge(number: &Label, scale: f64) -> RoundedRect {
    let radius = NUMBER_R / scale;
    let half_width = radius.max(number.rect.width() / 2.0 + NUMBER_PAD_X / scale);
    RoundedRect::from_rect(
        Rect::from_center_size(number.rect.center(), (half_width * 2.0, radius * 2.0)),
        radius,
    )
}

fn frame_tag_rect(frame: &Frame, tag: &Layout<Brush>) -> Rect {
    let line_height = tag
        .lines()
        .next()
        .map_or(0.0, |line| line.metrics().line_height);
    Rect::new(
        frame.rect.x0,
        frame.rect.y0,
        frame.rect.x0 + f64::from(tag.width()) + FRAME_TAG_PAD_X * 2.0,
        frame.rect.y0 + f64::from(line_height) + FRAME_TAG_PAD_Y * 2.0,
    )
}

/// The ink of a frame's keyword on its `panel_border` tag box: whichever of
/// `on_accent`, `bg` and `text` reads best against it. `panel_border` is a
/// hairline tint one step from the page in both themes, so the two colors
/// made for the accent and the page have almost no contrast on it and
/// `text` is what wins; the check is kept so a palette that changes the
/// border's role picks the right one.
pub(super) fn frame_tag_ink(palette: &Palette) -> Color {
    [palette.on_accent, palette.bg, palette.text]
        .into_iter()
        .max_by(|a, b| {
            let contrast = |c: Color| inspect::contrast(c, palette.panel_border).unwrap_or(0.0);
            contrast(*a).total_cmp(&contrast(*b))
        })
        .unwrap_or(palette.text)
}

// ------------------------------------------------------------------- strokes

/// A `width` line, as thick on screen at `scale` as at 1.
fn hairline(width: f64, scale: f64) -> Stroke {
    Stroke::new(width / scale)
}

/// A dashed `width` line with `pattern`, both corrected for `scale`.
fn dashed(width: f64, pattern: [f64; 2], scale: f64) -> Stroke {
    hairline(width, scale).with_dashes(0.0, pattern.map(|dash| dash / scale))
}

/// The width an arrow or message of `kind` is drawn at, before scaling.
fn edge_width(kind: StrokeKind) -> f64 {
    match kind {
        StrokeKind::Normal | StrokeKind::Dotted => EDGE_W,
        StrokeKind::Thick => EDGE_THICK_W,
    }
}

/// An arrow's stroke: butt caps, so the line ends exactly under its marker
/// rather than poking a round nub past the tip.
fn edge_stroke(kind: StrokeKind, scale: f64) -> Stroke {
    let stroke = hairline(edge_width(kind), scale)
        .with_caps(Cap::Butt)
        .with_join(Join::Round);
    match kind {
        StrokeKind::Dotted => stroke.with_dashes(0.0, EDGE_DASH.map(|dash| dash / scale)),
        StrokeKind::Normal | StrokeKind::Thick => stroke,
    }
}

// ------------------------------------------------------------------- markers

/// Both ends of `path`.
#[allow(clippy::too_many_arguments)]
fn markers<S: Sink>(
    path: &BezPath,
    start: Marker,
    end: Marker,
    color: Color,
    line_width: f64,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    if end != Marker::None
        && let Some((tip, direction)) = end_tangent(path)
    {
        marker(
            end, tip, direction, color, line_width, transform, scale, sink,
        );
    }
    if start != Marker::None
        && let Some((tip, direction)) = start_tangent(path)
    {
        marker(
            start, tip, direction, color, line_width, transform, scale, sink,
        );
    }
}

/// One marker with its tip at `tip`, pointing along `direction` (a unit
/// vector, out of the line). Drawn in a frame of its own — translated to the
/// tip, rotated to the line, and scaled by `1 / scale` — so the marker
/// shapes are constants and the one division keeps them the same size on
/// screen however far the drawing is scaled down.
#[allow(clippy::too_many_arguments)]
fn marker<S: Sink>(
    kind: Marker,
    tip: Point,
    direction: Vec2,
    color: Color,
    line_width: f64,
    transform: Affine,
    scale: f64,
    sink: &mut S,
) {
    let local = transform
        * Affine::translate(tip.to_vec2())
        * Affine::rotate(direction.atan2())
        * Affine::scale(1.0 / scale);
    let stroke = Stroke::new(line_width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    match kind {
        Marker::None => {}
        Marker::Arrow => sink.shape(local, arrow_head(), &Paint::fill(color)),
        Marker::OpenArrow => sink.shape(local, open_head(), &Paint::stroke(color, stroke)),
        Marker::Circle => sink.shape(
            local,
            &Circle::new((0.0, 0.0), CIRCLE_R),
            &Paint::fill(color),
        ),
        Marker::Cross => {
            let half = CROSS_SIZE / 2.0;
            sink.shape(
                local,
                &Line::new((-half, -half), (half, half)),
                &Paint::stroke(color, stroke.clone()),
            );
            sink.shape(
                local,
                &Line::new((-half, half), (half, -half)),
                &Paint::stroke(color, stroke),
            );
        }
    }
}

/// The filled arrowhead, tip at the origin, pointing along +x.
fn arrow_head() -> &'static BezPath {
    static HEAD: OnceLock<BezPath> = OnceLock::new();
    HEAD.get_or_init(|| {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((-ARROW_LEN, ARROW_W / 2.0));
        path.line_to((-ARROW_LEN, -ARROW_W / 2.0));
        path.close_path();
        path
    })
}

/// The same triangle as a chevron: two strokes meeting at the tip, open.
fn open_head() -> &'static BezPath {
    static HEAD: OnceLock<BezPath> = OnceLock::new();
    HEAD.get_or_init(|| {
        let mut path = BezPath::new();
        path.move_to((-ARROW_LEN, ARROW_W / 2.0));
        path.line_to((0.0, 0.0));
        path.line_to((-ARROW_LEN, -ARROW_W / 2.0));
        path
    })
}

/// Where `path` ends and which way it is going there.
fn end_tangent(path: &BezPath) -> Option<(Point, Vec2)> {
    let segment = path.segments().last()?;
    let direction =
        unit(derivative(segment, 1.0)).or_else(|| unit(segment.end() - segment.start()))?;
    Some((segment.end(), direction))
}

/// Where `path` starts and which way a marker there points: back out of
/// the line.
fn start_tangent(path: &BezPath) -> Option<(Point, Vec2)> {
    let segment = path.segments().next()?;
    let direction =
        unit(derivative(segment, 0.0)).or_else(|| unit(segment.end() - segment.start()))?;
    Some((segment.start(), -direction))
}

fn derivative(segment: PathSeg, t: f64) -> Vec2 {
    match segment {
        PathSeg::Line(line) => line.p1 - line.p0,
        PathSeg::Quad(quad) => quad.deriv().eval(t).to_vec2(),
        PathSeg::Cubic(cubic) => cubic.deriv().eval(t).to_vec2(),
    }
}

/// `v` at unit length, or `None` for a vector too short to have a direction
/// — a curve whose last control point sits on its end.
fn unit(v: Vec2) -> Option<Vec2> {
    let length = v.hypot();
    (length > 1e-6).then(|| v / length)
}

/// The distance from a shaped layout's top to its first baseline: what a
/// `LabelLine::baseline` has to be moved up by to place the layout.
fn first_baseline(layout: &Layout<Brush>) -> f32 {
    layout
        .lines()
        .next()
        .map_or(0.0, |line| line.metrics().baseline)
}
