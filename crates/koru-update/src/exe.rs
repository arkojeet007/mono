// SPDX-License-Identifier: MPL-2.0

//! What Koru's shipped executables are called on this platform.
//!
//! One `.exe` suffix, spelled in one place. These were literals — `"koru.exe"`
//! in [`crate::ReleaseFiles::validate`], `"koru-updater.exe"` twice in
//! `koru-workspace-host`'s `launch_updater` — which is harmless on Windows and wrong
//! everywhere else: a mac or Linux build looks for a helper that cannot exist,
//! and its own `release-files.json` fails validation for naming files it does
//! not ship.
//!
//! The agent owns the device and supervises the gateway, and both are resolved
//! *beside* the running executable. The Store worker is linked into the gateway
//! and therefore has no separately shipped image.
//!
//! Nothing pays for this today. Non-Windows builds are the `internal` channel,
//! `BuildInfo::updates_enabled` is false, and the paths are never reached — so
//! this is the fix landing *before* the platform that needs it, not a repair.
//!
//! **A macOS release is not a flat directory**, and this constant does not
//! pretend otherwise. A signed bundle puts the binary at
//! `Koru.app/Contents/MacOS/koru`, so the required-files list and the
//! replacement strategy both change shape when bundles arrive. That is the
//! packaging work in `docs/features/macos/RELEASING.md`, which has since shipped the
//! bundle; naming the file correctly was a prerequisite for it rather than a
//! substitute.

/// Koru itself.
pub const APP: &str = if cfg!(windows) { "koru.exe" } else { "koru" };

/// The standalone host for the reusable Koru AI chat surface. The Agent
/// launches it beside itself for blank opens and canonical chat links.
pub const AI_CHAT: &str = if cfg!(windows) {
    "koru-ai-chat.exe"
} else {
    "koru-ai-chat"
};

/// The agent: the tray, the global shortcut, and the gateway supervisor. It is
/// also what an update restarts into, because it is the process
/// that has to come back and put the rest up again.
pub const AGENT: &str = if cfg!(windows) {
    "koru-agent.exe"
} else {
    "koru-agent"
};

/// The one-shot Windows COM local server that receives reminder toast actions
/// and forwards them to the Agent. It is deliberately a separate process so
/// Windows never owns the lifetime of the long-running Agent.
pub const COM_NOTIFICATION: &str = if cfg!(windows) {
    "koru-com-notification.exe"
} else {
    "koru-com-notification"
};

/// The retired standalone public API host. The gateway is a role of
/// [`AGENT`] now — `koru-agent.exe --worker gateway` — and no current package
/// ships this binary.
///
/// The name is kept for the same reason [`LEGACY_STORE_SERVER`]'s is: an
/// installation being upgraded may still have this executable **running**, and
/// it had a `stop` verb, so the updater has to be able to ask it to stop and to
/// move the file aside. It is deliberately absent from [`ALL`].
pub const GATEWAY: &str = if cfg!(windows) {
    "koru-api-gateway.exe"
} else {
    "koru-api-gateway"
};

/// Compatibility-window name used only when stopping an installed pre-cutover
/// process. It is deliberately absent from every release inventory.
pub const LEGACY_STORE_SERVER: &str = if cfg!(windows) {
    "koru-store-server.exe"
} else {
    "koru-store-server"
};

/// Oldest supported pre-gateway Store process name. It is deliberately absent
/// from every current release inventory; an installed release manifest is what
/// keeps the file owned.
pub const LEGACY_STORE: &str = if cfg!(windows) {
    "koru-store.exe"
} else {
    "koru-store"
};

/// The local Model Context Protocol bridge. It is launched on demand by an MCP
/// host and is package-owned, but it is not a process Koru restarts after an
/// update.
pub const MCP: &str = if cfg!(windows) {
    "koru-mcp.exe"
} else {
    "koru-mcp"
};

/// The voice worker. It opens the microphone, hosts a speech engine and sends
/// back transcripts; audio never leaves that process.
///
/// **Package-owned and started on demand**, like [`MCP`] and unlike [`GATEWAY`]:
/// the Agent starts one on the first `EnsureService { service: Voice }`, which
/// arrives when a person first dictates, so an installation with voice switched
/// off pays nothing. It is in [`ALL`] all the same, because the two things that
/// list reaches are *is this package complete* and *has every image been let go
/// of* — and a worker that was running when an update began holds its
/// executable exactly as the gateway does.
///
/// `apps/koru-voice` declares both `package.name` and `[[bin]] name` as
/// `koru-voice-worker`: the `-worker` suffix distinguishes this process from
/// the `crates/koru-voice` contract package. Keep this constant synchronized
/// with Cargo, release/signing inventories, updater quiescence, Agent sibling
/// resolution, launchers, logs, docs, and tests named by Koru Agent roadmap
/// Plan 1.1. A partial rename makes the worker buildable but neither launchable
/// nor safely replaceable.
pub const VOICE: &str = if cfg!(windows) {
    "koru-voice-worker.exe"
} else {
    "koru-voice-worker"
};

