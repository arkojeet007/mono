// SPDX-License-Identifier: MPL-2.0

//! Every `inspect` and every `layout` counts once, in the column of its
//! answer.

use std::time::Duration;

use super::{BAD_ARROW_LINE7, FLOWCHART_SHOWCASE, GANTT, chain, stub};
use crate::counters::Counters;
use crate::inspect::inspect_uncounted;
use crate::layout::layout_uncounted;
use crate::{Cancel, CountersSnapshot, Fail, Limits, Status, inspect, layout};

#[test]
fn every_answer_lands_once_in_its_column() {
    // On a private instance, so the other tests' inspections, which run in
    // parallel with this one, do not land in it.
    let counters = Counters::new();
    let limits = Limits::default();
    for text in [
        FLOWCHART_SHOWCASE,
        BAD_ARROW_LINE7,
        GANTT,
        "hello world",
        "",
        "  \n",
        &chain(1000),
    ] {
        counters.record(&inspect_uncounted(text, &limits));
    }
    assert_eq!(
        counters.read(),
        CountersSnapshot {
            inspections: 7,
            draws: 1,
            not_parsed: 1,
            kind_not_drawn: 2,
            too_large: 1,
            empty: 2,
            ..CountersSnapshot::default()
        }
    );
}

#[test]
fn every_layout_lands_once_with_its_time() {
    let counters = Counters::new();
    let measure = stub();
    let limits = Limits::default();
    let cancel = Cancel::new();
    let drawn = layout_uncounted(FLOWCHART_SHOWCASE, &measure, &limits, &cancel, None);
    assert!(drawn.is_ok());
    counters.record_layout(&drawn, Duration::from_millis(7));
    // A status is a layout that never started; it lands in no failure column.
    let status = layout_uncounted(GANTT, &measure, &limits, &cancel, None);
    assert!(matches!(status, Err(Fail::Status(_))));
    counters.record_layout(&status, Duration::from_millis(1));
    counters.record_layout(&Err(Fail::Cancelled), Duration::from_millis(3));
    counters.record_layout(&Err(Fail::TimedOut), Duration::from_millis(2000));
    counters.record_layout(
        &Err(Fail::Internal {
            code: "koru.diagram.panic".to_owned(),
            message: "x".to_owned(),
        }),
        Duration::from_millis(12),
    );
    assert_eq!(
        counters.read(),
        CountersSnapshot {
            layouts: 5,
            layout_cancelled: 1,
            layout_timed_out: 1,
            layout_failed: 1,
            layout_ms_last: 12,
            layout_ms_max: 2000,
            ..CountersSnapshot::default()
        }
    );
}

#[test]
fn inspect_and_layout_count_in_the_process_wide_counters() {
    // The other tests inspect and lay out at the same time, so the deltas
    // are lower bounds; the exact accounting is the tests above.
    let before = Counters::snapshot();
    for _ in 0..3 {
        assert_eq!(inspect("", &Limits::default()), Status::Empty);
    }
    assert!(
        layout(
            FLOWCHART_SHOWCASE,
            &stub(),
            &Limits::default(),
            &Cancel::new()
        )
        .is_ok()
    );
    let after = Counters::snapshot();
    assert!(
        after.inspections >= before.inspections + 3,
        "{before:?} → {after:?}"
    );
    assert!(after.empty >= before.empty + 3, "{before:?} → {after:?}");
    assert!(after.layouts > before.layouts, "{before:?} → {after:?}");
    assert!(after.layout_ms_max >= after.layout_ms_last || after.layouts > 1);
}
