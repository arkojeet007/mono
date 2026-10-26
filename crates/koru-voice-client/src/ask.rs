// SPDX-License-Identifier: MPL-2.0

//! **How `koru-voice-worker status`, `stop` and `release` reach a running worker.**
//!
//! A small blocking client, and deliberately not [`crate::link`]: that is a
//! reconnecting, long-lived link that wakes a window and owns a reader thread.
//! What a CLI verb needs is one connection, one question and an answer, and
//! building it on top of the product link would give a diagnostic a dependency
//! on the machinery it may have to diagnose. It shares this crate with the link
//! and nothing else.
//!
//! **Here rather than in the worker** so the Agent can ask without linking the
//! worker, which carries both speech runtimes and `cpal`. The worker's own
//! `status`, `stop` and `release` verbs call it too.
//!
//! That is the standing rule rather than a shortcut: **a diagnostic must not
//! depend on the health of what it diagnoses**
//! ([PRINCIPLES](../../../guide/contributing/PRINCIPLES.md)), which is why
//! `koru-store-server projection` reads its database directly instead of asking
//! the worker.
//!
//! ## Not running is an answer, not an error
//!
//! Connecting *is* the liveness check, so `NotRunning` is what a failed connect
//! means and there is no file that could say otherwise. `status` prints it and
//! `stop` treats it as success — *stop* names a state to reach, and a worker
//! that is already gone is that state.

use std::time::Duration;

use koru_ipc::{Endpoint, IpcError};
use koru_voice::{ClientHello, FRAME_VERSION, PROTOCOL_VERSION, WorkerGreeting};

use koru_voice::control::{Control, Outgoing};
use koru_voice::status::StatusReport;

/// Long enough for a worker whose inference thread is mid-utterance, short
/// enough that a person does not think the command has hung.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What asking a worker a question can come to.
#[derive(Debug)]
pub enum Asked {
    Answered(Box<StatusReport>),
    /// Nothing is listening on this channel and root.
    NotRunning,
    Failed(String),
}

/// Ask the worker on `root` what it is doing.
pub fn status(root: &std::path::Path) -> Asked {
    ask(root, Control::Status)
}

/// Ask the worker on `root` to end, and report what it was doing when it
/// agreed.
pub fn stop(root: &std::path::Path) -> Asked {
    ask(root, Control::Stop)
}

/// Ask the worker on `root` to drop the model, and report what it holds
/// afterwards.
///
/// **Nothing is started.** Like [`status`] and [`stop`], connecting is the
/// whole of the liveness check, so a machine with no worker answers
/// [`Asked::NotRunning`] and has nothing resident to release anyway.
///
/// The report is the answer rather than the acknowledgement: a release asked
/// for during a dictation is refused by the worker, and the `model_loaded` in
/// what comes back is what says so.
pub fn release(root: &std::path::Path) -> Asked {
    ask(root, Control::Release)
}

fn ask(root: &std::path::Path, control: Control) -> Asked {
    let endpoint = match Endpoint::for_service_root(koru_ipc::VOICE, koru_channel::segment(), root)
    {
        Ok(endpoint) => endpoint,
        Err(error) => return Asked::Failed(error.to_string()),
    };
    let mut stream = match koru_ipc::connect_timed(&endpoint, TIMEOUT) {
        Ok(stream) => stream,
        // Every failure to connect means the same thing here: nothing is
        // listening. There is no file to have gone stale and no port to have
        // been reassigned, which is the property the whole rendezvous design
        // is for.
        Err(IpcError::NotRunning(_)) => return Asked::NotRunning,
        Err(error) => return Asked::Failed(error.to_string()),
    };
    if !koru_ipc::same_user(&stream) {
        return Asked::Failed("the voice endpoint belongs to another user".into());
    }
    let hello = ClientHello {
        // A label for the worker's log, and it says which side asked.
        product: "koru-voice-worker-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    if let Err(error) = koru_ipc::frame::write_frame(&mut stream, FRAME_VERSION, &hello) {
        return Asked::Failed(error.to_string());
    }
    match koru_ipc::frame::read_frame::<_, WorkerGreeting>(&mut stream) {
        Ok((version, _)) if version != FRAME_VERSION => {
            return Asked::Failed(format!(
                "the worker framed its greeting at version {version} and this build reads {FRAME_VERSION}"
            ));
        }
        Ok((_, WorkerGreeting::Welcome { .. })) => {}
        Ok((
            _,
            WorkerGreeting::Refused {
                what, ours, theirs, ..
            },
        )) => {
            // Both numbers, in the order the worker names them: `ours` is the
            // worker's and `theirs` is this CLI's, which is the one sentence
            // that says which end is behind.
            return Asked::Failed(format!(
                "the worker refused on the {what} version: it speaks {ours}, this build speaks {theirs}"
            ));
        }
        Err(error) => return Asked::Failed(error.to_string()),
    }
    if let Err(error) = koru_ipc::frame::write_frame(&mut stream, FRAME_VERSION, &control) {
        return Asked::Failed(error.to_string());
    }
    match koru_ipc::frame::read_frame::<_, Outgoing>(&mut stream) {
        Ok((_, Outgoing::Status(report))) => Asked::Answered(report),
        Ok((_, Outgoing::Event(_))) => {
            Asked::Failed("the worker answered a control verb with a session event".into())
        }
        Err(error) => Asked::Failed(error.to_string()),
    }
}
