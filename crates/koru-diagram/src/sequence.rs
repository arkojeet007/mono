// SPDX-License-Identifier: MPL-2.0

//! The sequence scene builder: merman's typed layout — with the patch's
//! frames, activations, boxes and title — and the semantic model into a
//! [`SequenceScene`].
//!
//! The layout gives the participants' boxes (`actor-top-*` and
//! `actor-bottom-*` nodes), the lifelines (`lifeline-*` edges), each
//! message's line and label box (`msg-*` edges) and each note's box (`note-*`
//! nodes); the patch adds the control frames with their dividers, the
//! activation bars, the `box` groups and the title; the semantic model gives
//! the markers and the stroke of every message, the autonumbering, the kind
//! of every participant and the text of everything. Koru draws what merman
//! laid out: no coordinate here is invented, and the few drawing rules the
//! layout leaves to merman's SVG renderer — where a self-message loops, where
//! a number sits, where a frame's title and a divider's label go, where a
//! line of text falls relative to its anchor — are reproduced from that
//! renderer with its constants, so the scene is merman's own picture of the
//! text. The tests hold it to that picture.
//!
//! **Text anchors.** Mermaid's SVG places text in four ways, and each label
//! here is anchored the same way at Koru's own line height: a message or note
//! line is drawn `1em` below its anchor with the anchor at the line's middle
//! (Mermaid's `dy="1em"` on a `dominant-baseline: middle` text); a frame's
//! title or a divider's label has its first baseline at the anchor; a
//! participant's name, a `box` title and a message number are centred on the
//! anchor; the diagram title starts at its anchor with its baseline there.
//! Further lines stack at the measurer's line height from the first.
//!
//! **Wrapping.** A label marked `wrap:` is broken by merman's port of
//! Mermaid's `wrapLabel`, with Koru's widths, at the width merman used: a
//! message at its line's length plus the padding (at least the default
//! message width), a note at its box less the padding, a participant's name
//! at the default width less the padding. A frame's title wraps at the width
//! Mermaid's `calculateLoopBounds` gives the block, which the layout JSON
//! does not carry; it is replayed here from the laid-out messages and notes
//! the frame encloses, the way merman's renderer replays it.

use std::collections::HashMap;

use kurbo::{BezPath, Point, Rect, Vec2};
use merman::diagrams::sequence::{
    SequenceControlKind, SequenceControlRole, SequenceDiagramRenderModel, SequenceMessage,
    SequenceMessageDirection, SequenceMessageKind, SequenceMessageMarker, SequenceMessagePayload,
    SequenceMessageStroke, SequenceSignalSemantics,
};
use merman::entities::decode_mermaid_entities_to_unicode;
use merman_render::model::{LayoutEdge, LayoutNode, SequenceDiagramLayout, SequenceTextLayout};
use merman_render::text::{
    TextMeasurer, TextMetrics as MermanMetrics, TextStyle, split_html_br_lines,
    wrap_label_like_mermaid_lines,
};

use crate::layout::{Fail, SCENE_CODE, internal};
use crate::measure::{TEXT_PX, TextMeasure};
use crate::scene::{
    DiagramScene, Frame, FrameSection, Label, Lifeline, Marker, Message, Note, Participant,
    ParticipantBox, ParticipantKind, Role, SceneBody, SequenceScene, StrokeKind,
};
use crate::{Counts, Kind};

