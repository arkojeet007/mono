// SPDX-License-Identifier: MPL-2.0

//! **Keeping this installation up to date.**
//!
//! Checking, downloading and installing used to belong to `koru.exe`, and the
//! whole shape of it was a window's shape: the controller was polled from a
//! loop that was already awake for the caret, the person answered a banner in
//! front of them, and the process that asked for the update was the process
//! that had to exit for it.
//!
//! None of that survives a multi-process installation. A swap has to replace
//! `koru.exe`, `koru-agent.exe` and every helper beside them together, so
//! somebody has to bring the whole set down **in order** and be the last one
//! standing — and the only process that outlives every window is this one.
//!
//! ## The decisions are pure; the performing is not
//!
//! The same split [`crate::route`] and [`crate::registry`] follow.
//! [`report_for`], [`allows`] and [`Handoff`] answer *what should the person be
//! told*, *may they do that yet*, and *is the stack down yet* — and none of them
//! opens a socket, stops a process or draws anything. That is what makes every
//! branch assertable without a window, which is most of this module's tests.
//!
//! [`launch_updater`], [`stop_store`] and [`offer_in_a_dialog`] are the half
//! that touches the machine.
//!
//! ## Two consents, and the second one is not implied by the first
//!
//! A person agrees to *download*, and then separately to *install*. It is not
//! politeness: a download is a background transfer they can ignore, and an
//! install closes their windows and restarts the machine's Koru. Agreeing to
//! the first has never meant agreeing to the second.
//!
//! [`allows`] is where that stops being a convention. The stage has to be
//! `Available` for `Download` and `Ready` for `Install`, so a shell cannot skip
//! a step even by sending the wrong message — see
//! [`koru_agent_wire::UpdateChoice`], where the contract is written down.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use koru_agent_wire::{UpdateChoice, UpdateReport, UpdateStage};
use koru_update::{PendingInstall, UpdateManifest, UpdateStatus};

use crate::registry::Shell;

/// How long the install waits for the *next* window to go before giving up.
///
/// **The clock measures silence, not the whole pass.** It used to be a single
/// 30-second budget for every window together, and on the other side of it is
/// a person reading a blocking modal — one per process, sometimes stacked
/// behind each other, asking whether to keep work the store has not confirmed.
/// Four windows meant four dialogs against one 30-second budget, and a
/// perfectly reasonable answer arrived after the update had already given up.
///
/// Each window that closes resets it, so the bound is on *no progress at all*
/// rather than on how long a person takes. Still bounded, because a window
/// that will never answer must not leave the update waiting forever — but the
/// only way to hit it now is for nothing to happen for two whole minutes.
pub const WAIT_FOR_SHELLS: Duration = Duration::from_secs(120);

/// What the agent tells every window about updating, built once and pushed to
/// all of them.
///
/// One report for every connection, deliberately: two windows showing different
/// answers to "is there an update" is a bug somebody would report as flicker.
pub fn report_for(
    status: &UpdateStatus,
    can_install: bool,
    automatic_checks: bool,
    channel: &str,
) -> UpdateReport {
    let mut report = UpdateReport {
        stage: UpdateStage::Idle,
        version: String::new(),
        build_number: 0,
        title: String::new(),
        summary: Vec::new(),
        notes_url: None,
        received: 0,
        total: 0,
        can_install,
        download_url: None,
        message: None,
        detail: None,
        channel: channel.to_string(),
        automatic_checks,
        active_session_count: 0,
        session_generation: 0,
    };

    // A manifest fills in the same six fields wherever it appears, so the
    // arms below say only what is different about them.
    fn offer(report: &mut UpdateReport, manifest: &UpdateManifest) {
        report.version = manifest.version.clone();
        report.build_number = manifest.build_number;
        report.title = manifest.release_notes.title.clone();
        report.summary = manifest.release_notes.summary.clone();
        report.notes_url = manifest.release_notes.url.clone();
        report.download_url = Some(manifest.package.url.clone());
    }

    match status {
        UpdateStatus::Idle => {}
        UpdateStatus::Checking => report.stage = UpdateStage::Checking,
        UpdateStatus::UpToDate => report.stage = UpdateStage::UpToDate,
        UpdateStatus::InstalledBuildNewer { available } => {
            report.stage = UpdateStage::InstalledNewer;
            // The build the *channel* has, not one being offered. This
            // installation is ahead of it and will not go back.
            report.build_number = *available;
        }
        UpdateStatus::Available(manifest) => {
            report.stage = UpdateStage::Available;
            offer(&mut report, manifest);
        }
        UpdateStatus::Downloading { received, total } => {
            report.stage = UpdateStage::Downloading;
            report.received = *received;
            report.total = *total;
        }
        UpdateStatus::Verifying => report.stage = UpdateStage::Verifying,
        UpdateStatus::ReadyToInstall { manifest, .. } => {
            report.stage = UpdateStage::Ready;
            offer(&mut report, manifest);
        }
        UpdateStatus::Installing => report.stage = UpdateStage::Installing,
        UpdateStatus::Deferred(build) => {
            report.stage = UpdateStage::Deferred;
            report.build_number = *build;
        }
        UpdateStatus::Failed(failure) => {
            report.stage = UpdateStage::Failed;
            report.message = Some(failure.message.clone());
            report.detail = Some(failure.detail.clone());
        }
    }
    report
}

