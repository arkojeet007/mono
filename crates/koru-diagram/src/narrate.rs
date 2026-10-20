// SPDX-License-Identifier: MPL-2.0

//! What a screen reader says about a scene, as data the caller localises.

use serde::{Deserialize, Serialize};

use crate::Counts;
use crate::scene::{DiagramScene, SceneBody};

/// The content of a diagram's narration: *Diagram, flowchart, N boxes, M
/// arrows*, then each arrow as prose. The words are the caller's; this is
/// what they are built from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narration {
    /// `accTitle`, read first when present.
    pub title: Option<String>,
    /// `accDescr`, read after the title when present.
    pub description: Option<String>,
    /// The counts the summary names.
    pub counts: Counts,
    /// The arrows or messages, in source order.
    pub items: Vec<NarrationItem>,
}

/// One arrow or message, by the labels a person sees — never by id. A
/// label's line breaks are spaces here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NarrationItem {
    /// A flowchart arrow: *Checkout, arrow "pays", to Payment*. `from` and
    /// `to` are the labels of the boxes, or the title of a group the arrow
    /// touches.
    Arrow {
        from: String,
        label: Option<String>,
        to: String,
    },
    /// A sequence message: *Person, "Sign in", to Koru app*.
    Message {
        from: String,
        text: String,
        to: String,
    },
}

/// The narration of `scene`.
pub fn narrate(scene: &DiagramScene) -> Narration {
    let items = match &scene.body {
        SceneBody::Flowchart(flowchart) => {
            let name = |id: &str| {
                flowchart
                    .nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map(|n| spoken(&n.label.text))
                    .or_else(|| {
                        flowchart
                            .clusters
                            .iter()
                            .find(|c| c.id == id)
                            .and_then(|c| c.title.as_ref())
                            .map(|t| spoken(&t.text))
                    })
                    .unwrap_or_else(|| id.to_owned())
            };
            flowchart
                .edges
                .iter()
                .map(|edge| NarrationItem::Arrow {
                    from: name(&edge.from),
                    label: edge.label.as_ref().map(|l| spoken(&l.text)),
                    to: name(&edge.to),
                })
                .collect()
        }
        SceneBody::Sequence(sequence) => {
            let name = |id: &str| {
                sequence
                    .participants
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| spoken(&p.label.text))
                    .unwrap_or_else(|| id.to_owned())
            };
            sequence
                .messages
                .iter()
                .map(|message| NarrationItem::Message {
                    from: name(&message.from),
                    text: spoken(&message.label.text),
                    to: name(&message.to),
                })
                .collect()
        }
    };
    Narration {
        title: scene.acc_title.clone(),
        description: scene.acc_descr.clone(),
        counts: scene.counts,
        items,
    }
}

/// A label's text as one spoken line.
fn spoken(text: &str) -> String {
    text.split('\n').collect::<Vec<_>>().join(" ")
}