// Mermaid's `sequence` config as merman's pinned default has it. Koru's site
// config locks the section (`engine::SECURE_KEYS`) and sets none of it, so
// these hold for every diagram.
/// `boxMargin`: the space above a frame's title and around its content.
const BOX_MARGIN: f64 = 10.0;
/// `boxTextMargin`: the margin of a frame's title and a divider's label.
const BOX_TEXT_MARGIN: f64 = 5.0;
/// `noteMargin`: the margin inside a note.
const NOTE_MARGIN: f64 = 10.0;
/// `wrapPadding`: the padding around a message's text.
const WRAP_PADDING: f64 = 10.0;
/// `labelBoxWidth`: the width of the tag at a frame's top-left corner, which
/// the painter draws from [`Frame::kind`]; the tag is `labelBoxHeight` (20)
/// tall.
const LABEL_BOX_WIDTH: f64 = 50.0;
/// `activationWidth`: the width of an activation bar.
const ACTIVATION_WIDTH: f64 = 10.0;
/// `height`: a participant's box, and the figure of an `actor`.
const ACTOR_HEIGHT: f64 = 65.0;
/// `width`: the least width a message is given, and a participant's box.
const MESSAGE_WIDTH_MIN: f64 = 150.0;
/// Mermaid's font size — `fontSize` and `messageFontSize` alike — which is
/// `1em` in its SVG: a message or note line is drawn this far below its
/// anchor.
const MERMAID_EM: f64 = 16.0;
/// Mermaid centres an actor's name `height / 2 + 35` below the figure's top.
const ACTOR_NAME_OFFSET: f64 = 35.0;
/// A self-message loops this far right of its lifeline.
const SELF_MESSAGE_REACH: f64 = 60.0;

/// Builds the scene from the patched layout and the semantic model.
pub(crate) fn build(
    model: &SequenceDiagramRenderModel,
    layout: SequenceDiagramLayout,
    counts: Counts,
    measure: &dyn TextMeasure,
) -> Result<DiagramScene, Fail> {
    let (origin, bounds) = match &layout.bounds {
        Some(b) => (
            Vec2::new(b.min_x, b.min_y),
            Rect::new(0.0, 0.0, b.max_x - b.min_x, b.max_y - b.min_y),
        ),
        None => (Vec2::ZERO, Rect::ZERO),
    };
    let ctx = Ctx {
        origin,
        nodes: layout.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
        edges: layout.edges.iter().map(|e| (e.id.as_str(), e)).collect(),
        measure,
    };
    let (participants, lifelines) = ctx.participants(model)?;
    let Content {
        messages,
        notes,
        block_widths,
    } = ctx.content(model)?;
    let frames = ctx.frames(&layout, &block_widths);
    let activations = layout
        .activations
        .iter()
        .map(|bar| Rect::from_origin_size(ctx.shift(bar.x, bar.y), (bar.width, bar.height)))
        .collect();
    let boxes = layout
        .boxes
        .iter()
        .map(|group| ParticipantBox {
            rect: Rect::from_origin_size(ctx.shift(group.x, group.y), (group.width, group.height)),
            title: group.title.as_ref().map(|title| {
                ctx.label_centered(
                    display(owned(split_html_br_lines(&title.text))),
                    Point::new(title.x, title.y),
                    Role::TextDim,
                )
            }),
        })
        .collect();
    let title = layout.title.as_ref().map(|title| ctx.title(title));

    Ok(DiagramScene {
        kind: Kind::Sequence,
        bounds,
        counts,
        acc_title: model.acc_title.clone(),
        acc_descr: model.acc_descr.clone(),
        body: SceneBody::Sequence(SequenceScene {
            participants,
            lifelines,
            messages,
            notes,
            activations,
            frames,
            boxes,
            title,
        }),
    })
}

/// The layout by id, the origin to shift it by, and the measurer.
struct Ctx<'a> {
    origin: Vec2,
    nodes: HashMap<&'a str, &'a LayoutNode>,
    edges: HashMap<&'a str, &'a LayoutEdge>,
    measure: &'a dyn TextMeasure,
}

/// The messages and notes in source order, and the width Mermaid gives each
/// control block, by the id of its opening message and of every labelled
/// divider.
struct Content {
    messages: Vec<Message>,
    notes: Vec<Note>,
    block_widths: HashMap<String, f64>,
}

/// The widths and vertical metrics of a label's lines stacked.
struct Stack {
    width: f64,
    height: f64,
    first_height: f64,
    first_ascent: f64,
}

