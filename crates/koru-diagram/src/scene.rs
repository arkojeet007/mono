// SPDX-License-Identifier: MPL-2.0

//! The scene: a laid-out diagram with no theme and no font in it.
//!
//! A [`DiagramScene`] is what [`layout`](crate::layout) hands back and what
//! the painter draws: shapes as `kurbo` paths, labels as lines already placed
//! by the host's own measurer, and **roles** instead of colors — the painter
//! maps a [`Role`] to the palette at draw time, so a theme change is a redraw
//! and never a relayout. Coordinates are logical pixels in the layout's own
//! space, with the origin at the scene's top-left corner: `bounds` always
//! starts at `(0, 0)`. The column scales the whole scene down to fit; it is
//! never scaled up.
//!
//! Every type here serialises, so a scene can be compared as JSON in a test
//! and inspected as text. Nothing persists one: a scene is derived from the
//! diagram's text and recomputed whenever the text changes.

use kurbo::{BezPath, Point, Rect};
use serde::{Deserialize, Serialize};

use crate::measure::{TEXT_PX, TextMeasure};
use crate::{Counts, Kind};

/// A laid-out diagram, ready to paint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramScene {
    /// The family of diagram; the same answer `body` gives, for callers that
    /// only need the word.
    pub kind: Kind,
    /// The scene's extent, origin at `(0, 0)`: the natural size of the
    /// drawing in logical pixels.
    pub bounds: Rect,
    /// The counts the screen reader and the inspector say, as `inspect` counted
    /// them before the layout.
    pub counts: Counts,
    /// `accTitle`, when the text declares one.
    pub acc_title: Option<String>,
    /// `accDescr`, when the text declares one.
    pub acc_descr: Option<String>,
    /// The drawing itself, by family.
    pub body: SceneBody,
}

/// The drawing, by family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SceneBody {
    Flowchart(FlowchartScene),
    Sequence(SequenceScene),
    Pie(PieScene),
}

/// A pie chart: slices about a centre, the percent of each on it, a legend
/// beside and a title above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieScene {
    /// `title …`, centred above the pie.
    pub title: Option<Label>,
    pub centre: Point,
    pub radius: f64,
    /// In source order, which is also clockwise order.
    pub slices: Vec<PieSlice>,
    /// One row per slice, down the right of the pie.
    pub legend: Vec<PieLegendItem>,
}

/// One slice: the arc from `start` to `end`, in radians clockwise from
/// twelve o'clock as Mermaid lays it, with the percent label on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieSlice {
    pub label: String,
    pub value: f64,
    /// Mermaid's rounded share of the whole.
    pub percent: u32,
    pub start: f64,
    pub end: f64,
    /// Which of the chart palette's fills this slice and its legend swatch
    /// wear, by index.
    pub series: u8,
    /// The percent, placed on the slice.
    pub text: Label,
}

/// One legend row: a square swatch in the slice's fill, the label after it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PieLegendItem {
    pub swatch: Rect,
    pub label: Label,
    pub series: u8,
}

/// What an item is painted as. The painter maps a role to the palette; a
/// scene never names a color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Label text: node labels, edge labels, messages, notes.
    Text,
    /// Secondary text: group titles.
    TextDim,
    /// Strokes: arrows, lifelines, outlines.
    Line,
    /// The fill of a box.
    Fill,
    /// An emphasised fill: activation bars.
    Accent,
    /// A control frame's border.
    Frame,
    /// A group's or note's background.
    Panel,
    /// One of a chart's categorical fills, by index: a pie's slices and
    /// their legend swatches. The painter cycles its chart palette, so a
    /// scene never names a colour here either.
    Series(u8),
}

/// A piece of text with its lines already placed.
///
/// `text` is the label as a person reads it: `<br>` and entities resolved,
/// lines joined by `'\n'`. `lines` are the same lines placed inside `rect`,
/// each centred horizontally, stacked at the measurer's line height, at
/// [`TEXT_PX`] and weight 400 whatever the diagram's text asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub text: String,
    pub lines: Vec<LabelLine>,
    /// The block the lines occupy: as wide as the widest line, as tall as the
    /// lines stacked.
    pub rect: Rect,
    pub role: Role,
}

