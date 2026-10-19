// SPDX-License-Identifier: MPL-2.0

//! `inspect` answers every input: the fixtures, damaged one byte at a time.
//!
//! Diagram text is untrusted — it arrives from imports, agents and other
//! people — and the engine must never take Koru down with it. Every byte
//! position of every fixture (in steps, to bound the time) is overwritten with
//! each of six bytes that matter to the grammars — a space, a newline, the
//! arrow characters, a brace, a quote — and `inspect` has to return.

use std::panic::{self, AssertUnwindSafe};
use std::time::Instant;

use super::FIXTURES;
use crate::inspect::PANIC_CODE;
use crate::{Limits, Status, inspect};

const REPLACEMENTS: [u8; 6] = *b" \n->{\"";
const STEP: usize = 7;

#[test]
fn inspect_returns_for_every_mutation_of_every_fixture() {
    let limits = Limits::default();
    let started = Instant::now();
    let mut mutations = 0usize;
    let mut failures = Vec::new();
    for (name, fixture) in FIXTURES {
        let bytes = fixture.as_bytes();
        for position in (0..bytes.len()).step_by(STEP) {
            for replacement in REPLACEMENTS {
                let mut mutated = bytes.to_vec();
                mutated[position] = replacement;
                // A byte inside a multi-byte character leaves invalid UTF-8;
                // what a caller could hand `inspect` is the lossy reading.
                let text = String::from_utf8_lossy(&mutated).into_owned();
                mutations += 1;
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
    }
    eprintln!("{mutations} mutations in {:?}", started.elapsed());
    assert!(
        failures.is_empty(),
        "inspect panicked on {} mutations:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