impl<'a> Ctx<'a> {
    fn shift(&self, x: f64, y: f64) -> Point {
        Point::new(x, y) - self.origin
    }

    fn node(&self, id: &str) -> Result<&'a LayoutNode, Fail> {
        self.nodes
            .get(id)
            .copied()
            .ok_or_else(|| internal(SCENE_CODE, format!("{id:?} was not laid out")))
    }

    fn rect(&self, node: &LayoutNode) -> Rect {
        Rect::from_center_size(self.shift(node.x, node.y), (node.width, node.height))
    }

    fn stack(&self, lines: &[String]) -> Stack {
        let mut stack = Stack {
            width: 0.0,
            height: 0.0,
            first_height: 0.0,
            first_ascent: 0.0,
        };
        for (index, line) in lines.iter().enumerate() {
            let metrics = self.measure.measure(line, TEXT_PX);
            stack.width = stack.width.max(f64::from(metrics.width));
            stack.height += f64::from(metrics.height);
            if index == 0 {
                stack.first_height = f64::from(metrics.height);
                stack.first_ascent = f64::from(metrics.ascent);
            }
        }
        stack
    }

    /// `text` broken as merman's port of Mermaid's `wrapLabel` breaks it at
    /// `max_width`, measured by Koru's measurer the way merman asked it.
    fn wrapped(&self, text: &str, max_width: f64) -> Vec<String> {
        wrap_label_like_mermaid_lines(
            text,
            &WrapMeasure(self.measure),
            &TextStyle::default(),
            max_width,
        )
    }

    /// A label centred on `anchor` (in merman's space).
    fn label_centered(&self, lines: Vec<String>, anchor: Point, role: Role) -> Label {
        Label::placed(lines, self.shift(anchor.x, anchor.y), self.measure, role)
    }

    /// A label whose first line's middle is `1em` below `anchor`: Mermaid's
    /// text drawn `dy="1em"` with `dominant-baseline: middle`.
    fn label_below(&self, lines: Vec<String>, anchor: Point, role: Role) -> Label {
        let stack = self.stack(&lines);
        let top = anchor.y + MERMAID_EM - stack.first_height / 2.0;
        Label::placed(
            lines,
            self.shift(anchor.x, top + stack.height / 2.0),
            self.measure,
            role,
        )
    }

    /// A label whose first baseline is at `anchor`, centred on its x.
    fn label_on_baseline(&self, lines: Vec<String>, anchor: Point, role: Role) -> Label {
        let stack = self.stack(&lines);
        let top = anchor.y - stack.first_ascent;
        Label::placed(
            lines,
            self.shift(anchor.x, top + stack.height / 2.0),
            self.measure,
            role,
        )
    }

    /// A label starting at `anchor.x` with its first baseline at `anchor.y`.
    fn label_from_left(&self, lines: Vec<String>, anchor: Point, role: Role) -> Label {
        let stack = self.stack(&lines);
        let top = anchor.y - stack.first_ascent;
        Label::placed(
            lines,
            self.shift(anchor.x + stack.width / 2.0, top + stack.height / 2.0),
            self.measure,
            role,
        )
    }

    /// The participants in `actor_order`, each with its lifeline.
    fn participants(
        &self,
        model: &SequenceDiagramRenderModel,
    ) -> Result<(Vec<Participant>, Vec<Lifeline>), Fail> {
        let mut participants = Vec::with_capacity(model.actor_order.len());
        let mut lifelines = Vec::with_capacity(model.actor_order.len());
        for id in &model.actor_order {
            let actor = model
                .actors
                .get(id)
                .ok_or_else(|| internal(SCENE_CODE, format!("participant {id:?} has no actor")))?;
            let top = self.node(&format!("actor-top-{id}"))?;
            let bottom = self.node(&format!("actor-bottom-{id}"))?;
            let kind = if actor.actor_type == "actor" {
                ParticipantKind::Actor
            } else {
                ParticipantKind::Box
            };
            let lines = if actor.wrap {
                self.wrapped(&actor.description, MESSAGE_WIDTH_MIN - 2.0 * WRAP_PADDING)
            } else {
                owned(split_html_br_lines(&actor.description))
            };
            // Mermaid names a box in its centre and an actor under its
            // figure, over the gap before the lifeline starts.
            let anchor = match kind {
                ParticipantKind::Box => Point::new(top.x, top.y),
                ParticipantKind::Actor => Point::new(
                    top.x,
                    top.y - top.height / 2.0 + ACTOR_HEIGHT / 2.0 + ACTOR_NAME_OFFSET,
                ),
            };
            participants.push(Participant {
                id: id.clone(),
                kind,
                top: self.rect(top),
                bottom: self.rect(bottom),
                label: self.label_centered(display(lines), anchor, Role::Text),
            });
            if let Some(edge) = self.edges.get(format!("lifeline-{id}").as_str())
                && let [first, .., last] = edge.points.as_slice()
            {
                let (first, last) = (self.shift(first.x, first.y), self.shift(last.x, last.y));
                lifelines.push(Lifeline {
                    x: first.x,
                    y0: first.y,
                    y1: last.y,
                });
            }
        }
        Ok((participants, lifelines))
    }

    /// Mermaid's message list replayed in order: the autonumber directives,
    /// the activations (for where a number sits), the control statements
    /// (for the frames' widths), the notes and the messages.
    fn content(&self, model: &SequenceDiagramRenderModel) -> Result<Content, Fail> {
        let centers: HashMap<&str, Center> = model
            .actor_order
            .iter()
            .filter_map(|id| {
                let node = self.nodes.get(format!("actor-top-{id}").as_str())?;
                Some((
                    id.as_str(),
                    Center {
                        x: node.x,
                        width: node.width,
                    },
                ))
            })
            .collect();
        let mut numbering = Numbering::default();
        let mut depths: HashMap<&str, usize> = HashMap::new();
        let mut blocks = Blocks::default();
        let mut messages = Vec::new();
        let mut notes = Vec::new();
        for msg in &model.messages {
            match msg.semantic_kind() {
                SequenceMessageKind::Autonumber => numbering.directive(msg),
                SequenceMessageKind::ActivationStart => {
                    if let Some(from) = msg.from.as_deref()
                        && centers.contains_key(from)
                    {
                        *depths.entry(from).or_default() += 1;
                    }
                }
                SequenceMessageKind::ActivationEnd => {
                    if let Some(from) = msg.from.as_deref()
                        && let Some(depth) = depths.get_mut(from)
                    {
                        *depth = depth.saturating_sub(1);
                    }
                }
                SequenceMessageKind::Control => blocks.control(msg),
                SequenceMessageKind::Note => {
                    let (note, left, right) = self.note(msg)?;
                    blocks.include_span(left, right);
                    notes.push(note);
                }
                SequenceMessageKind::Signal => {
                    // Mermaid advances the number for every message, shown or not.
                    let number = numbering.take();
                    messages.push(self.message(msg, number, &depths, &centers, &mut blocks)?);
                }
                SequenceMessageKind::CentralDecorationRecord | SequenceMessageKind::Unknown => {}
            }
        }
        Ok(Content {
            messages,
            notes,
            block_widths: blocks.closed,
        })
    }

    fn message(
        &self,
        msg: &SequenceMessage,
        number: Option<String>,
        depths: &HashMap<&str, usize>,
        centers: &HashMap<&str, Center>,
        blocks: &mut Blocks,
    ) -> Result<Message, Fail> {
        let semantics = msg
            .signal_semantics()
            .ok_or_else(|| internal(SCENE_CODE, format!("message {:?} is no signal", msg.id)))?;
        let (Some(from), Some(to)) = (msg.from.as_deref(), msg.to.as_deref()) else {
            return Err(internal(
                SCENE_CODE,
                format!("message {:?} names no participants", msg.id),
            ));
        };
        let edge_id = format!("msg-{}", msg.id);
        let edge = self
            .edges
            .get(edge_id.as_str())
            .ok_or_else(|| internal(SCENE_CODE, format!("{edge_id:?} was not laid out")))?;
        let [p0, p1, ..] = edge.points.as_slice() else {
            return Err(internal(SCENE_CODE, format!("{edge_id:?} has no line")));
        };
        let laid_label = edge
            .label
            .as_ref()
            .ok_or_else(|| internal(SCENE_CODE, format!("{edge_id:?} has no label box")))?;
        let (x0, x1, y) = (p0.x, p1.x, p0.y);
        let is_self = from == to;
        let text = msg.message_text();
        let bounded = (x0 - x1).abs();

        // The line as merman draws it: straight, or for a self-message the
        // cubic that leaves the lifeline rightward and returns 20 px lower.
        let mut path = BezPath::new();
        path.move_to(self.shift(x0, y));
        if is_self {
            path.curve_to(
                self.shift(x0 + SELF_MESSAGE_REACH, y - 10.0),
                self.shift(x0 + SELF_MESSAGE_REACH, y + 30.0),
                self.shift(x0, y + 20.0),
            );
        } else {
            path.line_to(self.shift(x1, p1.y));
        }

        let lines = if msg.wrap && !text.is_empty() {
            self.wrapped(text, (bounded + 2.0 * WRAP_PADDING).max(MESSAGE_WIDTH_MIN))
        } else {
            owned(split_html_br_lines(text))
        };
        let label = self.label_below(
            display(lines),
            Point::new(laid_label.x.round(), js_round(laid_label.y)),
            Role::Text,
        );

        // Mermaid's `calculateLoopBounds`, for the frames' title widths.
        let text_part = if msg.wrap {
            0.0
        } else {
            mermaid_text_width(text, self.measure) + 2.0 * WRAP_PADDING
        };
        let width = text_part
            .max(bounded + 2.0 * WRAP_PADDING)
            .max(MESSAGE_WIDTH_MIN);
        if is_self {
            if let Some(center) = centers.get(from) {
                let left = center.x - center.width / 2.0;
                blocks.include_span(
                    (left - width / 2.0).min(left - center.width / 2.0),
                    (left + width / 2.0).max(left + center.width / 2.0),
                );
            }
        } else {
            blocks.include_message(x0, x1, width);
        }

        let number = number.map(|text| {
            let x = match (centers.get(from), centers.get(to)) {
                (Some(from_center), Some(to_center)) => number_x(
                    &semantics,
                    stack_bounds(depths.get(from).copied().unwrap_or(0), from_center.x),
                    stack_bounds(depths.get(to).copied().unwrap_or(0), to_center.x),
                    x0,
                    x1,
                ),
                _ => x0,
            };
            Label::placed(vec![text], self.shift(x, y), self.measure, Role::Accent)
        });

        Ok(Message {
            id: msg.id.clone(),
            from: from.to_owned(),
            to: to.to_owned(),
            path,
            start: marker(semantics.source_marker),
            end: marker(semantics.target_marker),
            stroke: match semantics.stroke {
                SequenceMessageStroke::Solid => StrokeKind::Normal,
                SequenceMessageStroke::Dotted => StrokeKind::Dotted,
            },
            label,
            number,
        })
    }

    /// A note, and the left and right of its box in merman's space.
    fn note(&self, msg: &SequenceMessage) -> Result<(Note, f64, f64), Fail> {
        let node = self.node(&format!("note-{}", msg.id))?;
        let text = msg.message_text();
        let lines = if msg.wrap {
            self.wrapped(text, (node.width - 2.0 * WRAP_PADDING).max(1.0))
        } else {
            owned(split_html_br_lines(text))
        };
        let left = node.x - node.width / 2.0;
        let top = node.y - node.height / 2.0;
        let label = self.label_below(
            display(lines),
            Point::new(node.x, js_round(drawn_text_first_y(top, NOTE_MARGIN))),
            Role::Text,
        );
        Ok((
            Note {
                rect: self.rect(node),
                label,
            },
            left,
            left + node.width,
        ))
    }

    /// The patch's frames, with their titles and divider labels where
    /// Mermaid's `drawLoop` puts them: the title centred between the tag and
    /// the right edge, a divider's label centred on the frame, both with
    /// their first baseline `boxMargin + boxTextMargin` plus half the text
    /// margin below the frame's top or the divider.
    fn frames(&self, layout: &SequenceDiagramLayout, widths: &HashMap<String, f64>) -> Vec<Frame> {
        layout
            .frames
            .iter()
            .map(|frame| {
                let (x1, y1) = (frame.x, frame.y);
                let (x2, y2) = (x1 + frame.width, y1 + frame.height);
                let wrap_width = |id: &str, fallback: Option<f64>| {
                    widths
                        .get(id)
                        .map(|width| (width - 2.0 * WRAP_PADDING).max(1.0))
                        .or(fallback)
                };
                let title_lines = block_label(&frame.label)
                    .map(|label| {
                        self.block_lines(
                            &label,
                            wrap_width(&frame.id, Some((x2 - x1 - LABEL_BOX_WIDTH).max(0.0))),
                        )
                    })
                    .unwrap_or_default();
                let title = self.label_on_baseline(
                    title_lines,
                    Point::new((x1 + LABEL_BOX_WIDTH + x2) / 2.0, block_text_baseline(y1)),
                    Role::TextDim,
                );
                let sections = frame
                    .sections
                    .iter()
                    .map(|section| FrameSection {
                        y: self.shift(0.0, section.y).y,
                        label: block_label(&section.label).map(|label| {
                            self.label_on_baseline(
                                self.block_lines(&label, wrap_width(&section.id, None)),
                                Point::new((x1 + x2) / 2.0, block_text_baseline(section.y)),
                                Role::TextDim,
                            )
                        }),
                    })
                    .collect();
                Frame {
                    kind: frame.kind.clone(),
                    rect: Rect::from_points(self.shift(x1, y1), self.shift(x2, y2)),
                    title,
                    sections,
                }
            })
            .collect()
    }

    /// A frame's or divider's label (already bracketed and decoded), wrapped
    /// at `max_width` when there is one, else split at its `<br>`s.
    fn block_lines(&self, label: &str, max_width: Option<f64>) -> Vec<String> {
        match max_width {
            Some(width) => self.wrapped(label, width),
            None => owned(split_html_br_lines(label)),
        }
    }

    /// The diagram title: one line starting at the patch's `x` with its
    /// baseline at `y`, as Mermaid's bare `<text>` is drawn.
    fn title(&self, title: &SequenceTextLayout) -> Label {
        self.label_from_left(
            vec![decode_mermaid_entities_to_unicode(&title.text).into_owned()],
            Point::new(title.x, title.y),
            Role::Text,
        )
    }
}

