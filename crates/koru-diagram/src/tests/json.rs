// SPDX-License-Identifier: MPL-2.0

//! The JSON of every `Status`, exactly as the MCP server will emit it, and
//! back.

use serde_json::{from_str, to_string};

use super::FLOWCHART_SHOWCASE;
use crate::{Counts, Kind, Limits, Status, inspect};

fn round_trips(status: &Status, json: &str) {
    assert_eq!(to_string(status).expect("serialises"), json);
    assert_eq!(&from_str::<Status>(json).expect("deserialises"), status);
}

#[test]
fn every_status_has_its_json() {
    round_trips(
        &Status::Draws(Counts::Flowchart {
            nodes: 10,
            edges: 12,
            groups: 2,
        }),
        r#"{"status":"draws","kind":"flowchart","nodes":10,"edges":12,"groups":2}"#,
    );
    round_trips(
        &Status::Draws(Counts::Sequence {
            participants: 4,
            messages: 12,
            notes: 2,
        }),
        r#"{"status":"draws","kind":"sequence","participants":4,"messages":12,"notes":2}"#,
    );
    round_trips(
        &Status::NotParsed {
            line: 7,
            message: "Diagram parse error (flowchart-v2): Unrecognized token".to_owned(),
            code: "merman.parse.diagram_parse".to_owned(),
        },
        r#"{"status":"not_parsed","line":7,"message":"Diagram parse error (flowchart-v2): Unrecognized token","code":"merman.parse.diagram_parse"}"#,
    );
    round_trips(
        &Status::KindNotDrawn {
            kind: Some("pie".to_owned()),
        },
        r#"{"status":"kind_not_drawn","kind":"pie"}"#,
    );
    round_trips(
        &Status::KindNotDrawn { kind: None },
        r#"{"status":"kind_not_drawn","kind":null}"#,
    );
    round_trips(
        &Status::TooLarge {
            kind: Kind::Flowchart,
            count: 1000,
        },
        r#"{"status":"too_large","kind":"flowchart","count":1000}"#,
    );
    round_trips(
        &Status::TooLarge {
            kind: Kind::Sequence,
            count: 600,
        },
        r#"{"status":"too_large","kind":"sequence","count":600}"#,
    );
    round_trips(&Status::Empty, r#"{"status":"empty"}"#);
}

#[test]
fn a_kind_is_a_plain_string() {
    assert_eq!(to_string(&Kind::Flowchart).unwrap(), r#""flowchart""#);
    assert_eq!(to_string(&Kind::Sequence).unwrap(), r#""sequence""#);
    assert_eq!(
        to_string(&Kind::Other("pie".to_owned())).unwrap(),
        r#""pie""#
    );
    assert_eq!(from_str::<Kind>(r#""sequence""#).unwrap(), Kind::Sequence);
    assert_eq!(
        from_str::<Kind>(r#""gantt""#).unwrap(),
        Kind::Other("gantt".to_owned())
    );
}

#[test]
fn an_inspected_status_serialises_like_the_literal() {
    let status = inspect(FLOWCHART_SHOWCASE, &Limits::default());
    assert_eq!(
        to_string(&status).unwrap(),
        r#"{"status":"draws","kind":"flowchart","nodes":10,"edges":12,"groups":2}"#
    );
}
