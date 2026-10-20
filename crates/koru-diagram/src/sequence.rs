// SPDX-License-Identifier: MPL-2.0

//! The sequence scene builder.
//!
//! **A seam, not the builder.** What is here is enough for [`layout`](crate::layout)
//! to answer a sequence diagram with the right bounds, its participants and
//! their lifelines, so the painter and the editor compile and run against
//! the whole [`SequenceScene`] shape. The messages (`msg-<id>` edges with
//! the semantic `signal_semantics()` for markers and dashes), the notes
//! (`note-<id>` nodes), the activation bars, the control frames with their
//! dividers, the `box` groups and the title are all in the patched layout
//! JSON (`third-party/patches/merman-render.md`) and the semantic model, and
//! the sequence builder that follows this fills the vectors they belong in.

use std::collections::HashMap;

use kurbo::{Point, Rect, Vec2};
use merman::diagrams::sequence::SequenceDiagramRenderModel;
use merman_render::model::SequenceDiagramLayout;
use merman_render::text::split_html_br_lines;

use crate::layout::{Fail, SCENE_CODE, internal};
use crate::measure::TextMeasure;
use crate::scene::{
    DiagramScene, Label, Lifeline, Participant, ParticipantKind, Role, SceneBody, SequenceScene,
};
use crate::{Counts, Kind};

/// Builds the participants and lifelines; every other vector stays empty.
pub(crate) fn build(
    model: &SequenceDiagramRenderModel,
    layout: SequenceDiagramLayout,
    counts: Counts,
    measure: &dyn TextMeasure,
) -> Result<DiagramScene, Fail> {
    let (origin, bounds) = match layout.bounds {
        Some(b) => (
            Vec2::new(b.min_x, b.min_y),
            Rect::new(0.0, 0.0, b.max_x - b.min_x, b.max_y - b.min_y),
        ),
        None => (Vec2::ZERO, Rect::ZERO),
    };
    let shift = |x: f64, y: f64| Point::new(x, y) - origin;
    let nodes: HashMap<&str, Rect> = layout
        .nodes
        .iter()
        .map(|n| {
            (
                n.id.as_str(),
                Rect::from_center_size(shift(n.x, n.y), (n.width, n.height)),
            )
        })
        .collect();
    let edges: HashMap<&str, _> = layout.edges.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut participants = Vec::new();
    let mut lifelines = Vec::new();
    for id in &model.actor_order {
        let actor = model
            .actors
            .get(id)
            .ok_or_else(|| internal(SCENE_CODE, format!("participant {id:?} has no actor")))?;
        let node = |prefix: &str| {
            nodes
                .get(format!("{prefix}{id}").as_str())
                .copied()
                .ok_or_else(|| internal(SCENE_CODE, format!("participant {id:?} was not laid out")))
        };
        let top = node("actor-top-")?;
        let bottom = node("actor-bottom-")?;
        let lines = split_html_br_lines(&actor.description)
            .into_iter()
            .map(str::to_owned)
            .collect();
        participants.push(Participant {
            id: id.clone(),
            kind: if actor.actor_type == "actor" {
                ParticipantKind::Actor
            } else {
                ParticipantKind::Box
            },
            top,
            bottom,
            label: Label::placed(lines, top.center(), measure, Role::Text),
        });
        if let Some(edge) = edges.get(format!("lifeline-{id}").as_str())
            && let [first, .., last] = edge.points.as_slice()
        {
            let (first, last) = (shift(first.x, first.y), shift(last.x, last.y));
            lifelines.push(Lifeline {
                x: first.x,
                y0: first.y,
                y1: last.y,
            });
        }
    }

    Ok(DiagramScene {
        kind: Kind::Sequence,
        bounds,
        counts,
        acc_title: model.acc_title.clone(),
        acc_descr: model.acc_descr.clone(),
        body: SceneBody::Sequence(SequenceScene {
            participants,
            lifelines,
            messages: Vec::new(),
            notes: Vec::new(),
            activations: Vec::new(),
            frames: Vec::new(),
            boxes: Vec::new(),
            title: None,
        }),
    })
}