/// A participant's lifeline x and box width, in merman's space.
struct Center {
    x: f64,
    width: f64,
}

/// Mermaid's `autonumber` state: whether numbers show, the next number and
/// the step.
struct Numbering {
    visible: bool,
    next: f64,
    step: f64,
}

impl Default for Numbering {
    fn default() -> Self {
        Self {
            visible: false,
            next: 1.0,
            step: 1.0,
        }
    }
}

impl Numbering {
    fn directive(&mut self, msg: &SequenceMessage) {
        if let SequenceMessagePayload::Autonumber(autonumber) = &msg.message {
            self.visible = autonumber.visible;
            if let Some(start) = autonumber.start {
                self.next = start;
            }
            if let Some(step) = autonumber.step {
                self.step = step;
            }
        }
    }

    /// The number of the next message, shown as Mermaid formats it, and
    /// `None` when numbers are hidden; the count advances either way.
    fn take(&mut self) -> Option<String> {
        let current = self.next;
        self.next = ((current + self.step) * 100.0).round() / 100.0;
        if self.visible && current.is_finite() {
            Some(current.to_string())
        } else {
            None
        }
    }
}

/// Mermaid's `calculateLoopBounds`: the width each control block gives its
/// title, grown by every message and note inside it. A labelled divider
/// shares its block's width.
#[derive(Default)]
struct Blocks {
    open: Vec<OpenBlock>,
    closed: HashMap<String, f64>,
}