/// The retired standalone terminal window (TERMINATAB W5): sessions run as
/// Koru Workspace tabs now, and no current package ships this binary. The
/// name is kept because the frozen v2-v4 inventories require it and the
/// updater must still recognise — and replace or remove — the file inside an
/// installation it is upgrading from.
pub const TERMINAL: &str = if cfg!(windows) {
    "koru-terminal.exe"
} else {
    "koru-terminal"
};

/// The retired standalone PTY/session service. Sessions are a role of
/// [`AGENT`] now — `koru-agent.exe --worker sessions` — and no current package
/// ships this binary.
///
/// Kept, and kept in the updater's stop sequence, for the same reason
/// [`GATEWAY`] is: it had a `stop` verb, and an installation being upgraded may
/// still have one running. It is deliberately absent from [`ALL`].
pub const SESSION_HOST: &str = if cfg!(windows) {
    "koru-session-host.exe"
} else {
    "koru-session-host"
};

/// The retired pre-rename voice worker, which shipped as `koru-voice.exe`
/// before `apps/koru-voice` took the `-worker` suffix (see [`VOICE`]).
///
/// **The rename retired the name without retiring the process.** The old image
/// has the same `stop` verb the current one has — it is the same program under
/// a different file name — but the updater's stop sequence only ever named the
/// new one, so an upgrade that found a pre-rename worker running had nothing to
/// ask: it waited out the whole sixty-second quiesce and then failed naming a
/// process nobody had asked to stop. It is deliberately absent from [`ALL`];
/// the installed manifest is what keeps the file owned.
///
/// The `.exe` suffix follows the platform for the same reason every other name
/// here does, even though only Windows ever shipped this one.
pub const LEGACY_VOICE: &str = if cfg!(windows) {
    "koru-voice.exe"
} else {
    "koru-voice"
};

/// The external installer replaces package executables while none are running.
pub const UPDATER: &str = if cfg!(windows) {
    "koru-updater.exe"
} else {
    "koru-updater"
};

/// Sparse MSIX carrying the installed Agent's Windows notification identity.
/// It is data, not a process, and therefore deliberately does not belong to
/// [`ALL`]. Inventories v6 and v7 require it only on Windows.
pub const WINDOWS_IDENTITY_PACKAGE: &str = "koru-identity.msix";
/// Sparse-package visual assets resolved from the updater-owned external
/// installation, not from inside the identity-only MSIX.
pub const WINDOWS_IDENTITY_ASSETS: &[&str] = &[
    "assets/StoreLogo.png",
    "assets/Square44x44Logo.png",
    "assets/Square150x150Logo.png",
];

/// Where a package's executables live, relative to the package root.
///
/// Windows ships a flat folder, so the prefix is empty and every name in
/// a release inventory is also a path. A macOS release is a `Koru.app` bundle, and the
/// binaries live in its `Contents/MacOS/`; the data they resolve beside
/// themselves lives under [`DATA_PREFIX`]. `release-files.json` alone stays
/// at the package root: it is the package's inventory, not the build's.
pub const BUNDLE_PREFIX: &str = if cfg!(target_os = "macos") {
    "Koru.app/Contents/MacOS/"
} else {
    ""
};

/// A package-relative path for a file that ships beside the binaries.
pub fn in_bundle(name: &str) -> String {
    format!("{BUNDLE_PREFIX}{name}")
}

/// Where a package's *data* lives — `assets/`, `docs/`, `VERSION`, the
/// release notes. Beside the binaries on Windows; in the bundle's
/// `Resources/` on macOS, because codesign treats everything under
/// `Contents/MacOS/` as nested code and refuses to seal a font. Each
/// runtime reader carries the matching fallback.
pub const DATA_PREFIX: &str = if cfg!(target_os = "macos") {
    "Koru.app/Contents/Resources/"
} else {
    ""
};