/// One placed line of a [`Label`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabelLine {
    pub text: String,
    /// The left edge of the line.
    pub x: f64,
    /// The baseline, from the measurer's ascent.
    pub baseline: f64,
    pub width: f64,
}

/// What an arrow ends in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Marker {
    None,
    Arrow,
    Circle,
    Cross,
    /// An open, unfilled arrowhead: Mermaid's asynchronous sequence message
    /// (`-)`, `--)`). A flowchart never produces it.
    OpenArrow,
}

/// How a line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeKind {
    Normal,
    Dotted,
    Thick,
}

/// The shape of a flowchart box, in Mermaid's own set. Shapes merman knows
/// beyond these (documents, folders, icons, …) are mapped to the nearest one
/// by [`node_shape`](crate::shapes::node_shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeShape {
    /// `[text]`
    Rect,
    /// `(text)`: corners of radius 5.
    Rounded,
    /// `([text])`: a pill.
    Stadium,
    /// `[[text]]`: a rectangle with a bar 8 px inside each vertical edge.
    Subroutine,
    /// `[(text)]`: a cylinder, elliptical caps.
    Cylinder,
    /// `((text))`
    Circle,
    /// `(((text)))`: a circle inside a circle, 5 px apart.
    DoubleCircle,
    /// `{text}`: a rhombus through the edge midpoints of the box.
    Diamond,
    /// `{{text}}`: shoulders a quarter of the height.
    Hexagon,
    /// `[/text/]`: a parallelogram whose top edge is shifted right.
    LeanRight,
    /// `[\text\]`: a parallelogram whose top edge is shifted left.
    LeanLeft,
    /// `[/text\]` (Mermaid's `trapezoid`): the wide side at the bottom, the
    /// shape narrows upward.
    TrapezoidUp,
    /// `[\text/]` (Mermaid's `inv_trapezoid`): the wide side at the top, the
    /// shape narrows downward.
    TrapezoidDown,
    /// `>text]`: a flag, its left edge notched inward.
    Asymmetric,
    /// `@{ shape: text }`: the label alone, no border.
    Text,
}

/// A flowchart: boxes, arrows, groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowchartScene {
    /// The authored boxes, in source order. A group that an arrow points at
    /// is a cluster, not a node.
    pub nodes: Vec<SceneNode>,
    /// The arrows, in source order. An invisible link (`~~~`) lays out but is
    /// not here.
    pub edges: Vec<SceneEdge>,
    /// The groups, outermost first, so that painting them in order draws a
    /// nested group over its parent.
    pub clusters: Vec<SceneCluster>,
}

/// A flowchart box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneNode {
    pub id: String,
    pub shape: NodeShape,
    /// The box merman laid the node out in.
    pub rect: Rect,
    /// The shape's outline inside `rect`, ready to fill then stroke: the
    /// closed silhouette first, then the shape's inner strokes (the
    /// subroutine's bars, the cylinder's rim, the double circle's inner ring)
    /// as further subpaths. Fill with the nonzero rule. Empty for
    /// [`NodeShape::Text`].
    pub outline: BezPath,
    pub label: Label,
}

/// A flowchart arrow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneEdge {
    pub id: String,
    /// The node or group the arrow leaves, by id.
    pub from: String,
    /// The node or group the arrow enters, by id.
    pub to: String,
    /// A basis spline through merman's route, clipped at both ends to the
    /// outline of the node (or the box of the group) it touches, so a marker
    /// drawn at either end sits on the border.
    pub path: BezPath,
    pub start: Marker,
    pub end: Marker,
    pub stroke: StrokeKind,
    pub label: Option<Label>,
}

