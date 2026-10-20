// SPDX-License-Identifier: MPL-2.0

//! `inspect` and `layout` answer every input: the fixtures, damaged one byte
//! at a time.
//!
//! Diagram text is untrusted — it arrives from imports, agents and other
//! people — and the engine must never take Koru down with it. Every byte
//! position of every fixture (in steps, to bound the time) is overwritten with
//! each of six bytes that matter to the grammars — a space, a newline, the
//! arrow characters, a brace, a quote — and `inspect` has to return. The
//! second form runs the two showcases through `layout` the same way: it has
//! to return, without a panic escaping or caught, and without a call
//! outrunning the deadline.

use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use super::{FIXTURES, FLOWCHART_SHOWCASE, SEQUENCE_SHOWCASE, stub};
use crate::inspect::PANIC_CODE;
use crate::{Cancel, Fail, Limits, Status, inspect, layout};

const REPLACEMENTS: [u8; 6] = *b" \n->{\"";
const STEP: usize = 7;

/// Every mutation of `fixture`, as the lossy reading a caller could hand in:
/// a byte inside a multi-byte character leaves invalid UTF-8.
fn mutations(fixture: &str) -> impl Iterator<Item = (usize, u8, String)> + '_ {
    let bytes = fixture.as_bytes();
    (0..bytes.len()).step_by(STEP).flat_map(move |position| {
        REPLACEMENTS.into_iter().map(move |replacement| {
            let mut mutated = bytes.to_vec();
            mutated[position] = replacement;
            (
                position,
                replacement,
                String::from_utf8_lossy(&mutated).into_owned(),
            )
        })
    })
}

#[test]
fn inspect_returns_for_every_mutation_of_every_fixture() {
    let limits = Limits::default();
    let started = Instant::now();
    let mut count = 0usize;
    let mut failures = Vec::new();
    for (name, fixture) in FIXTURES {
        for (position, replacement, text) in mutations(fixture) {
            count += 1;
            let what = || format!("{name}: byte {position} replaced by {replacement:?}");
            match panic::catch_unwind(AssertUnwindSafe(|| inspect(&text, &limits))) {
                Err(_) => failures.push(format!("{} (escaped)", what())),
                Ok(Status::NotParsed { code, message, .. }) if code == PANIC_CODE => {
                    failures.push(format!("{} (caught: {message})", what()));
                }
                Ok(_) => {}
            }
        }
    }
    eprintln!("{count} mutations in {:?}", started.elapsed());
    assert!(
        failures.is_empty(),
        "inspect panicked on {} mutations:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn layout_returns_for_every_mutation_of_the_showcases() {
    let limits = Limits::default();
    let measure = stub();
    let started = Instant::now();
    let mut count = 0usize;
    let mut drawn = 0usize;
    let mut slowest = Duration::ZERO;
    let mut failures = Vec::new();
    for (name, fixture) in [
        ("flowchart-showcase", FLOWCHART_SHOWCASE),
        ("sequence-showcase", SEQUENCE_SHOWCASE),
    ] {
        for (position, replacement, text) in mutations(fixture) {
            count += 1;
            let what = || format!("{name}: byte {position} replaced by {replacement:?}");
            let call = Instant::now();
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                layout(&text, &measure, &limits, &Cancel::new())
            }));
            let elapsed = call.elapsed();
            slowest = slowest.max(elapsed);
            if elapsed > limits.deadline + Duration::from_millis(250) {
                failures.push(format!("{} (took {elapsed:?})", what()));
            }
            match result {
                Err(_) => failures.push(format!("{} (escaped)", what())),
                Ok(Err(Fail::Internal { code, message })) if code == PANIC_CODE => {
                    failures.push(format!("{} (caught: {message})", what()));
                }
                Ok(Ok(_)) => drawn += 1,
                Ok(Err(_)) => {}
            }
        }
    }
    eprintln!(
        "{count} mutated layouts in {:?}, {drawn} drew, slowest {slowest:?}",
        started.elapsed()
    );
    assert!(drawn > 0, "some mutations still draw");
    assert!(
        failures.is_empty(),
        "layout failed on {} mutations:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
