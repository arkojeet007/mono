// SPDX-License-Identifier: MPL-2.0

//! The signed, versioned protocol shared by Koru, its updater, and its release tool.
//!
//! This crate deliberately knows nothing about windows, documents, or the editor.
//! It validates remote input, manages update-only paths and state, verifies packages,
//! and extracts archives under strict limits. Platform replacement lives in the
//! updater executable.

mod archive;
mod client;
mod controller;
mod exe;
mod lease;
mod manifest;
mod paths;
#[cfg(target_os = "linux")]
pub mod processes;
mod state;
mod verify;

pub use archive::{
    ArchiveLimits, PackageVersion, ReleaseFiles, WindowsIdentity, extract_tar_gz, extract_zip,
    read_package_version, strict_relative_path,
};
// Channel identity moved to `koru-channel` so that crates which only need to
// know which build they are do not link an HTTP client to find out
// (TECHDEBT 19). Re-exported because this crate is where every existing
// caller already looks for it.
pub use client::{FetchResult, UpdateClient};
pub use controller::{PendingInstall, UpdateController};
pub use exe::{
    AGENT as AGENT_EXE, AI_CHAT as AI_CHAT_EXE, ALL as ALL_EXE, APP as APP_EXE, BUNDLE_PREFIX,
    COM_NOTIFICATION as COM_NOTIFICATION_EXE, DATA_PREFIX, GATEWAY as GATEWAY_EXE,
    INVENTORY_VERSION, LEGACY_STORE as LEGACY_STORE_EXE,
    LEGACY_STORE_SERVER as LEGACY_STORE_SERVER_EXE, LEGACY_VOICE as LEGACY_VOICE_EXE,
    MAC_NOTIFICATION_OWNER_FILES, MCP as MCP_EXE, NOTIFICATION_OWNER_EXECUTABLE,
    NOTIFICATION_OWNER_INFO_PLIST, RELEASE_AUXILIARY, RELEASE_DOCUMENTS, RELEASE_LEGAL,
    SESSION_HOST as SESSION_HOST_EXE, TERMINAL as TERMINAL_EXE, UPDATER as UPDATER_EXE,
    VOICE as VOICE_EXE, WINDOWS_IDENTITY_ASSETS, WINDOWS_IDENTITY_PACKAGE, in_bundle, in_data,
};
pub use koru_channel::{BuildInfo, RELEASE_NOTES_FILE, ReleaseChannel, build_info};
pub use lease::{InstanceLease, RestartGate, UpdateLock, install_key};
pub use manifest::{
    FeedLayout, ManifestError, PackageFormat, ReleaseNotes, UpdateManifest, UpdatePackage,
    UpdateTarget,
};
pub use paths::{UpdatePaths, signal_health};
pub use state::{CachedPackage, FailedBuild, UpdateState};
pub use verify::{PackageVerification, sha256_file, verify_package};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The maximum archive accepted by both the client and Cloudflare publisher.
pub const MAX_PACKAGE_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    InstalledBuildNewer {
        available: u64,
    },
    Available(UpdateManifest),
    Downloading {
        received: u64,
        total: u64,
    },
    Verifying,
    ReadyToInstall {
        manifest: UpdateManifest,
        package: PathBuf,
    },
    Installing,
    Deferred(u64),
    Failed(UpdateFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateFailure {
    pub message: String,
    pub detail: String,
    pub retryable: bool,
}

impl UpdateFailure {
    pub fn new(message: impl Into<String>, detail: impl Into<String>, retryable: bool) -> Self {
        Self {
            message: message.into(),
            detail: detail.into(),
            retryable,
        }
    }
}

/// Why a check is happening. It is not bookkeeping: a `Manual` check retries a
/// build that already failed to install, where an automatic one leaves it
/// alone, so a person pressing the button is not stuck with a silent refusal.
///
/// There was a third reason, `ChannelChanged`, and it went with the preference
/// it existed for — a controller only ever checks its own channel now, so
/// nothing can change under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckReason {
    Startup,
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPackage {
    pub path: PathBuf,
    pub build_number: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallRequest {
    pub package: PathBuf,
    pub install_dir: PathBuf,
    pub restart: PathBuf,
    pub current_build: u64,
    pub target_build: u64,
    pub expected_sha256: String,
    pub signature: String,
    pub health_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartRequest {
    pub executable: PathBuf,
    pub health_token: String,
}

/// Local handoff written by Koru and consumed by the temporary updater. Remote
/// manifests never choose the install or restart paths.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdaterRequest {
    pub package: PathBuf,
    pub install_dir: PathBuf,
    pub restart: PathBuf,
    pub current_build: u64,
    pub target_build: u64,
    pub package_size: u64,
    pub expected_sha256: String,
    pub signature: String,
    pub health_token: String,
    /// The explicit channel root that confines this transaction. Missing on
    /// requests written by already-installed builds and on ordinary installs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Explicit channel root for a local signed update rehearsal. Production
    /// requests omit it and retain the discovered per-user update paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rehearsal_root: Option<PathBuf>,
    /// Signed local manifest re-offered only when a rehearsal returns to the
    /// old build after preparation failure or rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rehearsal_manifest: Option<PathBuf>,
}

/// The seam future macOS and Linux installers implement. The Windows MVP uses
/// the same request types in `koru-updater`.
pub trait PlatformInstaller {
    fn install_dir(&self) -> Result<PathBuf, String>;
    fn is_install_dir_writable(&self) -> Result<bool, String>;
    fn prepare_update(&self, request: &InstallRequest) -> Result<(), String>;
    fn replace_application(&self, request: &InstallRequest) -> Result<(), String>;
    fn restart_application(&self, request: &RestartRequest) -> Result<(), String>;
}

/// Probe without touching the running executable. A missing directory is not
/// writable for update purposes: the active executable cannot live there.
///
/// **No fsync.** This answers a permissions question — may the updater write
/// here — and creating and deleting the probe answers it completely. The
/// `sync_all` this used to do proved durability, which nothing asked, at the
/// price of `FlushFileBuffers` queuing behind every dirty page on the volume:
/// on an install that shares an SMR disk with a release build, that was the
/// 70–157 s the agent's event loop went dark for
/// (docs/features/auto-update/BUGS.md issue 1). Durability is the updater's concern at
/// install time, and staging still fsyncs what it extracts.
pub fn is_writable_install(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".koru-update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(probe).is_ok()
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn an_existing_unrooted_handoff_deserializes_without_a_root() {
        let json = r#"{
            "package":"package.zip",
            "install_dir":"Koru",
            "restart":"Koru/koru.exe",
            "current_build":1,
            "target_build":2,
            "package_size":1,
            "expected_sha256":"hash",
            "signature":"signature",
            "health_token":"0123456789abcdef"
        }"#;
        let request: UpdaterRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.root, None);
        assert_eq!(request.rehearsal_root, None);
        assert_eq!(request.rehearsal_manifest, None);
    }
}