struct OpenBlock {
    ids: Vec<String>,
    from: f64,
    to: f64,
    width: f64,
}

impl Blocks {
    fn control(&mut self, msg: &SequenceMessage) {
        let Some(control) = msg.control_semantics() else {
            return;
        };
        if control.kind == SequenceControlKind::Rect {
            return;
        }
        match control.role {
            SequenceControlRole::Start => self.open.push(OpenBlock {
                ids: vec![msg.id.clone()],
                from: f64::INFINITY,
                to: f64::NEG_INFINITY,
                width: 0.0,
            }),
            SequenceControlRole::Separator => {
                if !msg.message_text().is_empty()
                    && let Some(block) = self.open.last_mut()
                {
                    block.ids.push(msg.id.clone());
                }
            }
            SequenceControlRole::End => {
                if let Some(block) = self.open.pop() {
                    for id in block.ids {
                        self.closed.insert(id, block.width);
                    }
                }
            }
        }
    }

    /// A note's box, or a self-message's reach, inside every open block.
    fn include_span(&mut self, left: f64, right: f64) {
        for block in &mut self.open {
            block.from = block.from.min(left);
            block.to = block.to.max(right);
            block.width = block.width.max((block.from - block.to).abs()) - LABEL_BOX_WIDTH;
        }
    }