/// Whether this choice is answerable from this stage.
///
/// **The two-step consent, as a function.** `Download` needs an offer to
/// download and `Install` needs something downloaded; everything else is either
/// always reasonable (`CheckNow`, `SetAutomatic`) or names the state it acts on.
///
/// A refusal here becomes `Response::Failed { code: "wrong_stage" }` rather than
/// a silently ignored message, because a window that pressed a button and saw
/// nothing happen has no way to tell that from a hung agent.
pub fn allows(stage: UpdateStage, choice: UpdateChoice) -> bool {
    match choice {
        UpdateChoice::Download => stage == UpdateStage::Available,
        UpdateChoice::Install { .. } => stage == UpdateStage::Ready,
        // Putting off an offer, whichever half of it is showing.
        UpdateChoice::Later => matches!(stage, UpdateStage::Available | UpdateStage::Ready),
        UpdateChoice::Cancel => matches!(stage, UpdateStage::Downloading | UpdateStage::Verifying),
        UpdateChoice::Retry => stage == UpdateStage::Failed,
        // Only meaningful while the install is asking windows to close, and
        // harmless to refuse otherwise: a window that says it could not close
        // when nothing asked it to is not describing this installation.
        UpdateChoice::CloseRefused => stage == UpdateStage::Installing,
        UpdateChoice::DismissNewer => stage == UpdateStage::InstalledNewer,
        // Always answerable: neither is about an offer.
        UpdateChoice::CheckNow | UpdateChoice::SetAutomatic { .. } => true,
    }
}

/// The stable code a refused [`allows`] answers with.
pub const WRONG_STAGE: &str = "wrong_stage";
/// There is no update to act on.
pub const NO_UPDATE: &str = "no_update";
/// This agent does not manage updates for this installation.
pub const NOT_PERMITTED: &str = "not_permitted";
/// The active terminal-session set no longer matches what the person saw.
pub const SESSIONS_CHANGED: &str = "sessions_changed";

/// Bringing the stack down, one window at a time.
///
/// An install cannot begin until every `koru.exe` on this installation has gone,
/// and they do not go instantly: a window with unsaved work asks its person
/// first, and that person may say no. So this is a state rather than a
/// blocking wait — the event loop advances it as windows disconnect, and never
/// sits still waiting for one.
///
/// **Shells first, then the store.** A window saves on its way out, and a store
/// that had already stopped could not accept those writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// No install in progress.
    Idle,
    /// Every window has been asked to close.
    AskingShells {
        deadline: Instant,
        /// Windows still connected. Reaching zero is what advances this.
        remaining: usize,
    },
    /// UI clients are gone; session trees are terminating gracefully.
    StoppingSessions,
    /// The windows are gone; the standalone chat is being asked to close.
    StoppingChat,
    /// Every windowed process and terminal session is gone; stop the store.
    StoppingStore,
    /// Everything is down and the updater may be spawned.
    Ready,
    /// It cannot go ahead, and why — shown in the banner rather than swallowed.
    Aborted(String),
}

impl Handoff {
    /// Begin, having just asked `remaining` windows to close.
    ///
    /// Zero windows is not a special case: it advances immediately, which is
    /// the ordinary path for an agent sitting alone in the notification area.
    pub fn begin(remaining: usize, now: Instant) -> Self {
        if remaining == 0 {
            Self::StoppingChat
        } else {
            Self::AskingShells {
                deadline: now + WAIT_FOR_SHELLS,
                remaining,
            }
        }
    }

    /// A window disconnected.
    ///
    /// **Progress resets the deadline.** A person answering one save prompt is
    /// evidence the pass is alive, and the next window's prompt deserves the
    /// same wait the first one got rather than whatever is left of a shared
    /// budget.
    pub fn shell_gone(&mut self, remaining: usize, now: Instant) {
        if let Self::AskingShells { .. } = self {
            if remaining == 0 {
                *self = Self::StoppingChat;
            } else if let Self::AskingShells {
                remaining: left,
                deadline,
            } = self
            {
                *left = remaining;
                *deadline = now + WAIT_FOR_SHELLS;
            }
        }
    }

    /// A window said no — someone chose to keep unsaved work.
    ///
    /// **Unsaved work always wins.** The update waits; it does not negotiate,
    /// and it certainly does not close the window anyway. The person gets the
    /// banner back and can try again when they are ready.
    pub fn refused(&mut self, why: &str) {
        if !matches!(self, Self::Idle) {
            *self = Self::Aborted(why.to_string());
        }
    }

    /// Give up if the windows did not go in time.
    pub fn expire(&mut self, now: Instant) {
        if let Self::AskingShells { deadline, .. } = self {
            if now >= *deadline {
                *self = Self::Aborted(
                    "a Koru window did not close in time; nothing was replaced".into(),
                );
            }
        }
    }