/// A `subgraph`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneCluster {
    pub id: String,
    pub rect: Rect,
    /// The group's title, placed at the top of `rect`; `None` for a group
    /// titled with nothing.
    pub title: Option<Label>,
    /// Nesting: `0` for an outermost group, one more per enclosing group.
    pub depth: usize,
}

/// A sequence diagram. The builder of this crate fills `participants` and
/// `lifelines` today; the rest is declared so the painter and the editor
/// compile against the whole shape, and is filled by the sequence builder
/// that follows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceScene {
    pub participants: Vec<Participant>,
    pub lifelines: Vec<Lifeline>,
    pub messages: Vec<Message>,
    pub notes: Vec<Note>,
    /// Activation bars.
    pub activations: Vec<Rect>,
    pub frames: Vec<Frame>,
    /// `box … end` participant groups.
    pub boxes: Vec<ParticipantBox>,
    /// `title …`, placed above the participants.
    pub title: Option<Label>,
}

/// How a participant is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantKind {
    /// `participant`: a box with the name in it.
    Box,
    /// `actor`: a stick figure under the name.
    Actor,
}

/// A participant, drawn at the top and again at the bottom of its lifeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub kind: ParticipantKind,
    pub top: Rect,
    pub bottom: Rect,
    /// Placed in `top`; the painter mirrors it into `bottom`.
    pub label: Label,
}

/// A participant's lifeline: a vertical dashed line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lifeline {
    pub x: f64,
    pub y0: f64,
    pub y1: f64,
}

/// A message between participants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub path: BezPath,
    pub start: Marker,
    pub end: Marker,
    pub stroke: StrokeKind,
    pub label: Label,
    /// The `autonumber` badge.
    pub number: Option<Label>,
}

/// A note beside or over participants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub rect: Rect,
    pub label: Label,
}

/// A control frame: `loop`, `alt`, `opt`, `par`, `critical`, `break`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The keyword: `loop`, `alt`, `opt`, `par`, `critical`, `break`.
    pub kind: String,
    pub rect: Rect,
    pub title: Label,
    /// The `else`/`and`/`option` dividers, in order.
    pub sections: Vec<FrameSection>,
}

/// A divider inside a [`Frame`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameSection {
    /// The y of the dashed line.
    pub y: f64,
    pub label: Option<Label>,
}

/// A `box … end` group of participants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParticipantBox {
    pub rect: Rect,
    pub title: Option<Label>,
}

impl Label {
    /// Places `lines` centred on `center`: each line measured at [`TEXT_PX`]
    /// by `measure`, centred horizontally, stacked at the measurer's line
    /// height, the baseline at the measurer's ascent. No lines give an empty
    /// label whose `rect` is the point `center`.
    /// Move the label and every line in it by `by`.
    pub(crate) fn translate(&mut self, by: kurbo::Vec2) {
        self.rect = self.rect + by;
        for line in &mut self.lines {
            line.x += by.x;
            line.baseline += by.y;
        }
    }

    pub(crate) fn placed(
        lines: Vec<String>,
        center: Point,
        measure: &dyn TextMeasure,
        role: Role,
    ) -> Self {
        let metrics: Vec<_> = lines
            .iter()
            .map(|line| measure.measure(line, TEXT_PX))
            .collect();
        let width = metrics
            .iter()
            .map(|m| f64::from(m.width))
            .fold(0.0_f64, f64::max);
        let height: f64 = metrics.iter().map(|m| f64::from(m.height)).sum();
        let left = center.x - width / 2.0;
        let top = center.y - height / 2.0;
        let mut y = top;
        let placed = lines
            .iter()
            .zip(&metrics)
            .map(|(line, m)| {
                let line_width = f64::from(m.width);
                let placed = LabelLine {
                    text: line.clone(),
                    x: center.x - line_width / 2.0,
                    baseline: y + f64::from(m.ascent),
                    width: line_width,
                };
                y += f64::from(m.height);
                placed
            })
            .collect();
        Self {
            text: lines.join("\n"),
            lines: placed,
            rect: Rect::new(left, top, left + width, top + height),
            role,
        }
    }
}