/// A package-relative path for a file that ships beside the package's data.
pub fn in_data(name: &str) -> String {
    format!("{DATA_PREFIX}{name}")
}

/// The generation stamp a package carries in `release-files.json`.
///
/// **One number, and nothing selects a requirement table with it.** Earlier
/// builds encoded two independent things in this field: which executables a
/// package had to carry, *and* whether it had staged Windows notification
/// identity — the Windows publisher wrote 5 for an unsigned package and 7 for a
/// signed one. That made the identity choice look like a generation, and it
/// meant retiring an executable needed a new number on both branches. The
/// identity composition is now expressed directly, as
/// [`crate::archive::WindowsIdentity`], and this is what it always should have
/// been: a record of which generation of Koru wrote the list.
///
/// **8 is the generation that retired the standalone gateway and session
/// host; 9 is the generation that made the runtime documents
/// ([`RELEASE_DOCUMENTS`]) required; 10 is the generation that made the
/// license notices ([`RELEASE_LEGAL`]) required.** Publish-time validation refuses any
/// other value, because the only caller that validates strictly is the
/// publisher and the publisher is always built from this tree. Install time
/// never resolves it at all: an updater copied out of an older installation
/// must still install a package whose generation it has never heard of, which
/// is ADR-258 and is why [`crate::ReleaseFiles::load_incoming`] exists.
pub const INVENTORY_VERSION: u32 = 10;

/// **Every executable a current package carries, and every one it is required
/// to carry.** One list, read in both directions.
///
/// The two directions are *is this package complete* — [`crate::ReleaseFiles`]'s
/// strict publish check, which requires every name here — and *is this list what
/// the build actually produces*: `koru-release`'s
/// `every_catalogued_executable_is_built_and_staged` asserts this equals the
/// build/sign/stage table plus the updater, so the build cannot gain or lose a
/// binary without this list saying so. A missing name does not fail loudly —
/// validation simply stops checking for a binary — which is the failure mode
/// that one list checked twice is designed against.
///
/// **The updater's quiesce sweep does not read this.** It walks the union of the
/// incoming and *installed* `release-files.json` lists, which is deliberately
/// wider: it has to wait for images this package no longer ships but the
/// installation still has — the two retired below among them.
///
/// **`koru-com-notification.exe` is required of every Windows package**,
/// signed or unsigned. The Windows build table stages it unconditionally, so
/// no package this publisher can produce is refused by requiring it — and the
/// alternative left a hole: an unsigned Windows package could lose the COM
/// broker that receives reminder toast actions and still validate. What is
/// *not* decided here is whether an unsigned package should exist at all; both
/// compositions are still valid, and that policy question is
/// `docs/contributing/release/BACKLOG.md`'s.
///
/// The retired names — [`GATEWAY`], [`SESSION_HOST`], [`TERMINAL`],
/// [`LEGACY_STORE`], [`LEGACY_STORE_SERVER`], [`LEGACY_VOICE`] — are
/// deliberately absent. A file an installation still has is owned through its
/// own installed manifest, not through this list.
///
/// **Linux carries no [`VOICE`] yet**: the worker is not built there until
/// Linux rung 3 (`docs/features/linux/`), and requiring it would refuse every
/// Linux package. That arm is deleted when voice lands on Linux, together with
/// the Linux arm of `koru-release`'s `CHANNEL_BINARIES`.
#[cfg(windows)]
pub const ALL: &[&str] = &[APP, AI_CHAT, AGENT, COM_NOTIFICATION, MCP, VOICE, UPDATER];
#[cfg(target_os = "linux")]
pub const ALL: &[&str] = &[APP, AI_CHAT, AGENT, MCP, UPDATER];
#[cfg(not(any(windows, target_os = "linux")))]
pub const ALL: &[&str] = &[APP, AI_CHAT, AGENT, MCP, VOICE, UPDATER];

pub const NOTIFICATION_OWNER_INFO_PLIST: &str =
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Info.plist";
pub const NOTIFICATION_OWNER_EXECUTABLE: &str =
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/MacOS/koru-notification-owner";
pub const MAC_NOTIFICATION_OWNER_FILES: &[&str] = &[
    NOTIFICATION_OWNER_INFO_PLIST,
    NOTIFICATION_OWNER_EXECUTABLE,
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/01_Koru_Default.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/02_Soft.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/03_Chime.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/04_Pulse.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/05_Bright.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/06_Calm.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/07_Drop.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/08_Knock.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/09_Signal.wav",
    "Koru.app/Contents/Library/LoginItems/Koru Agent.app/Contents/Resources/assets/reminders/10_Urgent.wav",
];

