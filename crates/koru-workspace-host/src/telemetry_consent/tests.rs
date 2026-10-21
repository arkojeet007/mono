// SPDX-License-Identifier: MPL-2.0

use super::*;

fn status(disabled: bool, answered: bool, granted: bool) -> koru_events::wire::EventsStatus {
    koru_events::wire::EventsStatus {
        disabled,
        total_events: 0,
        domain_events: 0,
        instrumentation_events: 0,
        last_seq: 0,
        pruned_events: 0,
        outbox_pending: 0,
        telemetry_consent: granted,
        telemetry_consent_answered: answered,
        baseline_seeded: false,
        tracked_documents: 0,
        sse_subscribers: 0,
        journal_bytes: 0,
        activity_queries: 0,
        activity_rows: 0,
    }
}

#[test]
fn the_reading_follows_the_record() {
    assert_eq!(
        Answer::from_status(&status(false, false, false)),
        Answer::Unanswered
    );
    assert_eq!(
        Answer::from_status(&status(false, true, true)),
        Answer::Granted
    );
    assert_eq!(
        Answer::from_status(&status(false, true, false)),
        Answer::Refused
    );
    assert_eq!(
        Answer::from_status(&status(true, false, false)),
        Answer::Unavailable,
        "a closed journal cannot keep an answer, so nobody is asked for one"
    );
}

/// An older worker's status has no `telemetry_consent_answered`; the default
/// reads as unanswered, so a newer shell shares nothing rather than assumes.
#[test]
fn an_older_workers_status_reads_as_unanswered() {
    let json = serde_json::json!({
        "total_events": 0, "domain_events": 0, "instrumentation_events": 0,
        "last_seq": 0, "pruned_events": 0, "outbox_pending": 0,
        "telemetry_consent": true, "baseline_seeded": false,
        "tracked_documents": 0, "sse_subscribers": 0, "journal_bytes": 0
    });
    let status: koru_events::wire::EventsStatus = serde_json::from_value(json).unwrap();
    assert_eq!(Answer::from_status(&status), Answer::Unanswered);
}

#[test]
fn the_toggle_shows_only_what_a_record_can_back() {
    assert_eq!(Answer::Granted.toggle(), Some(true));
    assert_eq!(Answer::Refused.toggle(), Some(false));
    assert_eq!(Answer::Unanswered.toggle(), Some(false));
    assert_eq!(Answer::Unknown.toggle(), None);
    assert_eq!(Answer::Unavailable.toggle(), None);
}

/// One test owns the process global.
#[test]
fn without_a_worker_a_press_records_nothing() {
    assert_eq!(current(), Answer::Unknown, "nothing is read in a test");
    store(Answer::Unanswered);
    // Without a worker a press changes nothing, so a test can never claim
    // to have recorded anything.
    set(true);
    assert_eq!(current(), Answer::Unanswered);
    store(Answer::Unknown);
}
