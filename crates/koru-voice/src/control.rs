// SPDX-License-Identifier: MPL-2.0

//! **What crosses the worker's endpoint, on top of the wire the contract crate
//! already declares.**
//!
//! [`crate::wire`] says everything about a *dictation*: the handshake, three
//! requests, seven events, and the two version numbers. What it deliberately
//! does not say is how somebody asks the worker what it is doing, because
//! `koru-voice-worker status` is a thing this binary has and a phone hosting the same
//! `Session` does not.
//!
//! ## Two verbs beside three, without a second copy of the three
//!
//! [`Incoming`] is `#[serde(untagged)]` over [`Control`] and
//! `crate::Request`, and both of those are internally tagged on `kind`
//! with **disjoint** tag values. So `{"kind":"begin",…}` fails [`Control`]'s
//! variant list and falls through to a request, and `{"kind":"status"}` matches
//! the control side and never reaches one. The alternative was a hand-written
//! superset enum, which would have restated `Begin`'s four fields in a second
//! place — the `koru-store-wire` mistake, where a client sent `json!` literals
//! against types it could not see.
//!
//! `a_control_verb_and_a_request_never_answer_to_the_same_tag` is what keeps
//! the disjointness true, because serde will not complain if it stops being:
//! an overlapping tag would simply make one of the two unreachable.
//!
//! ## The same on the way out
//!
//! [`Outgoing`] tries `crate::Event` first, so a client that has never
//! heard of a status report reads exactly the frames its contract describes —
//! which is what lets Plan 1.4's `koru-voice-client` deserialize `Event`
//! directly and ignore this module entirely.
//!
//! ## Declared here, not in the worker
//!
//! The worker serves these and `koru_voice_client::ask` sends them, and the
//! Agent must not link the worker, which carries both speech runtimes and
//! `cpal`. Which endpoint the worker binds is not here: that is
//! `koru_ipc::VOICE`, and this crate links no socket.

use serde::{Deserialize, Serialize};

use crate::status::StatusReport;

/// The three verbs the CLI uses, and nothing a dictation needs.
///
/// Deliberately small. A worker's control surface grows by one verb at a time
/// until it is a second API, and everything a *person* does to a dictation is
/// already in `crate::Request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Control {
    /// What is this worker doing. Answered with [`Outgoing::Status`].
    Status,
    /// End the process. Answered with [`Outgoing::Status`] first, so `stop` can
    /// say what it stopped, and then the socket closes because the worker is
    /// gone.
    Stop,
    /// **Let go of the model without ending the process.**
    ///
    /// The idle deadline already does this on its own; the verb exists because
    /// several hundred megabytes are resident until it fires, and a person
    /// looking at the notification area is looking at it *now*. Answered with
    /// [`Outgoing::Status`], so the caller can see `model_loaded` go false
    /// rather than take the reply on trust.
    ///
    /// **It is not `Stop` with a nicer name, and that is the whole reason it is
    /// a second verb.** Releasing keeps voice working and makes the next
    /// dictation pay the model load again; stopping ends the worker. One verb
    /// covering both would hide which of the two somebody was getting, and one
    /// of them has a cost next time.
    ///
    /// **A live dictation is not interrupted.** The worker refuses while the
    /// session machine is anything but idle and answers with the status that
    /// says so, because a menu row must not be able to take the model out from
    /// under an utterance somebody is halfway through.
    Release,
}

impl Control {
    /// Every tag [`Control`] answers to. Walked by the disjointness test rather
    /// than written out there, so a fourth verb cannot be added without the
    /// test seeing it.
    pub const TAGS: [&'static str; 3] = ["status", "stop", "release"];
}

/// Anything a client may send after the handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Incoming {
    /// Tried first: the two tags are a closed list, so a dictation request can
    /// never be swallowed by it.
    Control(Control),
    Session(crate::Request),
}

/// Anything the worker sends back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outgoing {
    /// Tried first, and it is the one every client understands.
    Event(crate::Event),
    /// Boxed because a `StatusReport` is an order of magnitude larger than an
    /// `Event`, and the enum is otherwise as big as its largest variant on
    /// every partial transcript.
    Status(Box<StatusReport>),
}

#[cfg(test)]
mod tests;