/// Everything a current package must carry that is **not** an executable.
///
/// macOS owns native notifications from a login-item bundle nested inside the
/// app, so its twelve files are required there and nowhere else. Windows's
/// notification identity is not here: it is staged conditionally, and a
/// conditional requirement is [`crate::archive::WindowsIdentity`]'s job rather
/// than a constant's.
pub const RELEASE_AUXILIARY: &[&str] = if cfg!(target_os = "macos") {
    MAC_NOTIFICATION_OWNER_FILES
} else {
    &[]
};

/// Documents a running Koru reads from beside its executables, as paths
/// under the package's data root ([`in_data`]).
///
/// **The path is the contract, not just the name.** Each has one runtime
/// reader that looks for exactly this relative path: `koru-mcp-server`'s
/// `MODEL_DOCUMENT_PATH` for the model contract, and
/// `koru_delegate_preamble::RELATIVE_PATH` for the delegated-agent preamble
/// — the file a person may edit (ADR-339), which resolves to the compiled-in
/// copy when this one is missing. So the package keeps this `docs/` layout
/// rather than flattening it. The sources are elsewhere in the repository —
/// beside the crate that reads each one — and `koru-release` copies each
/// source to its path here (`RELEASE_DOCUMENT_SOURCES` in
/// `crates/koru-release/src/package.rs`).
///
/// Required of every package, on every platform, since generation 9. Nothing
/// had shipped when the list gained its second entry, so no published
/// package is invalidated by it; the list is append-only from the first
/// release on.
pub const RELEASE_DOCUMENTS: &[&str] = &["docs/mcp/README.md", "docs/prompts/delegated-agent.md"];

/// **The license notices every package carries**, as paths under the
/// package's data root ([`in_data`]): Koru's own license (MPL-2.0), the
/// project notice, and the notices of everything Koru is built from.
///
/// Nothing *reads* these; a person does. They are required anyway, because
/// MPL-2.0 and nearly every license among Koru's dependencies make carrying the
/// license text a condition of distributing a binary at all. A package without
/// them is not one we may publish. `LICENSE` and `NOTICE` are copied from the
/// repository root; `THIRD-PARTY-NOTICES.txt` is generated by `koru-release`
/// for the build it packages. Required since generation 10, append-only.
pub const RELEASE_LEGAL: &[&str] = &["LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.txt"];

#[cfg(test)]
mod tests {
    use super::*;

    /// The suffix is the whole point, so it is what the test asserts — and it
    /// asserts it in both directions, since a constant that is `.exe` on every
    /// platform passes a one-sided check.
    #[test]
    fn the_suffix_follows_the_platform() {
        if cfg!(windows) {
            assert_eq!(APP, "koru.exe");
            assert_eq!(AI_CHAT, "koru-ai-chat.exe");
            assert_eq!(AGENT, "koru-agent.exe");
            assert_eq!(COM_NOTIFICATION, "koru-com-notification.exe");
            assert_eq!(GATEWAY, "koru-api-gateway.exe");
            assert_eq!(LEGACY_STORE_SERVER, "koru-store-server.exe");
            assert_eq!(LEGACY_STORE, "koru-store.exe");
            assert_eq!(LEGACY_VOICE, "koru-voice.exe");
            assert_eq!(MCP, "koru-mcp.exe");
            assert_eq!(VOICE, "koru-voice-worker.exe");
            assert_eq!(UPDATER, "koru-updater.exe");
            assert_eq!(TERMINAL, "koru-terminal.exe");
            assert_eq!(SESSION_HOST, "koru-session-host.exe");
        } else {
            assert_eq!(APP, "koru");
            assert_eq!(AI_CHAT, "koru-ai-chat");
            assert_eq!(AGENT, "koru-agent");
            assert_eq!(COM_NOTIFICATION, "koru-com-notification");
            assert_eq!(LEGACY_VOICE, "koru-voice");
            assert_eq!(VOICE, "koru-voice-worker");
            assert_eq!(UPDATER, "koru-updater");
            for name in ALL {
                assert!(!name.ends_with(".exe"), "{name} carries a Windows suffix");
            }
        }
    }

