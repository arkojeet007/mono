// SPDX-License-Identifier: MPL-2.0

//! **Whether this installation offers dictation at all** — one predicate, so
//! four surfaces cannot answer it four ways.
//!
//! `KORU_VOICE_DISABLED` is a kill switch rather than a diagnostic: with it set
//! no worker starts, no dictation chord is bound, the tray row says
//! *Unavailable* and the Settings section says why. Four separate places have
//! to agree about that, in four different crates, and each of them reaching for
//! `koru_env::switch` on its own is how three of them end up right and the
//! fourth ends up binding a chord that opens a feature nothing will serve.
//!
//! ## Why it lives here
//!
//! Because everything that has to ask already links this crate. The worker
//! (`koru-voice-worker`) does, the client does, the shell does through the
//! client, and Milestone 4's phone does directly. Putting it in the worker
//! would make the shell ask a binary it does not link; putting it in the client
//! would put it out of the worker's reach. This crate is the one place all of
//! them already meet, which is the same argument that put the wire messages and
//! the error codes here.
//!
//! It does not breach the charter. The exclusions are an inference runtime, a
//! microphone, a socket and file I/O; `koru-env` is a dependency-free leaf whose
//! whole content is a table of names and one truthiness rule, and reading a
//! process's own environment is neither of the two things that would make this
//! crate unusable on a phone or inside a test that must never open a device.
//!
//! ## Two reasons, one answer
//!
//! The switch is one reason; a platform this build has no voice runtime for
//! is the other ([`Off`]). [`available`] is false for either, so every surface
//! hides dictation the same way, and [`off`] says which, so the one surface
//! that explains (Settings) and the one that refuses (the Agent's
//! supervision) never blame a switch nobody set.
//!
//! ## Ask once
//!
//! [`disabled`] reads the environment on every call, deliberately — a cached
//! answer would need a lock to be testable and the value cannot change under a
//! running process anyway. So **a caller latches it**: at startup, when the key
//! map is built, when a surface is constructed. A per-frame caller is the bug
//! this note exists to name.

/// The variable's name, so a caller that wants to *say* it in a message names
/// the same string the registry declares.
pub const DISABLE_VAR: &str = koru_env::name::KORU_VOICE_DISABLED;

/// Whether dictation is switched off for this installation.
///
/// The one truthiness rule is `koru_env::switch`'s: on for anything except
/// unset, `""`, `0`, `false`, `no`, `off`. So `KORU_VOICE_DISABLED=0` means
/// *voice is enabled*, which is the reading TECHDEBT 29 closed and the one a
/// person expects.
pub fn disabled() -> bool {
    koru_env::switch(DISABLE_VAR)
}

/// Why this installation offers no dictation, when it does not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Off {
    /// [`DISABLE_VAR`] is set. A person or a launcher switched it off, and
    /// unsetting it turns dictation back on.
    Switch,
    /// This build has no voice runtime for its platform yet: Linux, until its
    /// voice rung (docs/features/linux/TECHNICAL.final.md, D4b). Nothing a
    /// person sets turns it on.
    Platform,
}

/// Desktop Linux, and only desktop Linux: the phone's `target_os` is
/// `"android"`, so Milestone 4's host, which links this crate directly, is
/// untouched.
const PLATFORM_OFF: bool = cfg!(target_os = "linux");

/// **Which reason wins, as arithmetic.** `Platform` outranks `Switch` when
/// both apply, because it names the remedy that is true: unsetting the switch
/// on Linux would not turn dictation on. Pure, so a test can walk all four
/// cases without touching the environment.
fn reason(switch: bool, platform: bool) -> Option<Off> {
    match (switch, platform) {
        (_, true) => Some(Off::Platform),
        (true, false) => Some(Off::Switch),
        (false, false) => None,
    }
}

/// Why dictation is off here, or `None` when it is offered.
pub fn off() -> Option<Off> {
    reason(disabled(), PLATFORM_OFF)
}

/// The same question the way most call sites want to read it.
///
/// Every reason counts, not only the switch: a surface that asks this is
/// asking whether to offer dictation at all. [`disabled`] still means exactly
/// that the switch is set.
pub fn available() -> bool {
    off().is_none()
}

#[cfg(test)]
mod tests;
