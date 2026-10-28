// SPDX-License-Identifier: MPL-2.0

//! The crate's tests, one file per concern, sharing the fixtures below.
//!
//! Nothing here touches the disk beyond `include_str!` of the fixtures —
//! and the one golden rewritten when `UPDATE_DIAGRAM_GOLDENS` is set — opens
//! a store or reaches the network: text in, status or scene out.

mod counters;
mod engine;
mod inspect;
mod json;
mod layout;
mod line;
mod measure;
mod narrate;
mod pie;
mod robustness;
mod sequence;
mod size;
mod smoke;

use std::sync::Arc;

use crate::{MeasuredLine, TextMeasure, TextMetrics};

/// The flowchart showcase: every box shape in common use, every arrow style,
/// labels on arrows, nested groups with their own direction, an arrow to a
/// group, accessibility metadata, non-ASCII labels, and four lines Koru keeps
/// but ignores. Ten boxes, twelve arrows, two groups.
pub(crate) const FLOWCHART_SHOWCASE: &str =
    include_str!("../../tests/fixtures/flowchart-showcase.mmd");

/// The sequence showcase: an actor and aliased participants, numbering, every
/// arrow kind, activations, notes over and beside, and the loop, alt/else and
/// opt frames. Four participants, twelve messages, two notes.
pub(crate) const SEQUENCE_SHOWCASE: &str =
    include_str!("../../tests/fixtures/sequence-showcase.mmd");

/// What the sequence showcase lacks: a title, a `box` group around two of
/// three participants, and a `critical` frame with an `option`, which Mermaid
/// widens to the left. Three messages, no numbering.
pub(crate) const SEQUENCE_CHECKOUT: &str =
    include_str!("../../tests/fixtures/sequence-checkout.mmd");

/// A small valid pie chart with a title: two sections. Two slices.
pub(crate) const PIE: &str = include_str!("../../tests/fixtures/pie.mmd");

/// A small valid Gantt chart: a kind Koru does not draw.
pub(crate) const GANTT: &str = include_str!("../../tests/fixtures/gantt.mmd");

/// An empty file.
pub(crate) const EMPTY: &str = include_str!("../../tests/fixtures/empty.mmd");

/// The flowchart showcase with the arrow on line 7 written `->`.
pub(crate) const BAD_ARROW_LINE7: &str = include_str!("../../tests/fixtures/bad-arrow-line7.mmd");

/// The flowchart showcase without the inner group's `end`.
pub(crate) const MISSING_END: &str = include_str!("../../tests/fixtures/missing-end.mmd");

/// The flowchart showcase behind a YAML front matter that asks for another
/// theme and layout engine.
pub(crate) const FRONT_MATTER: &str = include_str!("../../tests/fixtures/front-matter.mmd");

/// Every fixture, by file name.
pub(crate) const FIXTURES: [(&str, &str); 8] = [
    ("flowchart-showcase", FLOWCHART_SHOWCASE),
    ("sequence-showcase", SEQUENCE_SHOWCASE),
    ("sequence-checkout", SEQUENCE_CHECKOUT),
    ("pie", PIE),
    ("empty", EMPTY),
    ("bad-arrow-line7", BAD_ARROW_LINE7),
    ("missing-end", MISSING_END),
    ("front-matter", FRONT_MATTER),
];

/// `flowchart TD` with `n` boxes in one chain, `N0 --> N1 --> …`, one arrow
/// per line.
pub(crate) fn chain(n: usize) -> String {
    let mut text = String::from("flowchart TD\n");
    if n == 1 {
        text.push_str("    N0\n");
    }
    for i in 1..n {
        text.push_str(&format!("    N{} --> N{i}\n", i - 1));
    }
    text
}

/// `flowchart TD` with `rows × columns` boxes `R<r>C<c>`, an arrow right and
/// an arrow down from each.
pub(crate) fn grid(rows: usize, columns: usize) -> String {
    let mut text = String::from("flowchart TD\n");
    for r in 0..rows {
        for c in 0..columns {
            if c + 1 < columns {
                text.push_str(&format!("    R{r}C{c} --> R{r}C{}\n", c + 1));
            }
            if r + 1 < rows {
                text.push_str(&format!("    R{r}C{c} --> R{}C{c}\n", r + 1));
            }
        }
    }
    text
}

/// A sequence diagram with two participants and `n` messages between them.
pub(crate) fn sequence_with_messages(n: usize) -> String {
    let mut text = String::from("sequenceDiagram\n    participant A\n    participant B\n");
    for i in 0..n {
        if i % 2 == 0 {
            text.push_str(&format!("    A->>B: Request {i}\n"));
        } else {
            text.push_str(&format!("    B-->>A: Reply {i}\n"));
        }
    }
    text
}

/// The deterministic measurer: every character advances half the size, a
/// line is 1.2 sizes tall with the baseline 0.9 down, and words wrap by that
/// advance. No font, so the same on every machine.
pub(crate) struct Stub;

impl TextMeasure for Stub {
    fn measure(&self, text: &str, px: f32) -> TextMetrics {
        TextMetrics {
            width: text.chars().count() as f32 * 0.5 * px,
            height: 1.2 * px,
            ascent: 0.9 * px,
        }
    }

    fn wrap(&self, text: &str, px: f32, max_width: f32) -> Vec<MeasuredLine> {
        let mut lines: Vec<MeasuredLine> = Vec::new();
        let mut current = String::new();
        for word in text.split(' ').filter(|w| !w.is_empty()) {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if current.is_empty() || self.measure(&candidate, px).width <= max_width {
                current = candidate;
            } else {
                lines.push(MeasuredLine {
                    width: self.measure(&current, px).width,
                    text: std::mem::replace(&mut current, word.to_owned()),
                });
            }
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(MeasuredLine {
                width: self.measure(&current, px).width,
                text: current,
            });
        }
        lines
    }
}

/// The stub, shared the way the host shares its measurer.
pub(crate) fn stub() -> Arc<dyn TextMeasure> {
    Arc::new(Stub)
}