    /// A message's line and its own width inside every open block.
    fn include_message(&mut self, start_x: f64, stop_x: f64, width: f64) {
        for block in &mut self.open {
            block.from = block.from.min(start_x);
            block.to = block.to.max(stop_x);
            block.width = block.width.max(width) - LABEL_BOX_WIDTH;
        }
    }
}

/// Koru's measurer as merman's `TextMeasurer`, for merman's own `wrapLabel`
/// port: the width of a wrap candidate is Mermaid's `calculateTextWidth` —
/// the widest `<br>` line, rounded — at [`TEXT_PX`], the answer the layout
/// got from the host for the same operation.
struct WrapMeasure<'a>(&'a dyn TextMeasure);

impl TextMeasurer for WrapMeasure<'_> {
    fn measure(&self, text: &str, _style: &TextStyle) -> MermanMetrics {
        let metrics = self.0.measure(text, TEXT_PX);
        MermanMetrics {
            width: finite(metrics.width),
            height: finite(metrics.height),
            line_count: 1,
        }
    }

    fn measure_svg_simple_text_bbox_width_for_wrap_px(
        &self,
        text: &str,
        _style: &TextStyle,
    ) -> f64 {
        mermaid_text_width(text, self.0)
    }
}

/// Mermaid's `calculateTextDimensions(text).width` as merman asks the host
/// for it: each `<br>` line measured on its own (an empty one as a zero-width
/// space) and rounded, the widest kept. Nothing for an empty text.
fn mermaid_text_width(text: &str, measure: &dyn TextMeasure) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    split_html_br_lines(text)
        .into_iter()
        .map(|line| {
            let line = if line.is_empty() { "\u{200b}" } else { line };
            finite(measure.measure(line, TEXT_PX).width).round()
        })
        .fold(0.0, f64::max)
}

