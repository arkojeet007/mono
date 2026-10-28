// SPDX-License-Identifier: MPL-2.0

//! Drawing a diagram scene: the measurer it is laid out with, the shaping
//! done once ahead of painting, the vello painter, and the SVG writer.
//!
//! `koru-diagram` lays a Mermaid text out as a [`DiagramScene`]: shapes as
//! paths, labels as placed lines, and **roles** in place of colors. This
//! module is the other half.
//!
//! - [`InterMeasure`] is the measurer layout asks for every width — the
//!   bundled Inter face at [`TEXT_PX`] and weight 400 whatever font the
//!   person reads the page in, so a diagram has the same line breaks and
//!   proportions on every machine, which is what lets its PNG and SVG match
//!   the screen.
//! - [`PreparedDiagram`] shapes every placed line once, with the stack, size
//!   and weight the measurer used, so [`paint`] — called every frame for
//!   every visible diagram — shapes nothing and allocates nothing beyond
//!   vello's own encoding.
//! - [`paint`] maps each role to the palette *at draw time*, so a theme flip
//!   is a redraw and never a relayout, and divides every stroke width and
//!   marker by the column's scale so a drawing scaled down to fit keeps its
//!   line weight on screen.
//! - [`svg`] writes the same geometry as a file that needs no font: every
//!   glyph is an outline path.
//!
//! The painter and the writer are one walk over the scene ([`walk`]) feeding
//! two sinks, so they cannot disagree about a shape. Everything a person
//! could want to tune is a named constant below.

mod measure;
mod svg;
mod walk;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use koru_diagram::{Counts, DiagramScene, Role, SceneBody};
use kurbo::{Affine, Rect, Size};
use parley::Layout;
use peniko::Color;
use vello::Scene;

use crate::inspect;
use crate::theme::{Brush, Palette};

pub use measure::{InterMeasure, STACK, WEIGHT};

// ------------------------------------------------------------ the defaults
//
// Logical pixels at scale 1; the painter divides each by the column scale.

/// An arrow or message drawn `StrokeKind::Normal`.
pub const EDGE_W: f64 = 1.5;
/// An arrow drawn `StrokeKind::Thick` (`==>`).
pub const EDGE_THICK_W: f64 = 3.0;
/// The dash pattern of a `StrokeKind::Dotted` arrow or message, at [`EDGE_W`].
pub const EDGE_DASH: [f64; 2] = [3.0, 3.0];
/// The space kept clear of the line around an arrow's label, which is filled
/// with the page color so the line does not run through the words.
pub const EDGE_LABEL_PAD: f64 = 2.0;
/// A box's outline, over its `panel_bg` fill.
pub const NODE_OUTLINE_W: f64 = 1.0;
/// A `subgraph`'s outline; no fill.
pub const CLUSTER_OUTLINE_W: f64 = 1.0;
/// The corner radius of a `subgraph`'s outline.
pub const CLUSTER_RADIUS: f64 = 4.0;
/// A filled arrowhead: this long along the line, [`ARROW_W`] across.
pub const ARROW_LEN: f64 = 10.0;
pub const ARROW_W: f64 = 8.0;
/// A `--o` circle marker.
pub const CIRCLE_R: f64 = 4.0;
/// A `--x` cross marker, corner to corner.
pub const CROSS_SIZE: f64 = 8.0;
/// A sequence participant's box: like a `Rounded` node.
pub const PARTICIPANT_RADIUS: f64 = 5.0;
/// The stick figure of an `actor`: the head's radius, the half-span of the
/// arms and of the legs, its line width, the gap it keeps above its label and
/// the least height it draws at.
pub const ACTOR_HEAD_R: f64 = 5.0;
pub const ACTOR_ARM: f64 = 9.0;
pub const ACTOR_LEG: f64 = 7.0;
pub const ACTOR_W: f64 = 1.5;
pub const ACTOR_GAP: f64 = 2.0;
pub const ACTOR_MIN_H: f64 = 24.0;
/// A lifeline: dashed, in `text_dim`.
pub const LIFELINE_W: f64 = 1.0;
pub const LIFELINE_DASH: [f64; 2] = [5.0, 5.0];
/// The `autonumber` badge: a filled `accent` circle with the number on it,
/// stretched into a capsule of the same height when the number is wider
/// than the circle leaves room for (`10` and up).
pub const NUMBER_R: f64 = 8.0;
pub const NUMBER_PAD_X: f64 = 3.0;
/// A note: `panel_bg` fill, `panel_border` outline.
pub const NOTE_OUTLINE_W: f64 = 1.0;
/// An activation bar: `accent` at this alpha, outlined in `accent`.
pub const ACTIVATION_ALPHA: f32 = 0.25;
pub const ACTIVATION_OUTLINE_W: f64 = 1.0;
/// A control frame (`loop`, `alt`, …): outline, dashed dividers, and the
/// padding of the tag box its keyword sits in.
pub const FRAME_OUTLINE_W: f64 = 1.0;
pub const FRAME_DASH: [f64; 2] = [3.0, 3.0];
pub const FRAME_TAG_PAD_X: f64 = 6.0;
pub const FRAME_TAG_PAD_Y: f64 = 2.0;
/// A `box … end` participant group's outline; no fill.
pub const BOX_OUTLINE_W: f64 = 1.0;

/// The palette color a role is drawn in. `Fill` and `Panel` are a box's
/// background; the outline around either is `panel_border`, which the
/// painter adds itself.
pub fn ink(palette: &Palette, role: Role) -> Color {
    match role {
        Role::Text => palette.text,
        Role::TextDim => palette.text_dim,
        Role::Line => palette.text,
        Role::Fill | Role::Panel => palette.panel_bg,
        Role::Accent => palette.accent,
        Role::Frame => palette.panel_border,
        Role::Series(index) => palette.chart[index as usize % palette.chart.len()],
    }
}

