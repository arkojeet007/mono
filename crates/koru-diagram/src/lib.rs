// SPDX-License-Identifier: MPL-2.0

//! **Mermaid diagrams for Koru: the one crate that links the engine.**
//!
//! A `mermaid` code block is text until something turns it into a picture.
//! This crate is where that happens, and it is the **only** crate in the
//! workspace that depends on `merman` and `merman-render`: the engine is sealed
//! here the way `koru-blocks` seals the syntax highlighter and `koru-spell`
//! seals the spell engine, so it can be replaced without the editor, the host
//! or the MCP server noticing. Its API speaks Koru's vocabulary — the kind of
//! diagram, whether it draws and why not, a scene made of shapes and laid-out
//! labels — and **no merman type crosses it**.
//!
//! What it answers, for the editor, the host and the MCP server:
//!
//! - [`detect`] names the [`Kind`] of diagram a text declares, from its
//!   header, without reading the body.
//! - [`inspect`] says whether the text draws, as a [`Status`]: the counts the
//!   screen reader and the inspector show when it does, or which of the four
//!   reasons it does not — a line that could not be read, a kind Koru does
//!   not draw, too many boxes or messages — or that there is no text yet. It
//!   detects, parses and counts against [`Limits`]; it never lays out and
//!   never measures text, so the MCP server can afford it once per code block
//!   on every read.
//! - [`layout`] lays the text out as a [`DiagramScene`]: merman's geometry,
//!   every label measured by the host's own [`TextMeasure`] at [`TEXT_PX`],
//!   built into shapes, arrows and placed lines with no theme and no font in
//!   them. It runs the steps of [`inspect`] first, under
//!   [`Limits::deadline`], and stops when its [`Cancel`] is pulled; it
//!   answers a [`Fail`] otherwise.
//! - [`narrate`] turns a scene into what a screen reader says, as a
//!   [`Narration`] the caller localises.
//! - [`line_of`] is the one way a byte offset becomes a line number here, so
//!   the editor and the tests agree with the status.
//! - [`Counters`] keeps process-wide totals of what was asked and answered,
//!   and how long layouts took.
//!
//! The host's measurer and the painter of a scene live in koru-draw; this
//! crate has neither a font nor a renderer.

mod counters;
mod edges;
mod engine;
mod flowchart;
mod inspect;
mod layout;
mod line;
mod measure;
mod narrate;
mod pie;
mod scene;
mod sequence;
mod shapes;

#[cfg(test)]
mod tests;

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use counters::{Counters, CountersSnapshot};
pub use inspect::{detect, inspect};
pub use layout::{Cancel, Fail, layout};
pub use line::line_of;
pub use measure::{MeasuredLine, TEXT_PX, TextMeasure, TextMetrics};
pub use narrate::{Narration, NarrationItem, narrate};
pub use scene::{
    DiagramScene, FlowchartScene, Frame, FrameSection, Label, LabelLine, Lifeline, Marker, Message,
    NodeShape, Note, Participant, ParticipantBox, ParticipantKind, Role, SceneBody, SceneCluster,
    SceneEdge, SceneNode, SequenceScene, StrokeKind,
};
pub use scene::{PieLegendItem, PieScene, PieSlice};

/// The family of a diagram, in Koru's vocabulary.
///
/// Koru draws three: [`Kind::Flowchart`] — merman's `flowchart`, `graph`,
/// `flowchart-v2` and `flowchart-elk` headers alike, because ELK is not
/// compiled and every flowchart is laid out by dagre — [`Kind::Sequence`],
/// and [`Kind::Pie`]. Every other header merman recognises (`gantt`,
/// `classDiagram`, …) is [`Kind::Other`] carrying merman's type id, so a
/// caller can say which kind it is not drawing.
///
/// Serialises as a plain string: `"flowchart"`, `"sequence"`, `"pie"`, or
/// the type id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Flowchart,
    Sequence,
    Pie,
    #[serde(untagged)]
    Other(String),
}

/// How large a diagram Koru draws, and how long it waits for one.
///
/// The host and the MCP server pass one in; nothing here reads the
/// environment. `max_nodes` was **measured**: 800 is the largest round
/// hundred of generated flowchart boxes, in a chain and in a grid alike,
/// that lays out in under 200 ms on a development machine — a 1,000-box
/// grid is refused by merman's own layout work budget before the clock
/// matters (`guide/contributing/deps/MERMAN.md`, *How large a diagram
/// draws*; the ignored test `measure_how_large_a_diagram_draws` reproduces
/// the table). `max_messages` is a working default the same measurement
/// found no reason to move: 600 messages lay out in 5 ms. `deadline` bounds
/// a [`layout`]; [`inspect`] does no layout and does not read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most nodes (boxes) a flowchart may have; groups are not counted.
    pub max_nodes: usize,
    /// Most messages a sequence diagram may have.
    pub max_messages: usize,
    /// How long a layout may run before it is given up as too large.
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 800,
            max_messages: 500,
            deadline: Duration::from_secs(2),
        }
    }
}

/// What a drawable diagram contains, as the screen reader and the inspector
/// say it: the counts, by kind.
///
/// Serialises with a `kind` tag beside the counts:
/// `{"kind":"flowchart","nodes":10,"edges":12,"groups":2}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Counts {
    /// Boxes, arrows, and `subgraph` groups at any depth. A group is not a
    /// box, even when an arrow points at it.
    Flowchart {
        nodes: usize,
        edges: usize,
        groups: usize,
    },
    /// Participants (actors included), messages — the arrows, of every kind —
    /// and notes. Control frames, activations and numbering are not counted.
    Sequence {
        participants: usize,
        messages: usize,
        notes: usize,
    },
    /// A pie chart's sections. They count against `max_nodes`: a slice is a
    /// box of a kind, and no pie has ever come near the limit.
    Pie { slices: usize },
}

impl Counts {
    /// The kind these are the counts of.
    pub fn kind(&self) -> Kind {
        match self {
            Self::Flowchart { .. } => Kind::Flowchart,
            Self::Sequence { .. } => Kind::Sequence,
            Self::Pie { .. } => Kind::Pie,
        }
    }
}

/// The answer for a code block's text: it draws, or one of four reasons it
/// does not, or there is nothing to draw yet.
///
/// Serialises with a `status` tag — `{"status":"draws",…}`,
/// `{"status":"not_parsed","line":7,…}`, `{"status":"kind_not_drawn","kind":"pie"}`,
/// `{"status":"too_large","kind":"flowchart","count":1000}`, `{"status":"empty"}`
/// — which is the shape the MCP server emits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    /// The text parses, is a kind Koru draws, and is within the limits.
    Draws(Counts),
    /// merman could not read the text. `line` is the 1-based line Koru names,
    /// by the rule [`inspect`] states; `message` is merman's English text,
    /// for the inspector and the MCP reply and never for the page; `code` is
    /// merman's stable diagnostic code — `merman.parse.diagram_parse` for
    /// every flowchart or sequence syntax error.
    NotParsed {
        line: u32,
        message: String,
        code: String,
    },
    /// A kind Koru does not draw: merman's type id, or `None` when no diagram
    /// keyword was found at all.
    KindNotDrawn { kind: Option<String> },
    /// Over the limits. `count` is the nodes of a flowchart or the messages of
    /// a sequence diagram. It is `0` only when merman refused the text before
    /// anything could be counted: a source over its 2 MiB budget.
    TooLarge { kind: Kind, count: usize },
    /// The text is empty or whitespace: the editor shows its hint, the MCP
    /// server reports `empty`.
    Empty,
}
