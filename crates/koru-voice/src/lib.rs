// SPDX-License-Identifier: MPL-2.0

//! **What a speech engine is asked and what it answers, and one session run
//! against whatever answers it.**
//!
//! The vocabulary, the state machine, the wire messages and the stable error
//! codes. It links **no inference runtime, no microphone and no socket** — and
//! that exclusion is the charter rather than a coincidence of what has been
//! written so far.
//!
//! ## Why a crate with no runtime in it
//!
//! Six components have to agree about what a dictation is: two engine adapters
//! (`koru-voice-sherpa`, `koru-voice-whisper`), the model store, the worker
//! binary, the client that dials it, and — in Milestone 4 — a phone that hosts
//! the same session in-process with no worker and no socket at all. Every one
//! of them names these declarations, so **a renamed field breaks every build
//! rather than drifting into a silent `None`.** That is the `koru-store-wire`
//! lesson, where a client sent `serde_json::json!` literals against types it
//! could not see, two of which were not even `pub`.
//!
//! The exclusion is what makes it possible. A crate that linked ONNX could not
//! be a dependency of the window; a crate that linked `cpal` could not be
//! compiled into a test that must never open an audio device; a crate that
//! linked `koru-ipc` could not run on a phone. What is left is small enough to
//! be a leaf and complete enough that nothing above it has to invent a shape.
//!
//! ## What is here
//!
//! | | |
//! | --- | --- |
//! | [`SAMPLE_RATE`] | 16 kHz mono `f32`, declared once because four components resample into it |
//! | [`SpeechEngine`] | six methods, and the thread contract in [`engine`]'s note |
//! | [`Session`] | `Idle → Loading → Listening → Finalising → Idle`, emitting [`Effect`] and consuming [`HostReply`] |
//! | [`VoiceLanguage`] / [`Requested`] | the eight, and the ninth thing a request can say |
//! | [`Transcript`] | the words, wrapped so a stray `{:?}` cannot print them |
//! | [`input`] | which microphone, chosen by name and resolved against what is plugged in |
//! | [`ErrorCode`] | one closed list of stable lowercase tokens |
//! | [`wire`] | [`Request`], [`Event`], and the two version numbers |
//! | [`bootstrap`], [`control`], [`status`] | what the voice worker and its supervising Agent say to each other beyond a dictation: the stdin handshake, the control verbs and the status report |
//! | [`availability`] | whether this installation offers dictation at all — `KORU_VOICE_DISABLED`, read in one place |
//!
//! ## Three things it is easy to get wrong here
//!
//! **`Requested::Auto` has no BCP 47 tag, and never will.** `auto` is not a
//! tag. Rev 1 of the roadmap asked that every language variant round-trip
//! through BCP 47 while `Auto` was one of them, which is unsatisfiable; the two
//! types exist so the test is true rather than relaxed.
//!
//! **A `Transcript`'s `Debug` reveals nothing and its `Serialize` reveals
//! everything.** Both are deliberate. The socket exists to carry the text; the
//! redaction exists because request tracing and test failures routinely format
//! whole wire messages, and a derived `Debug` on any enclosing type walks
//! straight into its fields.
//!
//! **A stale generation is dropped, not applied.** The hard case is not two
//! sessions with different ids — it is a `Final` that was in flight when its
//! session was cancelled, arriving while the next one is open. Dropping it is
//! arithmetic in [`Session`] rather than vigilance at a call site.
//!
//! ## What it deliberately does not do
//!
//! No inference. No microphone. No file I/O. No transport — only message
//! shapes. No UI, and no translated string: the codes here are branched on and
//! a surface looks up its own words in `koru-i18n`.

pub mod availability;
pub mod bootstrap;
pub mod control;
pub mod engine;
pub mod error;
pub mod input;
pub mod language;
pub mod session;
pub mod status;
pub mod tier;
pub mod transcript;
pub mod wire;

pub use availability::{DISABLE_VAR, Off, available, disabled, off};
pub use engine::{
    EngineEvent, EngineId, EngineInfo, ModelId, SAMPLE_RATE, SessionId, SessionRequest,
    SpeechEngine,
};
pub use error::{EngineError, ErrorCode, VoiceError};
pub use input::{InputChoice, InputDevice, InputResolution, SYSTEM_DEFAULT};
pub use language::{Requested, VoiceLanguage};
pub use session::{Awaiting, Effect, EndReason, HostReply, Session, SessionStats, State};
pub use tier::ModelTier;
pub use transcript::Transcript;
pub use wire::{
    ClientHello, Event, FRAME_VERSION, LevelBucket, PROTOCOL_VERSION, Request, WorkerGreeting,
};

#[cfg(test)]
mod tests;