    /// **The one current executable list, pinned by name and by size.**
    ///
    /// It is read by package validation and by the updater's quiesce sweep, so
    /// a missing or duplicated name is a silent hole in both: validation stops
    /// checking for a binary and the sweep stops waiting for a process to let
    /// go of it. Written out in full rather than derived, because a test that
    /// recomputes the list from the list proves nothing.
    #[test]
    fn every_executable_is_in_the_list_exactly_once() {
        let expected: &[&str] = if cfg!(windows) {
            &[APP, AI_CHAT, AGENT, COM_NOTIFICATION, MCP, VOICE, UPDATER]
        } else if cfg!(target_os = "linux") {
            &[APP, AI_CHAT, AGENT, MCP, UPDATER]
        } else {
            &[APP, AI_CHAT, AGENT, MCP, VOICE, UPDATER]
        };
        assert_eq!(ALL, expected);
        assert_eq!(
            ALL.len(),
            if cfg!(windows) {
                7
            } else if cfg!(target_os = "linux") {
                5
            } else {
                6
            }
        );
        assert_eq!(
            ALL.iter().collect::<std::collections::HashSet<_>>().len(),
            ALL.len()
        );
    }

    /// **Every retired name is out of the current list, and stays a constant.**
    ///
    /// Each of these is a file an installation may still have — and, for four
    /// of them, a process an update may still find running. Being absent from
    /// [`ALL`] is what stops a current package being required to ship them;
    /// still existing as a constant is what lets the updater ask them to stop
    /// and move their files aside. `crates/koru-updater`'s
    /// `the_stop_sweep_asks_every_supported_store_generation` holds the other
    /// half.
    #[test]
    fn no_retired_executable_is_required_of_a_current_package() {
        for retired in [
            GATEWAY,
            SESSION_HOST,
            TERMINAL,
            LEGACY_STORE,
            LEGACY_STORE_SERVER,
            LEGACY_VOICE,
        ] {
            assert!(
                !ALL.contains(&retired),
                "{retired} is retired and must not be required of a current package"
            );
        }
        // Named rather than implied: this is the one the release table stages
        // unconditionally on Windows, and requiring it of *every* Windows
        // package — signed or unsigned — is a decision Plan 7 took on purpose.
        // See [`ALL`].
        if cfg!(windows) {
            assert!(ALL.contains(&COM_NOTIFICATION));
        } else {
            assert!(!ALL.contains(&COM_NOTIFICATION));
        }
    }

    /// The auxiliary set is macOS's login-item bundle and nothing else, with
    /// twelve distinct paths that all sit inside it.
    #[test]
    fn the_auxiliary_requirement_is_the_mac_notification_owner() {
        assert_eq!(MAC_NOTIFICATION_OWNER_FILES.len(), 12);
        let unique = MAC_NOTIFICATION_OWNER_FILES
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), 12);
        assert!(unique.contains(NOTIFICATION_OWNER_INFO_PLIST));
        assert!(unique.contains(NOTIFICATION_OWNER_EXECUTABLE));
        assert!(
            unique.iter().all(
                |path| path.starts_with("Koru.app/Contents/Library/LoginItems/Koru Agent.app/")
            ),
            "every auxiliary path lives inside the login-item bundle"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(RELEASE_AUXILIARY, MAC_NOTIFICATION_OWNER_FILES);
        } else {
            assert!(RELEASE_AUXILIARY.is_empty());
        }
    }

    /// **The two runtime documents, pinned by name.** Each is read at a fixed
    /// relative path by exactly one runtime reader, so a renamed or dropped
    /// entry here is a reader that finds nothing — silently, in the preamble's
    /// case, because it falls back to its compiled-in copy.
    #[test]
    fn the_runtime_documents_are_named_and_live_under_docs() {
        assert_eq!(
            RELEASE_DOCUMENTS,
            &["docs/mcp/README.md", "docs/prompts/delegated-agent.md"]
        );
        assert!(
            RELEASE_DOCUMENTS
                .iter()
                .all(|path| path.starts_with("docs/"))
        );
        assert!(!RELEASE_DOCUMENTS.iter().any(|path| ALL.contains(path)));
    }

    /// The identity package is data rather than a process, so it belongs to
    /// neither the executable list nor the quiesce sweep.
    #[test]
    fn the_windows_identity_package_is_not_an_executable() {
        assert!(!ALL.contains(&WINDOWS_IDENTITY_PACKAGE));
        assert_eq!(WINDOWS_IDENTITY_ASSETS.len(), 3);
        assert!(
            WINDOWS_IDENTITY_ASSETS
                .iter()
                .all(|asset| asset.starts_with("assets/"))
        );
    }
}
