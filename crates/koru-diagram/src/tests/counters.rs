// SPDX-License-Identifier: MPL-2.0

//! Every `inspect` counts once, in the column of its answer.

use super::{BAD_ARROW_LINE7, FLOWCHART_SHOWCASE, PIE, chain};
use crate::counters::Counters;
use crate::inspect::inspect_uncounted;
use crate::{CountersSnapshot, Limits, Status, inspect};

#[test]
fn every_answer_lands_once_in_its_column() {
    // On a private instance, so the other tests' inspections, which run in
    // parallel with this one, do not land in it.
    let counters = Counters::new();
    let limits = Limits::default();
    for text in [
        FLOWCHART_SHOWCASE,
        BAD_ARROW_LINE7,
        PIE,
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
        }
    );
}

#[test]
fn inspect_counts_in_the_process_wide_counters() {
    // The other tests inspect at the same time, so the deltas are lower
    // bounds; the exact accounting is the test above.
    let before = Counters::snapshot();
    for _ in 0..3 {
        assert_eq!(inspect("", &Limits::default()), Status::Empty);
    }
    let after = Counters::snapshot();
    assert!(
        after.inspections >= before.inspections + 3,
        "{before:?} → {after:?}"
    );
    assert!(after.empty >= before.empty + 3, "{before:?} → {after:?}");
}
