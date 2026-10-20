// SPDX-License-Identifier: MPL-2.0

//! How large a diagram draws: the measurement behind `Limits::default()`.
//!
//! Ignored by default, because it takes a while and its numbers are the
//! machine's. Run it with `cargo test -p koru-diagram -- --ignored measure`
//! and read the table it prints; `guide/contributing/deps/MERMAN.md` records
//! the one the default was set from.

use std::time::{Duration, Instant};

use super::{chain, grid, sequence_with_messages, stub};
use crate::{Cancel, Limits, layout};

/// Lays `text` out once, under no size limit and a long deadline, and
/// returns how long it took — or how it failed, which is a finding too: the
/// resource budget merman's `interactive` policy keeps is a limit of its own.
fn time(text: &str) -> String {
    let limits = Limits {
        max_nodes: usize::MAX,
        max_messages: usize::MAX,
        deadline: Duration::from_secs(30),
    };
    let started = Instant::now();
    let result = layout(text, &stub(), &limits, &Cancel::new());
    let elapsed = started.elapsed();
    match result {
        Ok(_) => format!("{} ms", elapsed.as_millis()),
        Err(fail) => format!("{fail:?} after {} ms", elapsed.as_millis()),
    }
}

/// The size limit is the largest round hundred of boxes whose layout
/// completes under 200 ms here, chain and grid alike. Prints the table.
#[test]
#[ignore = "a measurement, not a check: cargo test -p koru-diagram -- --ignored measure"]
fn measure_how_large_a_diagram_draws() {
    // Warm the engine so the first row is not the engine's own start.
    let _ = time(&chain(10));
    println!("| shape | nodes | layout |");
    println!("| --- | ---: | ---: |");
    for (rows, columns) in [
        (10, 10),
        (10, 20),
        (15, 20),
        (20, 25),
        (20, 40),
        (30, 30),
        (25, 40),
    ] {
        let n = rows * columns;
        println!("| chain | {n} | {} |", time(&chain(n)));
        println!(
            "| grid {rows} × {columns} | {n} | {} |",
            time(&grid(rows, columns))
        );
    }
    println!();
    println!("| shape | messages | layout |");
    println!("| --- | ---: | ---: |");
    for n in [100, 200, 300, 400, 500, 600] {
        println!(
            "| sequence, 2 participants | {n} | {} |",
            time(&sequence_with_messages(n))
        );
    }
}