    /// One word for the routing log, so a `connection gone` line can say
    /// whether a handoff was in progress when the window left.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AskingShells { .. } => "asking-shells",
            Self::StoppingSessions => "stopping-sessions",
            Self::StoppingChat => "stopping-chat",
            Self::StoppingStore => "stopping-store",
            Self::Ready => "ready",
            Self::Aborted(_) => "aborted",
        }
    }

    /// When the loop next needs to look at this, if ever.
    pub fn deadline(&self) -> Option<Instant> {
        match self {
            Self::AskingShells { deadline, .. } => Some(*deadline),
            _ => None,
        }
    }

    /// Whether an install is under way — so a second `Install` is ignored
    /// rather than starting a second handoff over the first.
    pub fn in_progress(&self) -> bool {
        !matches!(self, Self::Idle | Self::Aborted(_))
    }
}

/// The windows to reopen once the update has been installed.
///
/// Recorded before they are asked to close, because after that the registry is
/// empty and there is nothing left to ask. Without this an update would
/// silently close somebody's windows and hand back a tray icon.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reopen {
    /// `(profile, workspace)` in their storage spellings, deduplicated: two
    /// windows on one workspace should not come back as two windows, because
    /// the second would open over the first with nothing to distinguish them.
    pub windows: Vec<(String, String)>,
    /// A durable idempotency claim for each window launch that has begun.
    ///
    /// Written before `koru.exe` is spawned and retained until that shell says
    /// `Hello`. Replaying the same claim is safe because the shell holds its
    /// claim endpoint for its whole lifetime; at most one replay can continue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    claims: Vec<ReopenClaim>,
    /// The standalone AI host was live when quiescence began.
    #[serde(default)]
    pub ai_chat: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ReopenClaim {
    profile: String,
    workspace: String,
    token: String,
}

impl Reopen {
    /// Read the registry before anything is asked to close.
    pub fn from_shells(shells: &[&Shell]) -> Self {
        let mut seen = HashSet::new();
        let mut windows = Vec::new();
        for shell in shells {
            let pair = (shell.profile.to_storage(), shell.workspace.to_storage());
            if seen.insert(pair.clone()) {
                windows.push(pair);
            }
        }
        Self {
            windows,
            claims: Vec::new(),
            ai_chat: false,
        }
    }

    /// Remove windows that never closed (or already reconnected) before an
    /// aborted handoff is recovered. The record is deduplicated by workspace,
    /// so one matching live shell satisfies that entry.
    pub fn retain_closed(&mut self, shells: &[&Shell]) {
        self.windows.retain(|(profile, workspace)| {
            !shells.iter().any(|shell| {
                shell.profile.to_storage() == *profile && shell.workspace.to_storage() == *workspace
            })
        });
        self.retain_live_claims();
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty() && !self.ai_chat
    }