/// `value` as an `f64`, or `0.0` when it is negative or not a number.
fn finite(value: f32) -> f64 {
    let value = f64::from(value);
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn owned(lines: Vec<&str>) -> Vec<String> {
    lines.into_iter().map(str::to_owned).collect()
}

/// The lines as Mermaid shows them: its entity placeholders and `#…;`
/// shorthands decoded.
fn display(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| decode_mermaid_entities_to_unicode(&line).into_owned())
        .collect()
}

/// Mermaid's `[label]`, decoded and trimmed; `None` for an empty label, for
/// which Mermaid draws nothing.
fn block_label(raw: &str) -> Option<String> {
    let decoded = decode_mermaid_entities_to_unicode(raw);
    let trimmed = decoded.trim();
    (!trimmed.is_empty()).then(|| format!("[{trimmed}]"))
}

/// JavaScript's `Math.round`: a half rounds up, also below zero.
fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    floor + if value - floor >= 0.5 { 1.0 } else { 0.0 }
}

/// Mermaid's `drawText` first row: the anchor plus half the text margin.
fn drawn_text_first_y(anchor_y: f64, margin: f64) -> f64 {
    anchor_y + if margin > 0.0 { margin / 2.0 } else { 0.0 }
}

/// The first baseline of a frame's title or a divider's label: `boxMargin +
/// boxTextMargin` under `top`, then `drawText`'s half margin, rounded as
/// Mermaid rounds a row.
fn block_text_baseline(top: f64) -> f64 {
    js_round(drawn_text_first_y(
        top + BOX_MARGIN + BOX_TEXT_MARGIN,
        BOX_TEXT_MARGIN,
    ))
}

