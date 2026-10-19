// SPDX-License-Identifier: MPL-2.0

//! The crate's tests, one file per concern, sharing the fixtures below.
//!
//! Nothing here touches the disk beyond `include_str!`, opens a store or
//! reaches the network: text in, status out.

mod counters;
mod engine;
mod inspect;
mod json;
mod line;
mod robustness;
mod smoke;

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

/// A small valid pie chart: a kind Koru does not draw.
pub(crate) const PIE: &str = include_str!("../../tests/fixtures/pie.mmd");

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
pub(crate) const FIXTURES: [(&str, &str); 7] = [
    ("flowchart-showcase", FLOWCHART_SHOWCASE),
    ("sequence-showcase", SEQUENCE_SHOWCASE),
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