    /// Return the stable claim for one pending window, minting a v4 UUID when
    /// this is its first launch attempt.
    ///
    /// The caller must persist the returned mutation before spawning. A crash
    /// after that save can safely replay the same claim; a crash before the
    /// spawn merely leaves work for the next Agent.
    pub fn claim_window(&mut self, recorded: &(String, String)) -> String {
        if let Some(claim) = self
            .claims
            .iter()
            .find(|claim| claim.profile == recorded.0 && claim.workspace == recorded.1)
        {
            return claim.token.clone();
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.claims.push(ReopenClaim {
            profile: recorded.0.clone(),
            workspace: recorded.1.clone(),
            token: token.clone(),
        });
        token
    }

    /// A matching shell completed its real Agent handshake. This is the
    /// acknowledgement boundary: process creation alone never spends intent.
    pub fn acknowledge_window(&mut self, profile: &str, workspace: &str) -> bool {
        let before = self.windows.len();
        self.windows
            .retain(|candidate| candidate.0 != profile || candidate.1 != workspace);
        self.retain_live_claims();
        self.windows.len() != before
    }

    /// Spend one window intent only when its process returns the exact claim
    /// persisted before launch.
    ///
    /// Profile/workspace identity is not enough: an ordinary duplicate
    /// `Hello`, or a late acknowledgement worker from the preceding update,
    /// can name the same pair. The v4 claim is the generation boundary between
    /// those otherwise indistinguishable handshakes.
    pub fn acknowledge_claim(&mut self, profile: &str, workspace: &str, token: &str) -> bool {
        let matches = self.claims.iter().any(|claim| {
            claim.profile == profile
                && claim.workspace == workspace
                && claim.token == token
                && valid_reopen_claim(token)
        });
        matches && self.acknowledge_window(profile, workspace)
    }

    fn retain_live_claims(&mut self) {
        self.claims.retain(|claim| {
            self.windows.iter().any(|window| {
                window.0 == claim.profile
                    && window.1 == claim.workspace
                    && valid_reopen_claim(&claim.token)
            })
        });
    }

    /// Where the list waits while the machine has no agent on it.
    ///
    /// **It has to be a file.** The process that closes the windows is not the
    /// process that reopens them — this one exits so its own executable can be
    /// replaced, and what comes back is a different run of a different build.
    /// Anything held in memory is gone by then, and the windows would simply
    /// not return.
    fn file(paths: &crate::Paths) -> std::path::PathBuf {
        paths.agent_dir().join("reopen-after-update.json")
    }

    /// The pre-W5 Koru Terminal reopen receipt. This build never writes one —
    /// koru-terminal.exe is gone — but an update *from* an older build can
    /// leave one behind, so [`Self::load`] and [`Self::clear`] both remove it
    /// rather than leaving a stray latch beside the live record.
    fn legacy_terminal_file(paths: &crate::Paths) -> std::path::PathBuf {
        paths.agent_dir().join("reopen-terminal-after-update")
    }

    pub fn has_record(paths: &crate::Paths) -> bool {
        Self::file(paths).is_file()
    }

    /// Persist the work still owed. Empty means the transition completed and
    /// removes the old record; this also lets a partly successful reopen retain
    /// only the launches that actually failed.
    pub fn save(&self, paths: &crate::Paths) -> Result<(), String> {
        use std::io::Write as _;
        let path = Self::file(paths);
        if self.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            // **The directory fsync is what makes the unlink durable**, and it
            // is `cfg(unix)` because Windows has no equivalent handle to sync.
            // A missing directory is not a failure here: the two removals above
            // already treat `NotFound` as "there was nothing to remove", and if
            // the directory itself was never created then nothing was unlinked
            // and there is nothing to make durable. Opening it unconditionally
            // turned an ordinary empty quit on a fresh root into
            // `Err("No such file or directory")` — invisible on Windows, and
            // `an_empty_list_writes_nothing` fails on every other platform.
            #[cfg(unix)]
            if let Some(parent) = path.parent() {
                match std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or("the reopen record has no parent directory")?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        let temporary = parent.join(format!(
            ".reopen-{}-{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<(), String> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| error.to_string())?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            replace_file(&temporary, &path)?;
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
            #[cfg(unix)]
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result?;
        Ok(())
    }

    /// Read without consuming. Readiness and each successful process launch
    /// remove their own work through [`Self::save`]; a failed launch remains so
    /// the next Agent can retry instead of silently losing what the update
    /// closed.
    pub fn load(paths: &crate::Paths) -> Self {
        let path = Self::file(paths);
        // An older build's Terminal receipt has nothing left to reopen;
        // consume it so it cannot linger as an unexplained latch file.
        let _ = std::fs::remove_file(Self::legacy_terminal_file(paths));
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        if let Ok(mut reopen) = serde_json::from_slice::<Self>(&bytes) {
            reopen.retain_live_claims();
            return reopen;
        }
        // Before AI chat joined the lifecycle this file was the bare window
        // vector. Accept it across the first update that changes the shape.
        match serde_json::from_slice(&bytes) {
            Ok(windows) => Self {
                windows,
                claims: Vec::new(),
                ai_chat: false,
            },
            Err(error) => {
                koru_ipc::ipc_debug!("update: the recorded reopen state was unreadable: {error}");
                Self::default()
            }
        }
    }

    /// An abandoned handoff must not turn an ordinary later Agent restart into
    /// an update restart.
    pub fn clear(paths: &crate::Paths) {
        let _ = std::fs::remove_file(Self::file(paths));
        let _ = std::fs::remove_file(Self::legacy_terminal_file(paths));
    }

    /// Legacy consume-on-read entry point retained for older restart callers.
    pub fn take(paths: &crate::Paths) -> Self {
        let reopen = Self::load(paths);
        Self::clear(paths);
        reopen
    }
}

fn valid_reopen_claim(token: &str) -> bool {
    uuid::Uuid::parse_str(token)
        .ok()
        .is_some_and(|id| id.get_version_num() == 4)
}

#[cfg(not(windows))]
fn replace_file(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    std::fs::rename(source, target).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn replace_file(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

/// A *What's new* the person is owed, waiting for somewhere to show it.
///
/// **The install is over before anyone can be told about it.** The process that
/// agreed to the update exits so its own image can be replaced; what comes back
/// is a different run of a different build, and it may come back to an empty
/// desktop — nothing reopens when nothing was open. Held in memory this would be
/// a message delivered only to people who happened to have a window up, which is
/// the same bug the `Hello` push exists to fix, in a harder form.
///
/// So it waits on disk, beside [`Reopen`], for as many launches as it takes.
///
/// **The version is recorded rather than read back later.** A launch that
/// followed an install knows which build it is; a launch three days afterwards
/// still reports the same `build_info()`, and there would be no way to tell the
/// second from the first. The file *is* the distinction.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WhatsNew {
    pub version: Option<String>,
}

impl WhatsNew {
    fn file(paths: &crate::Paths) -> std::path::PathBuf {
        paths.agent_dir().join("whats-new.json")
    }

    /// Record that this launch was an install's doing.
    ///
    /// Called with the version of the build that has just started, from the one
    /// place that can prove it: an `--update-health-token` on the command line,
    /// which only [`crates/koru-updater`] ever passes and only after a swap it
    /// committed.
    pub fn owed(paths: &crate::Paths, version: &str) {
        let path = Self::file(paths);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_vec_pretty(&version) {
            Ok(bytes) => {
                if let Err(error) = std::fs::write(&path, bytes) {
                    koru_ipc::ipc_debug!("update: could not record what's new: {error}");
                }
            }
            Err(error) => koru_ipc::ipc_debug!("update: could not encode what's new: {error}"),
        }
    }

    /// What is owed, without consuming it.
    ///
    /// Like [`Reopen`], the file outlives an attempt. The difference is the
    /// acknowledgement: reopen removes one entry after that process launches;
    /// this is cleared by [`Self::delivered`] only once a frame carrying the
    /// notice is actually on the wire. A window can connect and die inside the
    /// same turn, and an eager delete would spend the only notice on it.
    pub fn peek(paths: &crate::Paths) -> Self {
        let Ok(bytes) = std::fs::read(Self::file(paths)) else {
            return Self::default();
        };
        match serde_json::from_slice::<String>(&bytes) {
            Ok(version) => Self {
                version: Some(version),
            },
            Err(error) => {
                // Unreadable is treated as absent rather than fatal, and the
                // file is left alone: an agent must not fail to start over a
                // message it cannot deliver.
                koru_ipc::ipc_debug!("update: the recorded what's new was unreadable: {error}");
                Self::default()
            }
        }
    }

    /// Forget it, now that a window has been told.
    ///
    /// Without this the tab opens on every launch for the rest of the build's
    /// life, and there is nothing the person can do about it.
    pub fn delivered(paths: &crate::Paths) {
        let _ = std::fs::remove_file(Self::file(paths));
    }
}

/// Hold the gateway's supervision and stop the child inside the window, so
/// this image and the Store worker embedded in it can be replaced.
///
/// **A handle operation since Plan 4**, and the reason it is called from the
/// update path lives here rather than in a comment at the call site: swapping
/// the image while the child is running is how a database ends up opened by two
/// different versions of the code that owns it.
///
/// The hold comes first, or the stop races the supervisor's own respawn. It
/// leaves the supervision parked and revivable — [`resume_gateway_after_abort`]
/// is the way back — because an install that aborts must restore service rather
/// than leave it suppressed for the rest of the run.
///
/// Blocking. Called from the update handoff's own thread, never from the loop.
pub fn hold_and_stop_gateway_for_update() -> Result<crate::gateway_host::GatewayStop, String> {
    let supervisor = crate::gateway_host::current()
        .ok_or_else(|| "the Agent gateway supervisor is not running".to_owned())?;
    let report = supervisor.hold_and_stop_for_update();
    match report.store_confirmed_its_shutdown() {
        true => Ok(report),
        // **Never inferred, and it fails closed.** An update never installs
        // over a Store that has not finished its last commit, and the only
        // thing that proves it did is the child's own exit code — which it
        // sets after joining the Store worker. A kill on the deadline, a child
        // that had already died, a non-zero code and a stop that never
        // completed are each a reason to stop, not to guess. The caller aborts
        // on this, which resumes the service; the updater's write-open probe
        // then never even sees the swap.
        false => Err(report.to_string()),
    }
}

/// The aborted install's way back: leave the update window so supervision
/// replaces a child that died inside it, or revives one that was stopped.
pub fn resume_gateway_after_abort() {
    if let Some(supervisor) = crate::gateway_host::current() {
        supervisor.resume();
    }
}

/// Kill every process running this installation's own MCP bridge, and answer
/// the pids that were killed.
///
/// `koru-mcp.exe` is the one shipped executable the handoff's ordinary asks
/// cannot reach: it is launched by MCP *clients* — Codex, Claude Desktop, any
/// stdio host — never by this agent, it has no stop CLI, and its parent is a
/// process this installation knows nothing about. Left alone it holds the
/// image lock, the updater's write-open probe refuses the swap for its whole
/// 60-second window, and the install rolls back with the reason visible only
/// in `updater.log` (docs/features/auto-update/BUGS.md issue 9 — three build-63
/// installs in a row died exactly this way).
///
/// Killing it is safe because of what the bridge *is*: a stateless stdio
/// relay. Every durable fact it touches lives in the store, which has already
/// been asked to stop by the time this runs; the client sees a closed pipe —
/// the same thing it sees when the bridge exits for any other reason — and
/// respawns the bridge on its next call, getting the new build
/// ([ADR-164](../../../docs/decisions/README.md)).
///
/// **Only images inside `install_dir` are touched.** A `koru-mcp` running
/// from a dev tree belongs to a different installation and blocks nothing
/// here, so matching on the name alone would kill a process this update does
/// not own. The comparison is the full image path, case-insensitively,
/// because that is how Windows compares them.
#[cfg(windows)]
pub fn kill_mcp_bridges(install_dir: &std::path::Path) -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess,
    };

    let target = install_dir.join(koru_update::MCP_EXE);
    let target = target.to_string_lossy();
    let mut killed = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return killed;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            // The name is only a pre-filter; the image path decides.
            if name.eq_ignore_ascii_case(koru_update::MCP_EXE) {
                let handle = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    entry.th32ProcessID,
                );
                if !handle.is_null() {
                    let mut buffer = [0u16; 1024];
                    let mut length = buffer.len() as u32;
                    if QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) != 0
                    {
                        let image = String::from_utf16_lossy(&buffer[..length as usize]);
                        if image.eq_ignore_ascii_case(&target) && TerminateProcess(handle, 1) != 0 {
                            killed.push(entry.th32ProcessID);
                        }
                    }
                    CloseHandle(handle);
                }
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    killed
}

/// The macOS half of the same decision. The image lock is not the obstacle
/// there — macOS renames a running image without a murmur — but the updater's
/// quiesce proof is *process absence under the bundle*, so a client-held
/// bridge blocks the install just the same. `pgrep -f` over the image path is
/// the shape [`crates/koru-updater`]'s `bundle_is_quiet` already uses.
#[cfg(target_os = "macos")]
pub fn kill_mcp_bridges(install_dir: &std::path::Path) -> Vec<u32> {
    let target = install_dir.join(koru_update::MCP_EXE);
    let pattern = target.to_string_lossy();
    let Ok(output) = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(pattern.as_ref())
        .output()
    else {
        return Vec::new();
    };
    let mut killed = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(pid) = line.trim().parse::<u32>() else {
            continue;
        };
        // SIGKILL for the same reason Windows uses TerminateProcess: the
        // point of this step is certainty, and a bridge has nothing to save.
        if unsafe { libc::kill(pid as i32, libc::SIGKILL) } == 0 {
            killed.push(pid);
        }
    }
    killed
}

/// The Linux half: the updater's quiesce proof there is process absence too,
/// read from the same table — `koru_update::processes::running_from` — so a
/// bridge is found exactly the way it would block the install: by the image
/// the kernel says it is running, which is this installation's `koru-mcp` and
/// nobody else's.
#[cfg(target_os = "linux")]
pub fn kill_mcp_bridges(install_dir: &std::path::Path) -> Vec<u32> {
    let Ok(target) = std::fs::canonicalize(install_dir.join(koru_update::MCP_EXE)) else {
        return Vec::new();
    };
    let Ok(running) = koru_update::processes::running_from(install_dir) else {
        return Vec::new();
    };
    let mut killed = Vec::new();
    for (pid, image) in running {
        if image != target {
            continue;
        }
        // SIGKILL, as on macOS: certainty is the point, and a bridge has
        // nothing to save.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } == 0 {
            killed.push(pid);
        }
    }
    killed
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn kill_mcp_bridges(_install_dir: &std::path::Path) -> Vec<u32> {
    Vec::new()
}

/// Offer an update to somebody who has no Koru window open.
///
/// **The fallback, not the usual path.** When a window is open the report is
/// pushed to it and the app's banner shows the release notes properly — a title
/// and its summary lines, laid out. A message box cannot do that, so this shows
/// the title and the summary joined and puts the rest behind *What's new*,
/// which opens the notes page in a browser.
///
/// The same shape as [`crate::service::offer_store_restart`]: a thread, because
/// a modal blocks until it is answered and the window loop may never block; and
/// an [`crate::AgentEvent::UpdateOfferHandled`] on the way out, so the caller
/// can let the next offer through without two dialogs racing.
/// What the no-window dialog says, for a report at either consent stage.
///
/// **Pure, and separated from the modal for the reason the rest of this module
/// is: the decisions are testable and the performing is not.** What these two
/// dialogs say is a product decision that was got wrong once and would be got
/// wrong again silently, because nothing that draws an `rfd` can be asserted on.
///
/// **What changed is said once, at the first question.** The two are one
/// conversation — *this exists, fetch it?* and then *it is here, install it?* —
/// and repeating the release notes at the second asks somebody to read again
/// what they read a moment ago, to answer a question that is no longer about the
/// content. By then the only thing being decided is whether to be interrupted
/// now. Reported from a real update, in those words.
///
/// The in-window banner already worked this way, which is why the fix was in one
/// place rather than two: `banner_copy`'s `Ready` arm carries the restart
/// sentence and nothing else.
pub fn dialog_copy(report: &UpdateReport) -> (String, String) {
    let downloading = report.stage == UpdateStage::Available;
    let (title, action) = if downloading {
        ("A new Koru is available", "Download it now?")
    } else {
        ("A new Koru is ready", "Install it now?")
    };
    let mut description = String::new();
    if downloading {
        description.push_str(&report.title);
        for line in &report.summary {
            description.push('\n');
            description.push_str(line);
        }
        description.push_str("\n\n");
    }
    description.push_str(action);
    if !downloading {
        description.push_str("\nKoru will close and restart to install it.");
        if report.active_session_count > 0 {
            let lang =
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
            description.push('\n');
            description.push_str(&koru_i18n::fill(
                koru_i18n::t(lang, koru_i18n::Key::UpdateReadyWithSessions),
                &[&report.active_session_count.to_string()],
            ));
        }
    }
    (format!("{title} — {}", report.version), description)
}

fn quit_sessions_copy(active_count: u32) -> (String, String, koru_i18n::Lang) {
    let lang = koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
    let count = active_count.to_string();
    (
        koru_i18n::t(lang, koru_i18n::Key::AgentQuitSessionsTitle).into(),
        koru_i18n::fill(
            koru_i18n::t(lang, koru_i18n::Key::AgentQuitSessionsBody),
            &[&count],
        ),
        lang,
    )
}

/// Ask before an ordinary tray quit terminates active terminal processes. This
/// runs on the dedicated quit worker, never the Agent event loop.
#[cfg(windows)]
pub fn confirm_agent_quit(active_count: u32) -> bool {
    let (title, body, _) = quit_sessions_copy(active_count);
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(&title)
        .set_description(&body)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}

#[cfg(target_os = "macos")]
pub fn confirm_agent_quit(active_count: u32) -> bool {
    let (title, body, lang) = quit_sessions_copy(active_count);
    let script = quit_dialog_script(&title, &body, lang);
    std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn confirm_agent_quit(active_count: u32) -> bool {
    let (title, body, _) = quit_sessions_copy(active_count);
    eprintln!("{title}\n{body}");
    false
}

#[cfg(any(target_os = "macos", test))]
fn quit_dialog_script(title: &str, body: &str, lang: koru_i18n::Lang) -> String {
    fn literal(text: &str) -> String {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    }
    let cancel = koru_i18n::t(lang, koru_i18n::Key::TerminalCancel);
    let quit = koru_i18n::t(lang, koru_i18n::Key::AgentQuitConfirm);
    format!(
        "display dialog {} with title {} buttons {{{}, {}}} default button {} cancel button {} with icon caution",
        literal(body),
        literal(title),
        literal(cancel),
        literal(quit),
        literal(quit),
        literal(cancel),
    )
}

#[cfg(windows)]
pub fn offer_in_a_dialog(report: UpdateReport, waker: crate::event::Waker) {
    let handled = waker.clone();
    if std::thread::Builder::new()
        .name("koru-agent update offer".into())
        .spawn(move || {
            let downloading = report.stage == UpdateStage::Available;
            let (title, description) = dialog_copy(&report);
            let answered = rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Info)
                .set_title(&title)
                .set_description(&description)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes;
            if answered {
                let choice = if downloading {
                    UpdateChoice::Download
                } else {
                    UpdateChoice::Install {
                        session_generation: report.session_generation,
                    }
                };
                waker.wake(crate::event::AgentEvent::UpdateChoiceMade { choice });
            } else {
                waker.wake(crate::event::AgentEvent::UpdateChoiceMade {
                    choice: UpdateChoice::Later,
                });
            }
            waker.wake(crate::event::AgentEvent::UpdateOfferHandled);
        })
        .is_err()
    {
        koru_ipc::ipc_debug!("update: could not start the offer thread");
        handled.wake(crate::event::AgentEvent::UpdateOfferHandled);
    }
}

/// The same offer on macOS, through `osascript`'s `display dialog`. `rfd` is
/// not used here because AppKit's `NSAlert` must run on the main thread and
/// the agent's main thread is the window loop, which may never block on a
/// modal; a child process has a main thread of its own. The answer comes back
/// as `button returned:…` on stdout, and a dismissed dialog (Escape, or the
/// person quitting the prompt) exits non-zero, which reads as *Later*.
#[cfg(target_os = "macos")]
pub fn offer_in_a_dialog(report: UpdateReport, waker: crate::event::Waker) {
    let handled = waker.clone();
    if std::thread::Builder::new()
        .name("koru-agent update offer".into())
        .spawn(move || {
            let downloading = report.stage == UpdateStage::Available;
            let (title, description) = dialog_copy(&report);
            let script = display_dialog_script(&title, &description);
            let answered = std::process::Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg(&script)
                .output()
                .map(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).contains("button returned:Yes")
                })
                .unwrap_or(false);
            let choice = match (answered, downloading) {
                (true, true) => UpdateChoice::Download,
                (true, false) => UpdateChoice::Install {
                    session_generation: report.session_generation,
                },
                (false, _) => UpdateChoice::Later,
            };
            waker.wake(crate::event::AgentEvent::UpdateChoiceMade { choice });
            waker.wake(crate::event::AgentEvent::UpdateOfferHandled);
        })
        .is_err()
    {
        koru_ipc::ipc_debug!("update: could not start the offer thread");
        handled.wake(crate::event::AgentEvent::UpdateOfferHandled);
    }
}

