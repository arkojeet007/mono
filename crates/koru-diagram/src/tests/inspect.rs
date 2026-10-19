// SPDX-License-Identifier: MPL-2.0

//! `detect` and `inspect` on the fixtures: the five statuses, the counts, the
//! line rule on real errors, the limits, and the cost of asking.

use std::time::{Duration, Instant};

use super::{
    BAD_ARROW_LINE7, EMPTY, FLOWCHART_SHOWCASE, FRONT_MATTER, MISSING_END, PIE, SEQUENCE_SHOWCASE,
    chain, sequence_with_messages,
};
use crate::line::last_non_empty_line;
use crate::{Counts, Kind, Limits, Status, detect, inspect};

fn limits() -> Limits {
    Limits::default()
}

#[test]
fn detect_names_the_kind_from_the_header() {
    assert_eq!(detect("graph LR\nA-->B"), Some(Kind::Flowchart));
    assert_eq!(detect("flowchart TD\nA-->B"), Some(Kind::Flowchart));
    assert_eq!(detect("flowchart-elk LR\nA-->B"), Some(Kind::Flowchart));
    assert_eq!(detect("sequenceDiagram\nA->>B: x"), Some(Kind::Sequence));
    assert_eq!(detect("pie\n\"a\": 1"), Some(Kind::Other("pie".to_owned())));
    assert_eq!(
        detect("classDiagram\n    Animal <|-- Duck"),
        Some(Kind::Other("classDiagram".to_owned()))
    );
    assert_eq!(detect("A paragraph of prose, with no diagram in it."), None);
    assert_eq!(detect(""), None);
    // Detection reads the header, not the body: a flowchart that will not
    // parse still detects as one.
    assert_eq!(detect(BAD_ARROW_LINE7), Some(Kind::Flowchart));
    // Front matter and a directive are stripped before the header is read.
    assert_eq!(detect(FRONT_MATTER), Some(Kind::Flowchart));
}

#[test]
fn the_flowchart_showcase_draws_with_its_counts() {
    assert_eq!(
        inspect(FLOWCHART_SHOWCASE, &limits()),
        Status::Draws(Counts::Flowchart {
            nodes: 10,
            edges: 12,
            groups: 2,
        })
    );
}

#[test]
fn the_sequence_showcase_draws_with_its_counts() {
    assert_eq!(
        inspect(SEQUENCE_SHOWCASE, &limits()),
        Status::Draws(Counts::Sequence {
            participants: 4,
            messages: 12,
            notes: 2,
        })
    );
}

#[test]
fn a_bad_arrow_names_its_line() {
    let Status::NotParsed {
        line,
        message,
        code,
    } = inspect(BAD_ARROW_LINE7, &limits())
    else {
        panic!("a bad arrow parses");
    };
    assert_eq!(line, 7);
    assert_eq!(code, "merman.parse.diagram_parse");
    assert!(!message.is_empty());
}

#[test]
fn a_missing_end_names_the_last_non_empty_line() {
    // Computed, not written down: the fixture ends in a newline, and the line
    // named is the last one with text on it, the `click` line.
    let expected = last_non_empty_line(MISSING_END);
    let named = MISSING_END
        .lines()
        .nth(expected as usize - 1)
        .expect("the named line exists");
    assert!(named.contains("click Done"), "named {named:?}");

    let Status::NotParsed { line, code, .. } = inspect(MISSING_END, &limits()) else {
        panic!("a missing `end` parses");
    };
    assert_eq!(line, expected);
    assert_eq!(code, "merman.parse.diagram_parse");
}

#[test]
fn a_pie_chart_is_a_kind_not_drawn() {
    assert_eq!(
        inspect(PIE, &limits()),
        Status::KindNotDrawn {
            kind: Some("pie".to_owned()),
        }
    );
}

#[test]
fn prose_is_no_kind_at_all() {
    assert_eq!(
        inspect("hello world", &limits()),
        Status::KindNotDrawn { kind: None }
    );
}

#[test]
fn nothing_to_draw_is_empty() {
    assert_eq!(EMPTY, "");
    assert_eq!(inspect(EMPTY, &limits()), Status::Empty);
    assert_eq!(inspect("   \n\n", &limits()), Status::Empty);
}

#[test]
fn a_chain_over_the_node_limit_is_too_large() {
    assert_eq!(
        inspect(&chain(1000), &limits()),
        Status::TooLarge {
            kind: Kind::Flowchart,
            count: 1000,
        }
    );
    assert_eq!(
        inspect(&chain(500), &limits()),
        Status::Draws(Counts::Flowchart {
            nodes: 500,
            edges: 499,
            groups: 0,
        })
    );
    assert_eq!(
        inspect(&chain(501), &limits()),
        Status::TooLarge {
            kind: Kind::Flowchart,
            count: 501,
        }
    );
    // The limit is the caller's.
    let tight = Limits {
        max_nodes: 9,
        ..limits()
    };
    assert_eq!(
        inspect(&chain(10), &tight),
        Status::TooLarge {
            kind: Kind::Flowchart,
            count: 10,
        }
    );
}

#[test]
fn a_sequence_over_the_message_limit_is_too_large() {
    assert_eq!(
        inspect(&sequence_with_messages(600), &limits()),
        Status::TooLarge {
            kind: Kind::Sequence,
            count: 600,
        }
    );
    assert_eq!(
        inspect(&sequence_with_messages(500), &limits()),
        Status::Draws(Counts::Sequence {
            participants: 2,
            messages: 500,
            notes: 0,
        })
    );
}

#[test]
fn front_matter_does_not_change_the_counts() {
    assert_eq!(
        inspect(FRONT_MATTER, &limits()),
        inspect(FLOWCHART_SHOWCASE, &limits())
    );
}

#[test]
fn an_error_behind_front_matter_is_named_in_the_text_as_given() {
    // The bad arrow behind the six front-matter lines: merman maps the span
    // back through what it strips, so the line is 6 + 7.
    let text = FRONT_MATTER.replace("Cart([Cart]) --> Validate", "Cart([Cart]) -> Validate");
    assert_ne!(text, FRONT_MATTER);
    let Status::NotParsed { line, .. } = inspect(&text, &limits()) else {
        panic!("a bad arrow parses");
    };
    assert_eq!(line, 13);
}

#[test]
fn inspect_is_cheap_enough_to_ask_on_every_read() {
    let limits = limits();
    // The first call builds the engine; it is reported, not held to the bound.
    let cold = Instant::now();
    let _ = inspect(FLOWCHART_SHOWCASE, &limits);
    eprintln!("inspect, first call in the process: {:?}", cold.elapsed());

    for (name, text) in [
        ("flowchart", FLOWCHART_SHOWCASE),
        ("sequence", SEQUENCE_SHOWCASE),
    ] {
        let best = (0..5)
            .map(|_| {
                let started = Instant::now();
                let _ = inspect(text, &limits);
                started.elapsed()
            })
            .min()
            .expect("five runs");
        eprintln!("inspect({name} showcase), best of five: {best:?}");
        assert!(best < Duration::from_millis(50), "{name}: {best:?}");
    }
}
