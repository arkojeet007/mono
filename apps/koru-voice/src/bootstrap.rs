// SPDX-License-Identifier: MPL-2.0

//! **What a supervising Agent hands the worker on stdin, and what the worker
//! checks about it.**
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
//! ## Where the type lives
//!
//! [`WorkerBootstrap`] itself is declared in `koru_voice::bootstrap`, so the
//! Agent can write one without linking this crate. What stays here is the
//! mechanism the contract crate's charter excludes: reading it from stdin,
//! checking it against this executable's bytes, and the digest that does it.

use koru_voice::bootstrap::{BOOTSTRAP_VERSION, WorkerBootstrap};

/// The digest both fingerprints use.
///
/// `blake3`, and the same spelling `koru_network_bridge::fingerprint` already
/// uses for the gateway's bootstrap — one hash function for one purpose across
/// the product, so a fingerprint computed by either side is comparable by the
/// other. The Agent fingerprints with that function, and both sides' tests
/// assert the same literal digest for `"abc"`.
pub fn fingerprint(value: impl AsRef<[u8]>) -> String {
    blake3::hash(value.as_ref()).to_hex().to_string()
}

/// Read one bootstrap from a reader — stdin, in the product.
///
/// **Bounded at 64 KiB**, the same bound the gateway reads with. A bootstrap is
/// a few hundred bytes; the cap is there because the parent could be anything,
/// and `read_to_string` on an unbounded pipe is a way to make this process
/// allocate whatever the writer feels like.
pub fn read(reader: impl std::io::Read) -> Result<WorkerBootstrap, String> {
    use std::io::Read as _;
    let mut text = String::new();
    reader
        .take(64 * 1024)
        .read_to_string(&mut text)
        .map_err(|error| format!("the Agent bootstrap could not be read: {error}"))?;
    serde_json::from_str(text.trim())
        .map_err(|error| format!("the Agent bootstrap is not readable: {error}"))
}

/// Check that this bootstrap is about this worker, on this root, in this
/// channel.
///
/// The executable's bytes are read to fingerprint them, which is the one
/// genuinely expensive part — a ~10 MB read at startup, on the main thread,
/// before anything is listening. It is paid once and it is what makes
/// "supervising the binary from `.previous/`" a refusal rather than a mystery.
pub fn validate(
    bootstrap: &WorkerBootstrap,
    root: &std::path::Path,
    channel: &str,
) -> Result<(), String> {
    if bootstrap.bootstrap_version != BOOTSTRAP_VERSION {
        return Err(format!(
            "the Agent bootstrap is version {} and this worker reads {BOOTSTRAP_VERSION}",
            bootstrap.bootstrap_version
        ));
    }
    if bootstrap.channel != channel {
        return Err(format!(
            "the Agent bootstrap is for the {} channel and this worker is {channel}",
            bootstrap.channel
        ));
    }
    if bootstrap.root_fingerprint != fingerprint(root.to_string_lossy().as_bytes()) {
        // The *fingerprints* disagree and neither path is printed: a root is a
        // filesystem path, and docs/contributing/operations/LOGGING.md names those beside
        // credentials.
        return Err("the Agent bootstrap names a different root".into());
    }
    let executable =
        std::env::current_exe().map_err(|error| format!("this executable's path: {error}"))?;
    let image = std::fs::read(&executable)
        .map_err(|error| format!("this executable could not be fingerprinted: {error}"))?;
    if bootstrap.installation_fingerprint != fingerprint(image) {
        return Err("the Agent bootstrap belongs to a different worker image".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