/// The AppleScript for one Yes/Later question. Pure, so the quoting is
/// testable: a release title containing `"` or `\` must not end the string
/// literal early, and nothing from a manifest may become script.
#[cfg(any(target_os = "macos", test))]
pub fn display_dialog_script(title: &str, description: &str) -> String {
    fn literal(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }
    format!(
        "display dialog {} with title {} buttons {{\"Later\", \"Yes\"}} default button \"Yes\" with icon note",
        literal(description),
        literal(title)
    )
}

/// A one-button notice for an install that could not proceed. The abort used
/// to be recorded only where a window's banner could show it — and the install
/// flow had just *closed* every window, so "Yes" produced a silent restore
/// and the person reasonably asked why nothing was happening.
#[cfg(target_os = "macos")]
pub fn announce_install_blocked(why: String) {
    let _ = std::thread::Builder::new()
        .name("koru-agent install blocked notice".into())
        .spawn(move || {
            let script = display_notice_script(
                "Koru could not install the update",
                &format!("{why}\n\nEverything was restored; nothing was changed."),
            );
            let _ = std::process::Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg(&script)
                .output();
        });
}

#[cfg(target_os = "macos")]
pub fn display_notice_script(title: &str, description: &str) -> String {
    fn literal(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }
    format!(
        "display dialog {} with title {} buttons {{\"OK\"}} default button \"OK\" with icon caution",
        literal(description),
        literal(title)
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn offer_in_a_dialog(report: UpdateReport, waker: crate::event::Waker) {
    eprintln!(
        "A new Koru is available: {} — {}",
        report.version, report.title
    );
    waker.wake(crate::event::AgentEvent::UpdateOfferHandled);
}

/// Hand the package to `koru-updater.exe` and let it take the installation.
///
/// Moved from `koru_workspace_host::App::launch_updater`, with three differences and no
/// other changes: there is no workspace to save first (the windows saved on
/// their way out), the restart target is **this** executable rather than
/// `koru.exe`, and the pid to wait for is this process's.
///
/// The helper is copied to a temporary directory before it runs, and its hash is
/// checked before *and* after the copy against the one compiled into this
/// binary — an updater that has been swapped underneath us is the one thing that
/// could quietly install something else.
pub fn launch_updater(
    pending: PendingInstall,
    paths: koru_update::UpdatePaths,
    root: Option<std::path::PathBuf>,
    rehearsal_manifest: Option<&std::path::Path>,
) -> Result<(), String> {
    let info = *koru_update::build_info();
    if !info.updates_enabled() {
        return Err("this development build cannot install updates".into());
    }
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let install_dir = current
        .parent()
        .ok_or("the current executable has no installation directory")?
        .to_path_buf();
    if root
        .as_deref()
        .is_some_and(|root| !koru_update::UpdatePaths::root_owns_install(root, &install_dir))
    {
        return Err("a rooted update may replace only a copied bundle beneath its root".into());
    }
    if root
        .as_deref()
        .is_some_and(|root| paths != koru_update::UpdatePaths::under(root))
    {
        return Err("rooted update paths do not match the selected root".into());
    }
    if !koru_update::is_writable_install(&install_dir) {
        return Err(format!(
            "{} is not writable; download the release manually",
            install_dir.display()
        ));
    }
    let helper_source = install_dir.join(koru_update::UPDATER_EXE);
    let (_, helper_hash) =
        koru_update::sha256_file(&helper_source).map_err(|error| error.to_string())?;
    if helper_hash != info.updater_sha256 {
        return Err("the installed updater helper failed its embedded hash check".into());
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    let helper_dir = match &root {
        Some(_) => paths
            .staging_dir()
            .join(format!("helper-{}-{token}", info.updater_protocol_version)),
        None => std::env::temp_dir()
            .join("KoruUpdater")
            .join(info.updater_protocol_version.to_string())
            .join(&token),
    };
    std::fs::create_dir_all(&helper_dir).map_err(|error| error.to_string())?;
    let helper = helper_dir.join(koru_update::UPDATER_EXE);
    std::fs::copy(&helper_source, &helper).map_err(|error| error.to_string())?;
    let (_, copied_hash) = koru_update::sha256_file(&helper).map_err(|error| error.to_string())?;
    if copied_hash != info.updater_sha256 {
        return Err("the temporary updater helper failed verification".into());
    }
    let rehearsal_root = rehearsal_manifest.is_some().then(|| root.clone()).flatten();
    let request = koru_update::UpdaterRequest {
        package_size: pending
            .package
            .metadata()
            .map_err(|error| error.to_string())?
            .len(),
        package: pending.package,
        install_dir,
        // **This executable, not koru.exe.** The agent is what came down last
        // and what has to come back first: it restores the store and reopens
        // the windows it closed. Restarting a shell instead would leave the
        // machine with no tray icon and no global shortcut until somebody
        // opened a window.
        restart: current,
        current_build: info.build_number,
        target_build: pending.manifest.build_number,
        expected_sha256: pending.manifest.package.sha256,
        signature: pending.manifest.package.signature,
        health_token: token,
        root,
        rehearsal_root,
        rehearsal_manifest: rehearsal_manifest.map(std::path::Path::to_path_buf),
    };
    let request_path = helper_dir.join("request.json");
    let bytes = serde_json::to_vec_pretty(&request).map_err(|error| error.to_string())?;
    std::fs::write(&request_path, bytes).map_err(|error| error.to_string())?;
    std::process::Command::new(helper)
        .arg("--request")
        .arg(request_path)
        .arg("--wait-pid")
        .arg(std::process::id().to_string())
        .spawn()
        .map_err(|error| format!("could not start updater: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
