// SPDX-License-Identifier: MPL-2.0

//! **What a supervising Agent hands the worker on stdin.**
//!
//! The shape is the API Gateway's, copied deliberately: the agent spawns the
//! binary beside its own executable with `--root <path> --supervised`, writes
//! one JSON document to the child's stdin, closes it, and then polls until the
//! endpoint answers. (The gateway's half of that has since become a worker role
//! of `koru-agent.exe` reading one bounded *line* from a pipe the parent keeps
//! open; the voice worker is still a package-owned sibling binary and still
//! reads to EOF.) Three things about that arrangement are worth stating
//! because none of them is obvious from the code that does it.
//!
//! **Stdin rather than an argument.** A command line is visible in every
//! process listing on the machine; a pipe between a parent and its child is
//! not. Nothing secret travels here *yet* — see the field list — but the
//! gateway's bootstrap carries a capability, and putting the voice worker's on
//! a different channel would be one refactor away from putting a credential on
//! an argv.
//!
//! **The fingerprints answer "are we the same installation".** Three channels
//! can be installed side by side, an update leaves an old binary in
//! `.previous/`, and an agent that spawned the wrong one would supervise a
//! worker writing into another channel's model cache. Comparing the channel,
//! a digest of the root and a digest of *this executable's own bytes* makes
//! that a refusal at startup rather than a puzzle later.
//!
//! **The `launch_id` is the agent's, and the worker only reports it.** The
//! greeting a client gets is `koru_voice::WorkerGreeting`, which has no field
//! for one — so a supervisor verifies its launch by asking `status`, where
//! `supervisor_launch_id` is echoed back. Written up in
//! `docs/features/voice/archive/QUESTIONS.md`, because Plan 1.4 owns `ensure_voice` and may
//! prefer to add the field to the greeting instead.
//!
//! ## Where this type lives
//!
//! Here, in the contract crate, because both sides must name it and the Agent
//! must not link the worker (which carries both speech runtimes and `cpal`).
//! Only the *data* is here. Reading it from stdin, checking it, and the digest
//! both fingerprints use stay in the worker's `bootstrap` module: they are file
//! I/O and a hash, which this crate's charter excludes. The Agent computes its
//! fingerprints with `koru_network_bridge::fingerprint`, the same `blake3` hex,
//! and a literal test vector on each side holds the two together.

use serde::{Deserialize, Serialize};

/// The version of this document. A mismatch is refused rather than
/// interpreted: an agent and a worker are two of the six binaries in one zip,
/// installed together and never mixed, so a disagreement here means something
/// is wrong about *which* binaries are running.
pub const BOOTSTRAP_VERSION: u32 = 1;

/// What the agent writes to the worker's stdin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerBootstrap {
    pub bootstrap_version: u32,
    /// Which launch this is. Echoed in `koru-voice-worker status` so a supervisor can
    /// tell *its* worker from one that was already running.
    pub launch_id: String,
    /// `stable`, `beta`, `nightly`, … A nightly agent must not supervise a
    /// stable worker: every endpoint name carries the channel, and the two
    /// would simply never meet.
    pub channel: String,
    /// A digest of the root path both sides were given.
    pub root_fingerprint: String,
    /// A digest of the worker executable's own bytes.
    pub installation_fingerprint: String,
}

#[cfg(test)]
mod tests;
