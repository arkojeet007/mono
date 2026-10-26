// SPDX-License-Identifier: MPL-2.0

use super::*;

use crate::{ModelId, ModelTier, Requested, SessionId, VoiceLanguage};

fn begin() -> crate::Request {
    crate::Request::Begin {
        session: SessionId::new(),
        generation: 4,
        language: Requested::Fixed(VoiceLanguage::ZhHans),
        tier: ModelTier::Compact,
        model: Some(ModelId::parse("parakeet-tdt-v3").unwrap()),
        input: crate::InputChoice::SystemDefault,
    }
}

/// **The load-bearing property of the untagged enum**, and serde will not
/// complain if it stops being true: an overlapping tag would simply make one of
/// the two unreachable, which is a dictation request quietly answered as a
/// control verb.
#[test]
fn a_control_verb_and_a_request_never_answer_to_the_same_tag() {
    let request_tags = ["begin", "finish", "cancel"];
    for control in Control::TAGS {
        assert!(
            !request_tags.contains(&control),
            "the control verb `{control}` collides with a dictation request"
        );
    }
    // The tags really are what serde writes, rather than what this test hopes.
    assert_eq!(
        serde_json::to_value(Control::Status).unwrap()["kind"],
        serde_json::json!("status")
    );
    assert_eq!(
        serde_json::to_value(Control::Stop).unwrap()["kind"],
        serde_json::json!("stop")
    );
    assert_eq!(
        serde_json::to_value(&begin()).unwrap()["kind"],
        serde_json::json!("begin")
    );
}

#[test]
fn a_dictation_request_is_not_swallowed_by_the_control_side() {
    let sent = begin();
    let wire = serde_json::to_string(&sent).unwrap();
    match serde_json::from_str::<Incoming>(&wire).unwrap() {
        Incoming::Session(read) => assert_eq!(read, sent),
        other => panic!("a begin was read as {other:?}"),
    }
    // And the three verbs the contract crate declares, so a fourth cannot be
    // added without this seeing it.
    for request in [
        crate::Request::Finish {
            session: SessionId::new(),
            generation: 1,
        },
        crate::Request::Cancel {
            session: SessionId::new(),
            generation: 1,
        },
    ] {
        let wire = serde_json::to_string(&request).unwrap();
        assert!(matches!(
            serde_json::from_str::<Incoming>(&wire).unwrap(),
            Incoming::Session(_)
        ));
    }
}

#[test]
fn a_control_verb_round_trips() {
    for control in [Control::Status, Control::Stop] {
        let wire = serde_json::to_string(&control).unwrap();
        match serde_json::from_str::<Incoming>(&wire).unwrap() {
            Incoming::Control(read) => assert_eq!(read, control),
            other => panic!("{control:?} was read as {other:?}"),
        }
    }
}

/// A client that has never heard of this module reads exactly the frames its
/// contract describes, which is what lets Plan 1.4's client deserialize
/// `crate::Event` directly.
#[test]
fn an_event_is_written_as_its_own_shape_and_nothing_more() {
    let event = crate::Event::State {
        session: SessionId::new(),
        generation: 1,
        state: crate::State::Listening,
    };
    let wrapped = serde_json::to_value(Outgoing::Event(event.clone())).unwrap();
    assert_eq!(wrapped, serde_json::to_value(&event).unwrap());
    match serde_json::from_value::<Outgoing>(wrapped).unwrap() {
        Outgoing::Event(read) => assert_eq!(read, event),
        other => panic!("an event was read as {other:?}"),
    }
}
