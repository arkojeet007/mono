// SPDX-License-Identifier: MPL-2.0

//! The adapter between the host's measurer and merman's: which operations
//! merman asks for when it lays the showcases out, that every one of them is
//! answered, and that the style merman asks for is ignored.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use merman::svg::{
    HostTextMeasurementRequest, HostTextMeasurer, TextMeasurementOperation as Op,
    TextMeasurementPhase, TextStyle, WrapMode,
};

use super::{FLOWCHART_SHOWCASE, SEQUENCE_SHOWCASE, stub};
use crate::layout::layout_uncounted;
use crate::measure::{HostMeasure, Request, RequestLog};
use crate::{Cancel, Limits, TEXT_PX};

/// Lays `text` out with the stub and returns every request merman made.
fn requests(text: &str) -> Vec<Request> {
    let log: RequestLog = Arc::new(Mutex::new(Vec::new()));
    layout_uncounted(
        text,
        &stub(),
        &Limits::default(),
        &Cancel::new(),
        Some(Arc::clone(&log)),
    )
    .expect("the showcase lays out");
    let requests = log.lock().unwrap().clone();
    assert!(!requests.is_empty(), "merman measured nothing");
    requests
}

fn operations(requests: &[Request]) -> BTreeSet<&'static str> {
    requests
        .iter()
        .map(|request| request.operation.external_name())
        .collect()
}

#[test]
fn the_flowchart_layout_asks_two_operations_and_both_are_answered() {
    let requests = requests(FLOWCHART_SHOWCASE);
    assert_eq!(
        operations(&requests),
        BTreeSet::from(["computed-length", "wrapped"])
    );
    assert!(requests.iter().all(|r| r.answered), "{requests:#?}");
    eprintln!(
        "flowchart showcase: {} requests, operations {:?}",
        requests.len(),
        operations(&requests)
    );
}

#[test]
fn the_sequence_layout_asks_five_operations_and_all_are_answered() {
    let requests = requests(SEQUENCE_SHOWCASE);
    assert_eq!(
        operations(&requests),
        BTreeSet::from([
            "mermaid-calculate-text-dimensions",
            "raw-bbox-height",
            "raw-bbox-width",
            "tspan-bbox-height",
            "tspan-bbox-width",
        ])
    );
    assert!(requests.iter().all(|r| r.answered), "{requests:#?}");
    eprintln!(
        "sequence showcase: {} requests, operations {:?}",
        requests.len(),
        operations(&requests)
    );
}

/// Asks the adapter directly, as merman would, with the style merman sends:
/// the diagram's own 16 px sans.
fn ask(
    operation: Op,
    text: &str,
    max_width: Option<f64>,
) -> Option<merman::svg::HostTextMeasurement> {
    let style = TextStyle {
        font_family: Some("\"trebuchet ms\", verdana, arial, sans-serif;".to_owned()),
        font_size: 16.0,
        font_weight: Some("700".to_owned()),
        ..TextStyle::default()
    };
    HostMeasure::new(stub(), None)
        .measure(HostTextMeasurementRequest {
            operation,
            phase: TextMeasurementPhase::Layout,
            text,
            style: &style,
            max_width,
            wrap_mode: WrapMode::SvgLike,
        })
        .expect("the adapter never errors")
}

#[test]
fn every_request_is_answered_at_text_px_whatever_the_style_asks() {
    use merman::svg::HostTextMeasurement as Answer;
    // Seven characters at half of 14 px, not of the 16 px bold merman asked.
    let expected = f64::from(stub().measure("Sign in", TEXT_PX).width);
    assert_eq!(expected, 49.0);
    let line = f64::from(stub().measure("x", TEXT_PX).height);
    match ask(Op::ComputedLength, "Sign in", None) {
        Some(Answer::Length(width)) => assert_eq!(width, expected),
        other => panic!("{other:?}"),
    }
    match ask(Op::MermaidCalculateTextDimensions, "Sign in", None) {
        Some(Answer::Metrics(m)) => {
            assert_eq!((m.width, m.height, m.line_count), (expected, line, 1));
        }
        other => panic!("{other:?}"),
    }
    match ask(Op::BBoxX, "Sign in", None) {
        Some(Answer::HorizontalExtents { left, right }) => {
            assert_eq!((left, right), (expected / 2.0, expected / 2.0));
        }
        other => panic!("{other:?}"),
    }
    match ask(Op::RawBBoxHeight, "a\nbb", None) {
        Some(Answer::Length(height)) => assert_eq!(height, 2.0 * line),
        other => panic!("{other:?}"),
    }
    // A wrapped request with no width is measured at its own line breaks…
    match ask(Op::Wrapped, "Second provider\n(EU)", None) {
        Some(Answer::Metrics(m)) => {
            assert_eq!((m.width, m.height, m.line_count), (105.0, 2.0 * line, 2));
        }
        other => panic!("{other:?}"),
    }
    // …and one with a width is wrapped by merman's rules at the stub's
    // advance: 7 px a character, 70 px holds "The" and "challenge" apart.
    match ask(Op::WrappedWithRawWidth, "The challenge expires", Some(70.0)) {
        Some(Answer::WrappedWithRawWidth { metrics, raw_width }) => {
            assert_eq!(metrics.line_count, 3);
            assert_eq!(metrics.width, 63.0, "the widest line, \"challenge\"");
            assert_eq!(raw_width, Some(147.0));
        }
        other => panic!("{other:?}"),
    }
    // A browser's baseline offset is declined; merman falls back for it.
    assert!(ask(Op::CreateTextBBoxYOffset, "Sign in", None).is_none());
    assert!(ask(Op::RawMiddleBBoxYOffset, "Sign in", None).is_none());
}
