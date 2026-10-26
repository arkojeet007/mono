// SPDX-License-Identifier: MPL-2.0

//! **Where the worker's output goes, and the two rules it obeys.**
//!
//! ```text
//! <store root>/logs/koru-voice-worker.log.2026-08-30
//! ```
//!
//! Beside `koru-api-gateway.log` and `koru-store-server.log` under the same
//! channel root, so one minute of a machine's life reads together — which is
//! the whole reason the file is not beside the executable, and per channel so a
//! nightly worker's log cannot be mistaken for a stable one's. Daily rotation
//! and seven files are `koru-log`'s and none of that logic is ours.
//!
//! ## The content boundary, restated because this process holds the worst of it
//!
//! `docs/contributing/operations/LOGGING.md` forbids audio content, transcripts, and
//! filesystem paths. This binary is the only process in Koru that holds all
//! three at once, so the rule here is absolute and structural rather than
//! careful:
//!
//! - a transcript is a `koru_voice::Transcript`, whose `Debug` is redacted and
//!   which has deliberately no `Display`;
//! - audio never leaves [`crate::ring`] and [`crate::capture`], and neither
//!   emits an event about a sample;
//! - a device is a [`koru_voice::status::DeviceCategory`], which does not hold the
//!   name it would otherwise print;
//! - a model is named by id, version and digest — never by the directory it is
//!   in.
//!
//! `no_log_line_carries_audio_or_a_transcript` drives a whole dictation with a
//! sentinel transcript and a sentinel waveform, at `trace`, and reads back
//! every line.
//!
//! ## The filter
//!
//! `warn` for everything, `info` for this worker and the crates it drives.
//! Without the first half the file fills with whatever a native audio backend
//! decides to say per buffer. `RUST_LOG` overrides both, which is the
//! documented way to turn a target up — there is deliberately **no**
//! `KORU_VOICE_DEBUG`, because `docs/contributing/operations/LOGGING.md` reserves a
//! feature-specific switch for a diagnostic that changes *behaviour*, not for
//! enabling a target's debug level.

use std::path::Path;

pub use koru_log::Logging;

/// What is recorded when nothing says otherwise.
const DEFAULT_FILTER: &str = "warn,koru_voice_worker=info,koru_voice=info,koru_voice_models=info";

/// The same, one level louder, when somebody is watching this process.
///
/// `--console` means a developer at a terminal or a harness reading output, and
/// what they are almost always watching for is the shape of one dictation:
/// which effects were performed, how long the model took, whether a partial
/// arrived. Those are `debug`, because on a live session they are several lines
/// a second and have no business in a file that rotates for seven days.
const CONSOLE_FILTER: &str =
    "warn,koru_voice_worker=debug,koru_voice=debug,koru_voice_models=debug";

/// Start logging. `console` adds stdout beside the rotating file.
///
/// A failure to create logging output must not decide whether the worker can
/// start — that is `koru-log`'s contract and it matters here more than
/// anywhere, because the worker is launched by a supervisor that would read a
/// refusal as a crash loop.
pub fn init(root: &Path, console: bool) -> Logging {
    if console {
        return koru_log::init_file_and_stdout(root, "koru-voice-worker.log", CONSOLE_FILTER);
    }
    koru_log::init_file(root, "koru-voice-worker.log", DEFAULT_FILTER)
}
