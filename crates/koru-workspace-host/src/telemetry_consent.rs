// SPDX-License-Identifier: MPL-2.0

//! **This device's telemetry answer, as the shell sees it.**
//!
//! The answer itself lives in one place: the events journal's `meta` table,
//! behind `PUT /v1/events/telemetry/consent` (the opt-in contract of
//! 2026-10-11, ADR-176-consenton's amendment).
//! Nothing here stores it. This module is the shell's *reading* of that
//! record, so the Settings toggle can be drawn
//! without anyone touching a socket on the event loop:
//!
//! - **One worker thread does every read and every write.** A press calls
//!   [`set`], which updates the reading at once — so the control moves under
//!   the person's hand — and queues the write. The worker performs it, reads
//!   the record back, and stores what the record now says. A write that did
//!   not land therefore puts the control back, rather than leaving it showing
//!   a choice nobody recorded.
//! - **A process global**, the `spelling` and `voice_prefs` route: a view
//!   cannot reach the `App`, and threading a handle through every
//!   constructor would be state this global already is. [`install`] is
//!   called once per process when the Store has answered; before it, and in
//!   any process that never calls it (every test), the reading is
//!   [`Answer::Unknown`] and no control is shown.
//! - **Nothing asks.** A device with no answer shares nothing; the Settings
//!   toggle is where somebody opts in (maintainers, 2026-10-19).
//! - **A wake, not a poll.** The worker sends `Wake::Settings` after every
//!   answer; the wake carries nothing, and each surface re-reads [`current`]
//!   on its next maintenance pass.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};

use winit::event_loop::EventLoopProxy;

use crate::app::Wake;

/// What this device's record says, as last read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Answer {
    /// Not read yet, or never will be in this process. No control is shown:
    /// one drawn before the record is read could contradict it.
    Unknown = 0,
    /// The events worker is not there to ask (`KORU_EVENTS_DISABLED`, or no
    /// gateway). Nothing is exported either way.
    Unavailable = 1,
    /// No answer under the opt-in contract: a fresh install, or an upgrade
    /// from the old default-granted state. Shares nothing; the toggle shows
    /// it off.
    Unanswered = 2,
    Granted = 3,
    Refused = 4,
}

impl Answer {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Unavailable,
            2 => Self::Unanswered,
            3 => Self::Granted,
            4 => Self::Refused,
            _ => Self::Unknown,
        }
    }

    /// The reading of a worker's status.
    pub fn from_status(status: &koru_events::wire::EventsStatus) -> Self {
        if status.disabled {
            Self::Unavailable
        } else if !status.telemetry_consent_answered {
            Self::Unanswered
        } else if status.telemetry_consent {
            Self::Granted
        } else {
            Self::Refused
        }
    }

    /// The Settings toggle's position, or `None` when there is no record to
    /// show one for. Unanswered draws *off*, which is what it is.
    pub fn toggle(self) -> Option<bool> {
        match self {
            Self::Granted => Some(true),
            Self::Refused | Self::Unanswered => Some(false),
            Self::Unknown | Self::Unavailable => None,
        }
    }
}

enum Job {
    Read,
    Set(bool),
}

static ANSWER: AtomicU8 = AtomicU8::new(Answer::Unknown as u8);
static WORKER: OnceLock<Sender<Job>> = OnceLock::new();

fn store(answer: Answer) {
    ANSWER.store(answer as u8, Ordering::Release);
}

/// The last reading of this device's answer.
pub fn current() -> Answer {
    Answer::from_u8(ANSWER.load(Ordering::Acquire))
}

/// Start the worker and take the first reading. Called when the Store has
/// answered; a second call is a no-op, and a test never makes the first.
pub fn install(proxy: EventLoopProxy<Wake>, gateway_root: Option<PathBuf>) {
    if cfg!(test) || WORKER.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<Job>();
    if WORKER.set(tx.clone()).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("koru-telemetry-consent".into())
        .spawn(move || {
            let segment = koru_workspace::fromstore::segment().to_owned();
            let mut client: Option<koru_api_client::Client> = None;
            while let Ok(job) = rx.recv() {
                if client.is_none() {
                    client = match gateway_root.as_deref() {
                        Some(root) => koru_api_client::Client::open_at(&segment, root).ok(),
                        None => koru_api_client::Client::open(&segment).ok(),
                    };
                }
                let Some(connected) = client.as_ref() else {
                    store(Answer::Unavailable);
                    let _ = proxy.send_event(Wake::Settings);
                    continue;
                };
                if let Job::Set(granted) = job
                    && let Err(error) = connected.events().set_telemetry_consent(granted)
                {
                    tracing::warn!(
                        target: "koru::telemetry",
                        %error,
                        "the telemetry answer was not recorded; showing what the record holds"
                    );
                }
                // Whatever was asked, the reading is what the record now says.
                match connected.events().status() {
                    Ok(status) => store(Answer::from_status(&status)),
                    Err(_) => {
                        store(Answer::Unavailable);
                        // Reconnect on the next job.
                        client = None;
                    }
                }
                let _ = proxy.send_event(Wake::Settings);
            }
        });
    match spawned {
        Ok(_) => {
            let _ = tx.send(Job::Read);
        }
        Err(error) => {
            tracing::error!(target: "koru::telemetry", %error, "the telemetry consent worker could not start");
        }
    }
}

/// Record this device's answer. Never blocks: the reading moves now, the write
/// happens on the worker, and the reading is corrected if the write did not
/// land. Without a worker (a test, a process that never installed one) this
/// changes nothing at all.
pub fn set(granted: bool) {
    let Some(worker) = WORKER.get() else {
        return;
    };
    store(if granted {
        Answer::Granted
    } else {
        Answer::Refused
    });
    let _ = worker.send(Job::Set(granted));
}

/// Read the record again — for a surface that has reason to think another
/// process changed it.
pub fn refresh() {
    if let Some(worker) = WORKER.get() {
        let _ = worker.send(Job::Read);
    }
}

#[cfg(test)]
mod tests;
