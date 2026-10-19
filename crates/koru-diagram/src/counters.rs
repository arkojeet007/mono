// SPDX-License-Identifier: MPL-2.0

//! Process-wide totals of what this crate was asked and answered.
//!
//! The host's inspector and logs read them; nothing here logs. One relaxed
//! atomic per number: a snapshot is a set of independent reads, so under
//! concurrent inspections the columns need not sum to `inspections` at that
//! instant. The layout work adds its own counters beside these.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::Status;

/// The totals at one moment, as plain numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountersSnapshot {
    /// Calls to [`inspect`](crate::inspect), whatever they answered.
    pub inspections: u64,
    /// Answers of [`Status::Draws`].
    pub draws: u64,
    /// Answers of [`Status::NotParsed`].
    pub not_parsed: u64,
    /// Answers of [`Status::KindNotDrawn`].
    pub kind_not_drawn: u64,
    /// Answers of [`Status::TooLarge`].
    pub too_large: u64,
    /// Answers of [`Status::Empty`].
    pub empty: u64,
}

/// The process-wide counters. [`inspect`](crate::inspect) increments them and
/// [`Counters::snapshot`] reads them; there is nothing else to do with them.
#[derive(Debug)]
pub struct Counters {
    inspections: AtomicU64,
    draws: AtomicU64,
    not_parsed: AtomicU64,
    kind_not_drawn: AtomicU64,
    too_large: AtomicU64,
    empty: AtomicU64,
}

static GLOBAL: Counters = Counters::new();

impl Counters {
    pub(crate) const fn new() -> Self {
        Self {
            inspections: AtomicU64::new(0),
            draws: AtomicU64::new(0),
            not_parsed: AtomicU64::new(0),
            kind_not_drawn: AtomicU64::new(0),
            too_large: AtomicU64::new(0),
            empty: AtomicU64::new(0),
        }
    }

    /// The process-wide instance.
    pub(crate) fn global() -> &'static Self {
        &GLOBAL
    }

    /// The process-wide totals since the process started.
    pub fn snapshot() -> CountersSnapshot {
        GLOBAL.read()
    }

    /// Counts one inspection and its answer.
    pub(crate) fn record(&self, status: &Status) {
        self.inspections.fetch_add(1, Ordering::Relaxed);
        let column = match status {
            Status::Draws(_) => &self.draws,
            Status::NotParsed { .. } => &self.not_parsed,
            Status::KindNotDrawn { .. } => &self.kind_not_drawn,
            Status::TooLarge { .. } => &self.too_large,
            Status::Empty => &self.empty,
        };
        column.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn read(&self) -> CountersSnapshot {
        CountersSnapshot {
            inspections: self.inspections.load(Ordering::Relaxed),
            draws: self.draws.load(Ordering::Relaxed),
            not_parsed: self.not_parsed.load(Ordering::Relaxed),
            kind_not_drawn: self.kind_not_drawn.load(Ordering::Relaxed),
            too_large: self.too_large.load(Ordering::Relaxed),
            empty: self.empty.load(Ordering::Relaxed),
        }
    }
}
