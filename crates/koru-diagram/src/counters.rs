// SPDX-License-Identifier: MPL-2.0

//! Process-wide totals of what this crate was asked and answered.
//!
//! The host's inspector and logs read them; nothing here logs. One relaxed
//! atomic per number: a snapshot is a set of independent reads, so under
//! concurrent inspections the columns need not sum to `inspections` at that
//! instant.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::Status;
use crate::layout::Fail;
use crate::scene::DiagramScene;

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
    /// Calls to [`layout`](crate::layout), whatever they answered.
    pub layouts: u64,
    /// Answers of [`Fail::Cancelled`].
    pub layout_cancelled: u64,
    /// Answers of [`Fail::TimedOut`].
    pub layout_timed_out: u64,
    /// Answers of [`Fail::Internal`].
    pub layout_failed: u64,
    /// How long the last layout took, in milliseconds, whatever it answered.
    pub layout_ms_last: u64,
    /// The longest a layout has taken, in milliseconds.
    pub layout_ms_max: u64,
}

/// The process-wide counters. [`inspect`](crate::inspect) and
/// [`layout`](crate::layout) increment them and [`Counters::snapshot`] reads
/// them; there is nothing else to do with them.
#[derive(Debug)]
pub struct Counters {
    inspections: AtomicU64,
    draws: AtomicU64,
    not_parsed: AtomicU64,
    kind_not_drawn: AtomicU64,
    too_large: AtomicU64,
    empty: AtomicU64,
    layouts: AtomicU64,
    layout_cancelled: AtomicU64,
    layout_timed_out: AtomicU64,
    layout_failed: AtomicU64,
    layout_ms_last: AtomicU64,
    layout_ms_max: AtomicU64,
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
            layouts: AtomicU64::new(0),
            layout_cancelled: AtomicU64::new(0),
            layout_timed_out: AtomicU64::new(0),
            layout_failed: AtomicU64::new(0),
            layout_ms_last: AtomicU64::new(0),
            layout_ms_max: AtomicU64::new(0),
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

    /// Counts one layout, its answer and its time. A [`Fail::Status`] is a
    /// layout that never started and lands in no failure column.
    pub(crate) fn record_layout(&self, result: &Result<DiagramScene, Fail>, elapsed: Duration) {
        self.layouts.fetch_add(1, Ordering::Relaxed);
        let column = match result {
            Ok(_) | Err(Fail::Status(_)) => None,
            Err(Fail::Cancelled) => Some(&self.layout_cancelled),
            Err(Fail::TimedOut) => Some(&self.layout_timed_out),
            Err(Fail::Internal { .. }) => Some(&self.layout_failed),
        };
        if let Some(column) = column {
            column.fetch_add(1, Ordering::Relaxed);
        }
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.layout_ms_last.store(ms, Ordering::Relaxed);
        self.layout_ms_max.fetch_max(ms, Ordering::Relaxed);
    }

    pub(crate) fn read(&self) -> CountersSnapshot {
        CountersSnapshot {
            inspections: self.inspections.load(Ordering::Relaxed),
            draws: self.draws.load(Ordering::Relaxed),
            not_parsed: self.not_parsed.load(Ordering::Relaxed),
            kind_not_drawn: self.kind_not_drawn.load(Ordering::Relaxed),
            too_large: self.too_large.load(Ordering::Relaxed),
            empty: self.empty.load(Ordering::Relaxed),
            layouts: self.layouts.load(Ordering::Relaxed),
            layout_cancelled: self.layout_cancelled.load(Ordering::Relaxed),
            layout_timed_out: self.layout_timed_out.load(Ordering::Relaxed),
            layout_failed: self.layout_failed.load(Ordering::Relaxed),
            layout_ms_last: self.layout_ms_last.load(Ordering::Relaxed),
            layout_ms_max: self.layout_ms_max.load(Ordering::Relaxed),
        }
    }
}