/// A pie slice's outline, in the page colour, so neighbouring slices are
/// parted the way Mermaid parts them; and a legend swatch's.
pub const SLICE_OUTLINE_W: f64 = 2.0;
pub const SWATCH_OUTLINE_W: f64 = 1.0;

/// A scene with every line of text shaped, ready to paint on any thread
/// without shaping again.
///
/// Shaping is the one expensive step between a scene and pixels, and the
/// painter runs every frame, so it happens here, once, on whichever thread
/// builds the entry — the host's worker, not the loop. The layouts are plain
/// data: the host's cache holds one of these in an `Arc` and every window
/// paints from it. Colors are not in it: a label is recolored at draw time,
/// so a theme flip never rebuilds one.
pub struct PreparedDiagram {
    scene: Arc<DiagramScene>,
    /// One layout per `LabelLine`, in [`walk::each_line`] order.
    lines: Vec<Layout<Brush>>,
    /// One layout per sequence [`Frame`](koru_diagram::Frame): its keyword.
    frame_tags: Vec<Layout<Brush>>,
}

// The host cache shares a prepared diagram across threads; parley layouts
// carry only vectors and `Arc`-backed font data, and this says so at compile
// time rather than at the first `Arc<PreparedDiagram>` someone writes.
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<PreparedDiagram>();
};

impl PreparedDiagram {
    /// Shapes every line of `scene` — the labels and the frame keywords —
    /// with [`InterMeasure`]'s stack, size and weight.
    pub fn new(scene: Arc<DiagramScene>) -> Self {
        let mut lines = Vec::new();
        let mut frame_tags = Vec::new();
        measure::with_engine(|engine| {
            walk::each_line(&scene, |placed| {
                lines.push(measure::shape(engine, &placed.line.text, measure::px()));
            });
            if let SceneBody::Sequence(sequence) = &scene.body {
                for frame in &sequence.frames {
                    frame_tags.push(measure::shape(engine, &frame.kind, measure::px()));
                }
            }
        });
        Self {
            scene,
            lines,
            frame_tags,
        }
    }

    pub fn scene(&self) -> &Arc<DiagramScene> {
        &self.scene
    }

    /// The drawing's own size, in logical pixels: the scene's bounds.
    pub fn natural_size(&self) -> Size {
        self.scene.bounds.size()
    }

    /// How many layouts were shaped for the labels.
    #[cfg(test)]
    pub(crate) fn shaped_lines(&self) -> usize {
        self.lines.len()
    }
}

/// Paints `prepared` into `out`.
///
/// `transform` is everything above the drawing: the page's root transform,
/// the block's translation and the column scale. `scale` is that column
/// scale on its own (at most 1: a drawing is scaled down to fit, never up),
/// and every stroke width, dash and marker is divided by it so the lines are
/// as thick on screen as they would be at natural size — the same rule the
/// canvas uses for its hairlines. A scale that is not a positive number is
/// painted as 1.
///
/// Roles become colors here and nowhere else, per the table in [`ink`]: a
/// cached layout is recolored as it is drawn, so one prepared diagram serves
/// both themes. Shapes nothing, allocates nothing of its own.
pub fn paint(
    prepared: &PreparedDiagram,
    palette: &Palette,
    transform: Affine,
    scale: f64,
    out: &mut Scene,
) {
    let size = prepared.natural_size();
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let _scope = inspect::scope(
        "Diagram",
        "part",
        transform,
        Rect::from_origin_size((0.0, 0.0), size),
        |record| {
            record.property(
                "natural size",
                format!("{:.1} × {:.1}", size.width, size.height),
                "DiagramScene::bounds; logical units at scale 1",
            );
            record.property(
                "scale",
                format!("{scale:.3}"),
                "the column's scale; every stroke is divided by it",
            );
            record.property(
                "counts",
                counts_text(&prepared.scene.counts),
                "koru_diagram::Counts, as the screen reader says them",
            );
        },
    );
    walk::walk(prepared, palette, transform, scale, &mut walk::Vello(out));
}

/// `prepared` as an SVG document at its natural size, in `palette`'s colors.
///
/// `<svg xmlns viewBox="0 0 W H" width="W" height="H">`, a background rect
/// in `palette.bg`, then one `<path>` per shape and **one per glyph**: the
/// labels are the outlines of the very glyphs the screen shows, taken from
/// the font through skrifa, so the file looks the same on a machine without
/// Koru's font. No `<text>`, no `font-family`, no `@font-face`, no `<image>`.
/// Inter declares no Reserved Font Name, so outlining it is within its
/// licence. Numbers are written to three decimals, so the same scene gives
/// the same bytes.
pub fn svg(prepared: &PreparedDiagram, palette: &Palette) -> String {
    svg_counted(prepared, palette).0
}

/// [`svg`], with the number of shape and glyph paths it wrote — the two
/// numbers the `<path` count of the document is made of.
pub(crate) fn svg_counted(prepared: &PreparedDiagram, palette: &Palette) -> (String, svg::Counts) {
    let mut sink = svg::Svg::new(prepared.natural_size(), palette.bg);
    walk::walk(prepared, palette, Affine::IDENTITY, 1.0, &mut sink);
    sink.finish()
}

fn counts_text(counts: &Counts) -> String {
    match counts {
        Counts::Flowchart {
            nodes,
            edges,
            groups,
        } => format!("{nodes} nodes, {edges} edges, {groups} groups"),
        Counts::Sequence {
            participants,
            messages,
            notes,
        } => format!("{participants} participants, {messages} messages, {notes} notes"),
        Counts::Pie { slices } => format!("{slices} slices"),
    }
}