/// The left and right of a participant's activation stack `depth` bars
/// deep, as Mermaid's `activationBounds` has them: a bare lifeline counts as
/// 1 px either side.
fn stack_bounds(depth: usize, center_x: f64) -> (f64, f64) {
    let (mut left, mut right) = (center_x - 1.0, center_x + 1.0);
    if depth > 0 {
        let start_x = |stacked: usize| center_x + ((stacked as f64) - 1.0) * ACTIVATION_WIDTH / 2.0;
        let (first, last) = (start_x(0), start_x(depth - 1));
        left = left.min(first).min(last);
        right = right
            .max(first + ACTIVATION_WIDTH)
            .max(last + ACTIVATION_WIDTH);
    }
    (left, right)
}

/// Where Mermaid centres a message's number: 1 px inside the outer edge of
/// the activation stacks at the end the arrow leaves — the left one for a
/// rightward arrow, the right one for a leftward arrow, swapped for a
/// reversed arrow — and at the left for a self-message.
fn number_x(
    semantics: &SequenceSignalSemantics,
    from_bounds: (f64, f64),
    to_bounds: (f64, f64),
    start_x: f64,
    stop_x: f64,
) -> f64 {
    let low = from_bounds
        .0
        .min(from_bounds.1)
        .min(to_bounds.0)
        .min(to_bounds.1);
    let high = from_bounds
        .0
        .max(from_bounds.1)
        .max(to_bounds.0)
        .max(to_bounds.1);
    let is_self = (start_x - stop_x).abs() <= f64::EPSILON;
    let rightward = start_x <= stop_x;
    let reversed = semantics.direction == SequenceMessageDirection::Reverse;
    if is_self || rightward != reversed {
        low + 1.0
    } else {
        high - 1.0
    }
}

/// merman's endpoint marker as the scene's: a filled head of any shape is an
/// arrow, Mermaid's `-)` and the open half-heads are the open arrowhead, a
/// cross is a cross.
fn marker(marker: SequenceMessageMarker) -> Marker {
    match marker {
        SequenceMessageMarker::None => Marker::None,
        SequenceMessageMarker::Filled
        | SequenceMessageMarker::FilledHalfTop
        | SequenceMessageMarker::FilledHalfBottom => Marker::Arrow,
        SequenceMessageMarker::Cross => Marker::Cross,
        SequenceMessageMarker::Point
        | SequenceMessageMarker::OpenHalfTop
        | SequenceMessageMarker::OpenHalfBottom => Marker::OpenArrow,
    }
}
