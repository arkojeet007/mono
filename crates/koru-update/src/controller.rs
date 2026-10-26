// SPDX-License-Identifier: MPL-2.0

use reqwest::header::CONTENT_LENGTH;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::{
    CachedPackage, CheckReason, FetchResult, ReleaseChannel, UpdateClient, UpdateFailure,
    UpdateManifest, UpdatePaths, UpdateState, UpdateStatus, UpdateTarget, build_info,
    verify_package,
};

/// How long after start the first automatic check waits. Long enough that it
/// is never competing with a window coming up.
const STARTUP_DELAY: Duration = Duration::from_secs(5);

/// How long between automatic checks after the first.
///
/// **The cadence follows the channel, because that is what a channel is.** This
/// was a flat twenty-four hours, chosen when the only question was "a shell runs
/// for hours, an agent runs for days" — which is true and was not the whole
/// question. A channel is a release *rate*: nightly can ship several times in a
/// working day, and a daily poll means most of those are never seen by a machine
/// that is already running. It was reported exactly that way — a release
/// published, nothing said, and *Check for updates* clicked by hand.
///
/// **None of these is "right away", and nothing here can be.** This is a poll
/// against a static manifest on R2; there is no channel from the feed to the
/// machine. The interval is the honest upper bound on how stale an offer can be,
/// and the number to move if that bound is wrong.
///
/// One sleeping thread and, at worst, twenty-four requests a day.
fn automatic_interval(channel: ReleaseChannel) -> Duration {
    let hours = match channel {
        // Several builds a day, and an audience that wants them.
        ReleaseChannel::Nightly => 1,
        // A few a week.
        ReleaseChannel::Beta => 6,
        // Stable, and the internal builds that have no feed at all and whose
        // check is refused before it reaches the network.
        _ => 24,
    };
    Duration::from_secs(hours * 60 * 60)
}

/// Wakes whoever is going to call [`UpdateController::tick`].
type Notifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInstall {
    pub manifest: UpdateManifest,
    pub package: PathBuf,
}

#[derive(Clone)]
pub struct UpdateController {
    shared: Arc<Mutex<Shared>>,
}

struct Shared {
    base_url: String,
    /// Package beside a local rehearsal manifest. When present, Download copies
    /// and verifies this file and never constructs an HTTP client.
    rehearsal_package: Option<PathBuf>,
    paths: UpdatePaths,
    state: UpdateState,
    /// Always `ReleaseChannel::of_this_build()`. It is assigned once, in
    /// `new`, and never again — *only ever the same channel*, said in the type
    /// rather than trusted in a comment. There is no code path left that can
    /// point this controller at another channel's feed: switching channels is
    /// done by downloading that channel's zip, and no build ever up- or
    /// downgrades itself.
    channel: ReleaseChannel,
    session_deferred: Option<u64>,
    /// Session-only: user dismissed the "installed build is newer" notice.
    /// Only a restart shows it again.
    session_newer_dismissed: bool,
    status: UpdateStatus,
    worker: Option<Receiver<WorkerEvent>>,
    cancel: Arc<AtomicBool>,
    notify: Option<Notifier>,
}

/// A worker's channel to the controller, with the wake attached.
///
/// **Send first, wake second, always.** The reader is asleep in an event loop
/// and `tick` is a non-blocking drain, so a wake that arrives before the event
/// is queued is a wake that drains nothing — and the event then sits in the
/// queue with nobody left to wake for it, until some unrelated input happens to
/// come along. That is the classic form of this bug and it looks like an update
/// that only appears when you move the mouse. Every send goes through here so
/// there is one place the order can be got right.
struct Events {
    tx: Sender<WorkerEvent>,
    notify: Option<Notifier>,
}

impl Events {
    fn send(&self, event: WorkerEvent) {
        let _ = self.tx.send(event);
        if let Some(notify) = &self.notify {
            notify();
        }
    }
}

enum WorkerEvent {
    CheckDone {
        reason: CheckReason,
        result: Result<FetchResult, String>,
        etag_key: String,
    },
    Progress {
        received: u64,
        total: u64,
    },
    DownloadDone {
        manifest: UpdateManifest,
        result: Result<PathBuf, String>,
    },
}

impl UpdateController {
    pub fn discover(base_url: impl Into<String>) -> Option<Self> {
        Self::new(base_url, UpdatePaths::discover()?).ok()
    }

    pub fn new(base_url: impl Into<String>, paths: UpdatePaths) -> Result<Self, String> {
        paths.ensure().map_err(|e| e.to_string())?;
        let mut state = UpdateState::load(&paths.state_file());
        state.clear_etags();
        let mut shared = Shared {
            base_url: base_url.into(),
            rehearsal_package: None,
            paths,
            state,
            // Never `state.selected_channel`. See `Shared::channel`.
            channel: ReleaseChannel::of_this_build(),
            session_deferred: None,
            session_newer_dismissed: false,
            status: UpdateStatus::Idle,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            notify: None,
        };
        Self::offer_what_is_already_downloaded(&mut shared);
        Ok(Self {
            shared: Arc::new(Mutex::new(shared)),
        })
    }

    /// A normal signed manifest and package on local disk, offered through the
    /// same two-consent state machine without automatic polling or networking.
    pub fn rehearsal(manifest_path: &std::path::Path, paths: UpdatePaths) -> Result<Self, String> {
        paths.ensure().map_err(|e| e.to_string())?;
        let info = build_info();
        let target = UpdateTarget {
            channel: ReleaseChannel::of_this_build(),
            platform: info.platform.to_string(),
            architecture: info.architecture.to_string(),
        };
        let bytes = fs::read(manifest_path)
            .map_err(|e| format!("could not read local update manifest: {e}"))?;
        let manifest =
            UpdateManifest::parse_and_validate(&bytes, &target).map_err(|e| e.to_string())?;
        if manifest.build_number <= info.build_number {
            return Err("the rehearsal update must be newer than the installed build".into());
        }
        let package = manifest_path
            .parent()
            .ok_or("the rehearsal manifest has no parent directory")?
            .join(&manifest.package.file_name);
        if !package.is_file() {
            return Err(format!(
                "the rehearsal package is missing beside its manifest: {}",
                package.display()
            ));
        }
        let mut state = UpdateState::load(&paths.state_file());
        state.clear_etags();
        state.last_manifest = Some(manifest.clone());
        state.last_seen_build = Some(manifest.build_number);
        let shared = Shared {
            base_url: "https://local.invalid".into(),
            rehearsal_package: Some(package),
            paths,
            state,
            channel: target.channel,
            session_deferred: None,
            session_newer_dismissed: false,
            status: UpdateStatus::Available(manifest),
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            notify: None,
        };
        let _ = shared.state.save(&shared.paths.state_file());
        Ok(Self {
            shared: Arc::new(Mutex::new(shared)),
        })
    }

    /// **Ask the disk before asking the server.** A package that has already
    /// been downloaded and verified is an offer this machine can make on its
    /// own, and it makes it before any network call — so a start with no
    /// connection, or against an origin having a bad day, still lands on
    /// *ready to install* rather than on silence.
    ///
    /// It is also the honest place for the recovery: an interrupted install
    /// leaves a verified zip in the cache, and needing a *round trip* to
    /// remember that was the fragility behind ADR-151. A conditional request
    /// answering "nothing changed" then decided a question it was never asked.
    ///
    /// Cheap on purpose — the file's existence, not its hash. Sixty megabytes
    /// of SHA-256 on every start would be paid by everyone to protect a rare
    /// case, and it protects nothing anyway: `koru-updater` verifies the
    /// signature again immediately before it swaps a single file, which is the
    /// check that has to be right and is the one place that can be sure.
    fn offer_what_is_already_downloaded(shared: &mut Shared) {
        let Some(cached) = shared.state.cached_verified_package.clone() else {
            return;
        };
        if cached.build_number <= build_info().build_number || !cached.path.is_file() {
            return;
        }
        // The manifest is what a banner shows — the title, the notes link, the
        // version string. Without the one this package came from there is
        // nothing to draw, so this stays quiet rather than inventing one.
        let Some(manifest) = shared.state.last_manifest.clone() else {
            return;
        };
        if manifest.build_number != cached.build_number {
            return;
        }
        // **And it has to be this channel's build.** Build numbers come from
        // one counter across every channel, so "newer" is a comparison that
        // says nothing about which product line a package belongs to: a
        // nightly's 40 is simply larger than a stable's 20. The directories are
        // scoped per channel now and this should be unreachable, which is
        // exactly why it is checked — the cost of being wrong here is a stable
        // install being handed a nightly.
        if manifest.channel != shared.channel {
            return;
        }
        // `Startup`, not `Manual`: this is not somebody asking, so it lifts
        // neither a deferral nor a failed-build hold. `settle_on` re-checks the
        // cache, which is what makes this `ReadyToInstall` rather than
        // `Available`.
        Self::settle_on(shared, CheckReason::Startup, manifest);
    }

    /// Install the wake. A host whose loop is `ControlFlow::Wait` at zero CPU —
    /// the agent — has no reason of its own ever to call [`tick`](Self::tick),
    /// so the controller has to be able to wake somebody rather than wait to be
    /// asked. Without one, nothing breaks and nothing arrives until the host
    /// wakes for another reason.
    pub fn set_notifier(&self, notify: impl Fn() + Send + Sync + 'static) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.notify = Some(Arc::new(notify));
    }

    /// Start the background check loop: one wait, then one check a day.
    ///
    /// Calling it twice starts two threads, so call it once, at start.
    pub fn start_automatic(&self) {
        // A `cargo test` run that reaches `https://updates.koru.do/` is the
        // worst thing this file could do, and nothing about a unit test would
        // reveal it had happened. `cfg!` rather than `#[cfg]` so the loop below
        // is still compiled and still type-checked under test.
        if cfg!(test) {
            return;
        }
        let controller = self.clone();
        let _ = thread::Builder::new()
            .name("koru-update-automatic".into())
            .spawn(move || {
                thread::sleep(STARTUP_DELAY);
                // Read every round rather than once — including the channel,
                // which cannot change under a running agent but costs nothing
                // to ask and keeps the interval beside the check it paces.
                loop {
                    // Turning automatic checks off takes effect at the next
                    // wake, and turning them back on does not need a restart.
                    if controller.automatic_checks() {
                        controller.check(CheckReason::Startup);
                    }
                    thread::sleep(automatic_interval(controller.channel()));
                }
            });
    }

    /// The channel this controller checks, which is this build's and no other.
    pub fn channel(&self) -> ReleaseChannel {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .channel
    }

    pub fn automatic_checks(&self) -> bool {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state
            .automatic_checks
    }

    pub fn set_automatic_checks(&self, on: bool) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.state.automatic_checks = on;
        let _ = shared.state.save(&shared.paths.state_file());
    }

    /// Drain whatever the workers have finished. Non-blocking, and the only
    /// thing the notifier's wake has to lead to; `true` means something a host
    /// draws changed.
    pub fn tick(&self) -> bool {
        let mut changed = false;
        loop {
            let event = {
                let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                let Some(worker) = &shared.worker else {
                    break;
                };
                match worker.try_recv() {
                    Ok(event) => Some(event),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        shared.worker = None;
                        if matches!(
                            shared.status,
                            UpdateStatus::Checking
                                | UpdateStatus::Downloading { .. }
                                | UpdateStatus::Verifying
                        ) {
                            shared.status = UpdateStatus::Failed(UpdateFailure::new(
                                "The update operation stopped unexpectedly.",
                                "background update worker disconnected",
                                true,
                            ));
                            changed = true;
                        }
                        None
                    }
                }
            };
            let Some(event) = event else {
                break;
            };
            changed |= self.apply(event);
        }
        changed
    }

    pub fn status(&self) -> UpdateStatus {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    pub fn paths(&self) -> UpdatePaths {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .paths
            .clone()
    }

    pub fn install_blocked(&self, detail: impl Into<String>) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.status = UpdateStatus::Failed(UpdateFailure::new(
            "Close the other Koru windows before updating.",
            detail,
            true,
        ));
    }

    pub fn check(&self, reason: CheckReason) {
        let (base_url, target, etag, etag_key, events) = {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if shared.worker.is_some() {
                return;
            }
            if shared.rehearsal_package.is_some() {
                if let Some(manifest) = shared.state.last_manifest.clone() {
                    Self::settle_on(&mut shared, reason, manifest);
                } else {
                    shared.status = UpdateStatus::Failed(UpdateFailure::new(
                        "The local update manifest is unavailable.",
                        "rehearsal controller lost its manifest",
                        false,
                    ));
                }
                return;
            }
            // Internal is a developer flavor only — there is no CDN feed.
            if !shared.channel.has_update_feed() {
                shared.status = UpdateStatus::UpToDate;
                return;
            }
            let info = build_info();
            let target = UpdateTarget {
                channel: shared.channel,
                platform: info.platform.to_string(),
                architecture: info.architecture.to_string(),
            };
            let etag_key = etag_key(&shared.base_url, &target);
            let etag = shared.state.etag(&etag_key).map(str::to_owned);
            let (tx, rx) = mpsc::channel();
            let events = Events {
                tx,
                notify: shared.notify.clone(),
            };
            shared.worker = Some(rx);
            shared.status = UpdateStatus::Checking;
            (shared.base_url.clone(), target, etag, etag_key, events)
        };
        let info = *build_info();
        let _ = thread::Builder::new()
            .name("koru-update-check".into())
            .spawn(move || {
                let agent = format!("Koru/{} (build {})", info.version, info.build_number);
                let result = UpdateClient::new(&base_url, &agent)
                    .and_then(|client| client.fetch_manifest(&target, etag.as_deref()));
                events.send(WorkerEvent::CheckDone {
                    reason,
                    result,
                    etag_key,
                });
            });
    }

    pub fn download(&self, manifest: UpdateManifest) {
        let (paths, public_key, cancel, events, rehearsal_package) = {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if shared.worker.is_some() {
                return;
            }
            shared.cancel.store(false, Ordering::Release);
            let (tx, rx) = mpsc::channel();
            let events = Events {
                tx,
                notify: shared.notify.clone(),
            };
            shared.worker = Some(rx);
            shared.status = UpdateStatus::Downloading {
                received: 0,
                total: manifest.package.size,
            };
            (
                shared.paths.clone(),
                build_info().update_public_key.to_string(),
                Arc::clone(&shared.cancel),
                events,
                shared.rehearsal_package.clone(),
            )
        };
        let _ = thread::Builder::new()
            .name("koru-update-download".into())
            .spawn(move || {
                let result = match rehearsal_package {
                    Some(source) => {
                        copy_verified(&source, &manifest, &paths, &public_key, &cancel, &events)
                    }
                    None => download_verified(&manifest, &paths, &public_key, &cancel, &events),
                };
                events.send(WorkerEvent::DownloadDone { manifest, result });
            });
    }

    pub fn cancel_download(&self) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel
            .store(true, Ordering::Release);
    }

    pub fn defer(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let build = match &shared.status {
            UpdateStatus::Available(manifest) => manifest.build_number,
            UpdateStatus::ReadyToInstall { manifest, .. } => manifest.build_number,
            _ => return,
        };
        shared.session_deferred = Some(build);
        shared.state.deferred_build = Some(build);
        shared.status = UpdateStatus::Deferred(build);
        let _ = shared.state.save(&shared.paths.state_file());
    }

    /// Dismiss the "installed build is newer than the selected channel" notice
    /// for this session. Does not persist — a restart shows it again if the
    /// situation still holds.
    pub fn dismiss_installed_newer(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(shared.status, UpdateStatus::InstalledBuildNewer { .. }) {
            shared.session_newer_dismissed = true;
            shared.status = UpdateStatus::Idle;
        }
    }

    pub fn take_pending_install(&self) -> Option<PendingInstall> {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let UpdateStatus::ReadyToInstall { manifest, package } = &shared.status else {
            return None;
        };
        let pending = PendingInstall {
            manifest: manifest.clone(),
            package: package.clone(),
        };
        shared.status = UpdateStatus::Installing;
        Some(pending)
    }

    /// Turn a manifest — freshly fetched, or the one a 304 says still
    /// stands — into the status the banner shows.
    ///
    /// Shared by both arms of a check on purpose. It used to live inside the
    /// 200 arm, so the 304 arm had no way to reach the `cached_verified_package`
    /// branch below and answered `UpToDate` instead.
    fn settle_on(shared: &mut Shared, reason: CheckReason, manifest: UpdateManifest) {
        shared.state.last_seen_build = Some(manifest.build_number);
        let installed = build_info().build_number;
        let failed = shared
            .state
            .failed_build
            .as_ref()
            .is_some_and(|failed| failed.build_number == manifest.build_number);
        // A person who asks "Check for updates" wants the answer even for a
        // build they said Later to an hour ago — so a manual check lifts both
        // the deferral and the failed-build hold; an automatic one honours them.
        let manual = matches!(reason, CheckReason::Manual);
        let deferred = shared.session_deferred == Some(manifest.build_number);
        if manual && deferred {
            shared.session_deferred = None;
        }
        if manifest.build_number > installed && (!deferred || manual) && (!failed || manual) {
            if let Some(cached) = &shared.state.cached_verified_package
                && cached.build_number == manifest.build_number
                && cached.path.is_file()
            {
                shared.status = UpdateStatus::ReadyToInstall {
                    manifest,
                    package: cached.path.clone(),
                };
            } else {
                shared.status = UpdateStatus::Available(manifest);
            }
        } else if manifest.build_number < installed {
            if shared.session_newer_dismissed {
                shared.status = UpdateStatus::Idle;
            } else {
                shared.status = UpdateStatus::InstalledBuildNewer {
                    available: manifest.build_number,
                };
            }
        } else {
            shared.status = UpdateStatus::UpToDate;
        }
    }

    fn apply(&self, event: WorkerEvent) -> bool {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        match event {
            WorkerEvent::CheckDone {
                reason,
                result,
                etag_key,
            } => {
                shared.worker = None;
                shared.state.last_check_at = Some(unix_timestamp());
                match result {
                    // **A 304 answers with what a 200 would have.** It means
                    // the manifest has not changed since the ETag we sent —
                    // not that this build is up to date, which is what it used
                    // to be read as. That reading is unrecoverable rather than
                    // merely wrong: the ETag is sent by manual checks too, so
                    // once the status had been lost to a failed install, every
                    // *Check for updates* answered "up to date" while a
                    // verified newer package sat in the cache, and only
                    // publishing a further build could clear it.
                    Ok(FetchResult::NotModified) => match shared.state.last_manifest.clone() {
                        Some(manifest) => Self::settle_on(&mut shared, reason, manifest),
                        // Nothing was ever received under this ETag, so there
                        // is nothing to re-derive from. Only reachable from a
                        // state file written before `last_manifest` existed.
                        None => shared.status = UpdateStatus::UpToDate,
                    },
                    Ok(FetchResult::Manifest { manifest, etag }) => {
                        if let Some(etag) = etag {
                            shared.state.set_etag(etag_key, etag);
                        }
                        shared.state.last_manifest = Some(manifest.clone());
                        Self::settle_on(&mut shared, reason, manifest);
                    }
                    Err(detail) => {
                        shared.status = UpdateStatus::Failed(UpdateFailure::new(
                            "Koru could not check for updates.",
                            detail,
                            true,
                        ));
                    }
                }
                let _ = shared.state.save(&shared.paths.state_file());
            }
            WorkerEvent::Progress { received, total } => {
                shared.status = UpdateStatus::Downloading { received, total };
            }
            WorkerEvent::DownloadDone { manifest, result } => {
                shared.worker = None;
                match result {
                    Ok(package) => {
                        shared.state.cached_verified_package = Some(CachedPackage {
                            build_number: manifest.build_number,
                            path: package.clone(),
                            sha256: manifest.package.sha256.clone(),
                        });
                        prune_verified(&shared.paths.verified_dir(), Some(&package));
                        shared.status = UpdateStatus::ReadyToInstall { manifest, package };
                    }
                    Err(detail) if detail == "canceled" => {
                        shared.status = UpdateStatus::Available(manifest);
                    }
                    Err(detail) => {
                        shared.status = UpdateStatus::Failed(UpdateFailure::new(
                            "The update could not be downloaded or verified.",
                            detail,
                            true,
                        ));
                    }
                }
                let _ = shared.state.save(&shared.paths.state_file());
            }
        }
        true
    }
}

/// Keep the two newest packages in `verified/` and delete the rest.
///
/// **Nothing pruned this, ever.** Each update leaves ~60 MB behind, so a
/// machine that had followed nightly since the first build was holding 658 MB
/// across sixteen zips going back six weeks — none of which anything could
/// ever install, because only a build *newer* than this one is offered.
///
/// Two rather than one: the newest is what is about to be installed, and the
/// one behind it is what a rollback or a second attempt reaches for. Two is
/// also what makes this safe to run the moment a download lands, which is the
/// only moment the directory is known to have grown.
///
/// **Ordered by build number, taken from the file name** — `40-koru-...zip`,
/// written by [`download_verified`]. A name that does not start with a build
/// number is left alone: this deletes things it put there and recognises, not
/// everything it finds in a directory.
///
/// One caveat worth knowing: `verified/` is under `cache_dir`, which is
/// **shared by every channel** — there is no `<channel>` in the path, only in
/// the store's and the agent's. Build numbers come from one counter across all
/// channels, so the ordering is still meaningful, but a nightly that prunes can
/// drop a stable build's cached package. The cost is a re-download, never a
/// wrong install, because the file name carries the build it is.
///
/// Best effort throughout. A file that will not delete — locked by a virus
/// scanner, open in another process — is skipped, because failing to tidy is
/// not a reason to fail an update that has otherwise succeeded.
/// What a manifest's ETag is remembered under: the URL it was fetched from.
///
/// It was the relative manifest path, which named one resource while every
/// origin shared one layout. Now the path depends on the origin
/// ([`crate::manifest::FeedLayout`]), and a tag is only meaningful to the server
/// that issued it, so the key is the whole URL. A base that does not parse
/// falls back to the path; the check that follows fails on the same parse.
fn etag_key(base_url: &str, target: &UpdateTarget) -> String {
    Url::parse(base_url)
        .ok()
        .and_then(|base| target.manifest_url(&base).ok())
        .map_or_else(|| target.manifest_path(), String::from)
}

fn prune_verified(dir: &std::path::Path, keep: Option<&std::path::Path>) {
    const KEEP: usize = 2;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut packages: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let build = name.split_once('-')?.0.parse::<u64>().ok()?;
            Some((build, path))
        })
        .collect();
    if packages.len() <= KEEP {
        return;
    }
    packages.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in packages.into_iter().skip(KEEP) {
        // Never the one this update is about to hand to the updater, whatever
        // the ordering says. The two can only disagree if something else wrote
        // a higher-numbered package while this one downloaded, and losing the
        // package we are holding would be the one failure that matters.
        if keep.is_some_and(|keep| keep == path) {
            continue;
        }
        let _ = fs::remove_file(&path);
    }
}

fn copy_verified(
    source: &std::path::Path,
    manifest: &UpdateManifest,
    paths: &UpdatePaths,
    public_key: &str,
    cancel: &AtomicBool,
    events: &Events,
) -> Result<PathBuf, String> {
    let size = source.metadata().map_err(|e| e.to_string())?.len();
    if size != manifest.package.size {
        return Err(format!(
            "local package size is {size}, expected {}",
            manifest.package.size
        ));
    }
    fs::create_dir_all(paths.downloads_dir()).map_err(|e| e.to_string())?;
    fs::create_dir_all(paths.verified_dir()).map_err(|e| e.to_string())?;
    let part = paths.downloads_dir().join(format!(
        "{}.{}.part",
        manifest.package.file_name,
        std::process::id()
    ));
    let final_path = paths.verified_dir().join(format!(
        "{}-{}",
        manifest.build_number, manifest.package.file_name
    ));
    let result = (|| {
        let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
        let mut output = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("canceled".into());
            }
            let read = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            received += read as u64;
            output
                .write_all(&buffer[..read])
                .map_err(|e| e.to_string())?;
            digest.update(&buffer[..read]);
            events.send(WorkerEvent::Progress {
                received,
                total: manifest.package.size,
            });
        }
        output.sync_all().map_err(|e| e.to_string())?;
        if received != manifest.package.size {
            return Err(format!(
                "copied {received} bytes, expected {}",
                manifest.package.size
            ));
        }
        if lower_hex(&digest.finalize()) != manifest.package.sha256 {
            return Err("local package SHA-256 does not match the manifest".into());
        }
        verify_package(&part, &manifest.package, public_key)?;
        if final_path.exists() {
            fs::remove_file(&final_path).map_err(|e| e.to_string())?;
        }
        fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
        Ok(final_path.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(part);
    }
    result
}

fn download_verified(
    manifest: &UpdateManifest,
    paths: &UpdatePaths,
    public_key: &str,
    cancel: &AtomicBool,
    events: &Events,
) -> Result<PathBuf, String> {
    let url = Url::parse(&manifest.package.url).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        && !(cfg!(debug_assertions) && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
    {
        return Err("package URL must use HTTPS".into());
    }
    fs::create_dir_all(paths.downloads_dir()).map_err(|e| e.to_string())?;
    fs::create_dir_all(paths.verified_dir()).map_err(|e| e.to_string())?;
    let part = paths.downloads_dir().join(format!(
        "{}.{}.part",
        manifest.package.file_name,
        std::process::id()
    ));
    let final_path = paths.verified_dir().join(format!(
        "{}-{}",
        manifest.build_number, manifest.package.file_name
    ));
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30 * 60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client.get(url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("package request returned {}", response.status()));
    }
    if let Some(length) = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && length != manifest.package.size
    {
        return Err(format!(
            "server content length is {length}, expected {}",
            manifest.package.size
        ));
    }
    let result = (|| {
        let mut output = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut received = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err("canceled".into());
            }
            let read = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                break;
            }
            received = received
                .checked_add(read as u64)
                .ok_or("download byte-count overflow")?;
            if received > manifest.package.size {
                return Err("download exceeded the declared package size".into());
            }
            output
                .write_all(&buffer[..read])
                .map_err(|e| e.to_string())?;
            digest.update(&buffer[..read]);
            events.send(WorkerEvent::Progress {
                received,
                total: manifest.package.size,
            });
        }
        output.sync_all().map_err(|e| e.to_string())?;
        if received != manifest.package.size {
            return Err(format!(
                "downloaded {received} bytes, expected {}",
                manifest.package.size
            ));
        }
        let hash = lower_hex(&digest.finalize());
        if hash != manifest.package.sha256 {
            return Err("downloaded package SHA-256 does not match the manifest".into());
        }
        verify_package(&part, &manifest.package, public_key)?;
        if final_path.exists() {
            fs::remove_file(&final_path).map_err(|e| e.to_string())?;
        }
        fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
        Ok(final_path.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(part);
    }
    result
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_manifest(build_number: u64) -> UpdateManifest {
        let info = build_info();
        UpdateManifest {
            schema_version: 1,
            channel: ReleaseChannel::of_this_build(),
            platform: info.platform.into(),
            architecture: info.architecture.into(),
            version: format!("0.0.0-test.{build_number}"),
            build_number,
            published_at: "2026-08-22T00:00:00Z".into(),
            commit: None,
            source: None,
            minimum_updater_version: None,
            minimum_supported_build: None,
            required: false,
            release_notes: crate::manifest::ReleaseNotes {
                title: "Test".into(),
                summary: vec!["Test update".into()],
                url: None,
            },
            package: crate::manifest::UpdatePackage {
                url: "https://example.invalid/a.zip".into(),
                file_name: "a.zip".into(),
                size: 1,
                sha256: "a".repeat(64),
                signature: "test signature".into(),
                // This build's platform, so this build's package format —
                // validation refuses a zip on Linux.
                format: if cfg!(target_os = "linux") {
                    crate::manifest::PackageFormat::TarGz
                } else {
                    crate::manifest::PackageFormat::Zip
                },
            },
        }
    }

    /// A `Shared` with no paths touched and no network reachable: every test
    /// here drives `settle_on`, which reads state and writes a status.
    fn a_shared() -> Shared {
        Shared {
            base_url: "https://example.invalid".into(),
            rehearsal_package: None,
            paths: {
                let root = std::env::temp_dir().join("koru-controller-test");
                UpdatePaths {
                    config_dir: root.join("config"),
                    data_dir: root.join("data"),
                    cache_dir: root.join("cache"),
                    logs_dir: root.join("logs"),
                }
            },
            state: UpdateState::default(),
            channel: ReleaseChannel::of_this_build(),
            session_deferred: None,
            session_newer_dismissed: false,
            status: UpdateStatus::Idle,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            notify: None,
        }
    }

    #[test]
    fn a_local_rehearsal_offers_the_normal_manifest_without_a_feed() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("a.zip");
        fs::write(&package, b"x").unwrap();
        let manifest = a_manifest(build_info().build_number.saturating_add(1).max(1));
        let manifest_path = dir.path().join("a.update.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let paths = UpdatePaths::under(&dir.path().join("root"));

        let controller = UpdateController::rehearsal(&manifest_path, paths.clone()).unwrap();
        assert_eq!(
            controller.status(),
            UpdateStatus::Available(manifest.clone())
        );
        assert_eq!(controller.paths(), paths);

        // Check/Retry derives the same local offer and starts no worker. A local
        // rehearsal must never turn this placeholder URL into a request.
        controller.check(CheckReason::Manual);
        assert_eq!(controller.status(), UpdateStatus::Available(manifest));
        assert!(
            controller
                .shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .worker
                .is_none()
        );
    }

    /// **A package is not offered to another channel**, whatever the numbers
    /// say. One counter serves every channel, so "newer" orders builds without
    /// saying which product line they belong to; the directories are scoped per
    /// channel now, and this is the check that does not depend on that.
    #[test]
    fn a_package_from_another_channel_is_never_offered() {
        let cache = std::env::temp_dir().join("koru-cross-channel-test");
        std::fs::create_dir_all(&cache).unwrap();
        let package = cache.join("cross.zip");
        std::fs::write(&package, b"z").unwrap();

        let build = build_info().build_number + 1;
        let mut manifest = a_manifest(build);
        manifest.channel = match ReleaseChannel::of_this_build() {
            ReleaseChannel::Nightly => ReleaseChannel::Stable,
            _ => ReleaseChannel::Nightly,
        };
        let mut shared = a_shared();
        shared.state.last_manifest = Some(manifest);
        shared.state.cached_verified_package = Some(crate::state::CachedPackage {
            build_number: build,
            path: package.clone(),
            sha256: String::new(),
        });

        UpdateController::offer_what_is_already_downloaded(&mut shared);
        assert_eq!(
            shared.status,
            UpdateStatus::Idle,
            "a higher build number from another channel is not an update to this one"
        );
        let _ = std::fs::remove_file(&package);
    }

    /// **The newest two survive and nothing else does.** Sixteen zips going
    /// back six weeks — 658 MB — is what "nothing ever pruned this" looked
    /// like on a real machine following nightly.
    #[test]
    fn pruning_keeps_the_two_newest_packages() {
        let dir = std::env::temp_dir().join("koru-prune-test-newest");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for build in [9u64, 33, 39, 40, 25] {
            std::fs::write(dir.join(format!("{build}-koru-windows-x86_64.zip")), b"z").unwrap();
        }

        prune_verified(&dir, None);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "39-koru-windows-x86_64.zip".to_string(),
                "40-koru-windows-x86_64.zip".to_string()
            ],
            "the newest two, ordered by build number rather than by name"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// It deletes what it recognises, not everything in the directory.
    #[test]
    fn pruning_leaves_alone_what_it_did_not_name() {
        let dir = std::env::temp_dir().join("koru-prune-test-strangers");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["9-a.zip", "33-a.zip", "39-a.zip", "40-a.zip", "notes.txt"] {
            std::fs::write(dir.join(name), b"z").unwrap();
        }

        prune_verified(&dir, None);

        assert!(dir.join("notes.txt").is_file(), "not ours, not deleted");
        assert!(!dir.join("9-a.zip").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The package this update is holding is never the one thrown away.
    #[test]
    fn pruning_never_drops_the_package_it_was_handed() {
        let dir = std::env::temp_dir().join("koru-prune-test-holding");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for build in [9u64, 33, 39, 40] {
            std::fs::write(dir.join(format!("{build}-a.zip")), b"z").unwrap();
        }
        // Contrived, and the point: an older build is the one in hand.
        let holding = dir.join("9-a.zip");

        prune_verified(&dir, Some(&holding));

        assert!(
            holding.is_file(),
            "the package in hand survives the ordering"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The disk is asked before the server.** An interrupted install leaves a
    /// verified package in the cache; needing a network round trip to remember
    /// that is what made ADR-151's chain terminal rather than annoying.
    #[test]
    fn a_verified_package_in_the_cache_is_offered_before_any_check() {
        let cache = std::env::temp_dir().join("koru-cache-offer-test");
        std::fs::create_dir_all(&cache).unwrap();
        let package = cache.join("cached.zip");
        std::fs::write(&package, b"not really a zip").unwrap();

        let build = build_info().build_number + 1;
        let mut shared = a_shared();
        shared.state.last_manifest = Some(a_manifest(build));
        shared.state.cached_verified_package = Some(crate::state::CachedPackage {
            build_number: build,
            path: package.clone(),
            sha256: String::new(),
        });

        UpdateController::offer_what_is_already_downloaded(&mut shared);

        match &shared.status {
            UpdateStatus::ReadyToInstall { package: p, .. } => assert_eq!(p, &package),
            other => panic!("a downloaded build should be offered at once, got {other:?}"),
        }
        let _ = std::fs::remove_file(&package);
    }

    /// A cache entry whose file has been deleted — by a disk cleaner, or by
    /// hand — must not be offered as ready to install.
    #[test]
    fn a_cache_entry_whose_file_is_gone_is_not_offered() {
        let build = build_info().build_number + 1;
        let mut shared = a_shared();
        shared.state.last_manifest = Some(a_manifest(build));
        shared.state.cached_verified_package = Some(crate::state::CachedPackage {
            build_number: build,
            path: std::env::temp_dir().join("koru-no-such-package.zip"),
            sha256: String::new(),
        });

        UpdateController::offer_what_is_already_downloaded(&mut shared);
        assert_eq!(shared.status, UpdateStatus::Idle, "nothing to offer");
    }

    /// **A 304 is not "up to date".** The failure this pins was a stopper: an
    /// install that gave up put the status into `Failed`, the next check sent
    /// the stored ETag, the origin answered 304, and the controller announced
    /// *up to date* — with build 40 verified in the cache and build 39
    /// installed. A manual *Check for updates* sends the same ETag, so nothing
    /// the person could press disagreed, and only publishing a further build
    /// would have cleared it.
    #[test]
    fn a_not_modified_check_re_offers_the_build_it_last_saw() {
        let cache = std::env::temp_dir().join("koru-304-test");
        std::fs::create_dir_all(&cache).unwrap();
        let package = cache.join("a-package.zip");
        std::fs::write(&package, b"not really a zip").unwrap();

        let installed = build_info().build_number;
        let manifest = a_manifest(installed + 1);
        let mut shared = a_shared();
        shared.state.last_manifest = Some(manifest.clone());
        shared.state.cached_verified_package = Some(crate::state::CachedPackage {
            build_number: manifest.build_number,
            path: package.clone(),
            sha256: String::new(),
        });
        // As `install_blocked` leaves it after a handoff that timed out.
        shared.status = UpdateStatus::Failed(UpdateFailure::new("blocked", "detail", true));

        UpdateController::settle_on(&mut shared, CheckReason::Manual, manifest);

        match &shared.status {
            UpdateStatus::ReadyToInstall { package: p, .. } => assert_eq!(p, &package),
            other => panic!("a verified newer build must be offered again, got {other:?}"),
        }
        let _ = std::fs::remove_file(&package);
    }

    /// The same derivation still says *up to date* when it truly is, so the fix
    /// is "answer with the manifest" rather than "never say up to date".
    #[test]
    fn a_not_modified_check_on_the_installed_build_is_still_up_to_date() {
        let mut shared = a_shared();
        let manifest = a_manifest(build_info().build_number);
        UpdateController::settle_on(&mut shared, CheckReason::Manual, manifest);
        assert_eq!(shared.status, UpdateStatus::UpToDate);
    }

    /// **A channel is a release rate, and the poll has to match it.**
    ///
    /// A flat twenty-four hours meant a running machine missed most nightlies —
    /// several ship in a working day. Asserted as an ordering rather than three
    /// magic numbers, because the numbers are a judgement and the ordering is
    /// the property: a faster channel is never polled more slowly than a slower
    /// one.
    #[test]
    fn a_faster_channel_is_polled_more_often() {
        let nightly = automatic_interval(ReleaseChannel::Nightly);
        let beta = automatic_interval(ReleaseChannel::Beta);
        let stable = automatic_interval(ReleaseChannel::Stable);
        assert!(nightly < beta, "nightly ships fastest");
        assert!(beta < stable, "beta ships faster than stable");
        // An internal build has no feed at all and its check is refused before
        // it reaches the network, so it costs nothing either way — but it must
        // not be the *most* eager thing on the machine.
        assert_eq!(
            automatic_interval(ReleaseChannel::Internal),
            stable,
            "a build with no feed polls no harder than stable"
        );
    }

    /// Nobody should be waiting a working day to hear about a nightly. Not a
    /// tuning knob — this is the bound the bug report was about.
    #[test]
    fn a_nightly_is_offered_within_the_hour() {
        assert!(automatic_interval(ReleaseChannel::Nightly) <= Duration::from_secs(60 * 60));
    }

    /// One target, two origins, two tags: an ETag issued by one server is
    /// never sent to another.
    #[test]
    fn an_etag_is_remembered_against_the_url_that_issued_it() {
        let target = UpdateTarget {
            channel: ReleaseChannel::Nightly,
            platform: "linux".into(),
            architecture: "x86_64".into(),
        };
        assert_eq!(
            etag_key("https://updates.koru.do/", &target),
            "https://updates.koru.do/channels/nightly/linux-x86_64.json"
        );
        assert_eq!(
            etag_key(
                "https://github.com/koruworks/mono/releases/download/nightly/",
                &target
            ),
            "https://github.com/koruworks/mono/releases/download/nightly/nightly-linux-x86_64.json"
        );
    }

    /// Every test here builds its own `UpdatePaths` under a temp directory.
    /// Discovering the real ones would put a test's `state.json` in the
    /// developer's `%LOCALAPPDATA%\koru` and let it toggle their own updates.
    fn temp_paths(dir: &tempfile::TempDir) -> UpdatePaths {
        UpdatePaths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            cache_dir: dir.path().join("cache"),
            logs_dir: dir.path().join("logs"),
        }
    }

    /// A base URL no test ever dials. It is here to make that visible: nothing
    /// below calls `check`, so nothing below opens a socket.
    const UNDIALLED: &str = "https://updates.invalid/";

    fn controller_over(paths: UpdatePaths) -> UpdateController {
        UpdateController::new(UNDIALLED, paths).unwrap()
    }

    fn offered_manifest(build_number: u64) -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            channel: ReleaseChannel::Beta,
            platform: "macos".into(),
            architecture: "aarch64".into(),
            version: "0.0.2-beta.6".into(),
            build_number,
            published_at: "2026-08-21T14:35:23Z".into(),
            commit: Some("6c41f03".into()),
            source: None,
            minimum_updater_version: Some(1),
            minimum_supported_build: None,
            required: false,
            release_notes: crate::ReleaseNotes {
                title: "Test".into(),
                summary: Vec::new(),
                url: None,
            },
            package: crate::UpdatePackage {
                url: "https://updates.invalid/koru.dmg".into(),
                file_name: "koru.dmg".into(),
                size: 1,
                sha256: "a".repeat(64),
                signature: "sig".into(),
                format: crate::PackageFormat::Dmg,
            },
        }
    }

    fn check_found(controller: &UpdateController, reason: CheckReason, build_number: u64) {
        controller.apply(WorkerEvent::CheckDone {
            reason,
            result: Ok(FetchResult::Manifest {
                manifest: offered_manifest(build_number),
                etag: None,
            }),
            etag_key: "channels/beta/macos-aarch64.json".into(),
        });
    }

    /// Seen from the tray on a Mac: beta.6 was downloaded, the Ready banner
    /// got *Later*, and then **Check for updates** showed nothing — the same
    /// build was deferred for the session. An automatic poll must stay quiet
    /// about a build the person waved away; a manual check is the person
    /// asking to be offered it again, and answers.
    #[test]
    fn a_manual_check_offers_a_build_the_session_deferred_and_a_background_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let controller = controller_over(temp_paths(&dir));
        let build = build_info().build_number + 1;
        let offered = |status: UpdateStatus| {
            matches!(
                status,
                UpdateStatus::Available(_) | UpdateStatus::ReadyToInstall { .. }
            )
        };
        check_found(&controller, CheckReason::Startup, build);
        assert!(offered(controller.status()));
        controller.defer();
        assert_eq!(controller.status(), UpdateStatus::Deferred(build));

        check_found(&controller, CheckReason::Startup, build);
        assert!(
            !offered(controller.status()),
            "a background check does not nag about a deferred build: {:?}",
            controller.status()
        );

        check_found(&controller, CheckReason::Manual, build);
        assert!(
            offered(controller.status()),
            "a manual check is a request to be offered it again: {:?}",
            controller.status()
        );
        // And it stays offered on the next background check, since the
        // person asked for it back.
        check_found(&controller, CheckReason::Startup, build);
        assert!(offered(controller.status()));
    }

    #[test]
    fn a_fresh_controller_starts_no_worker_and_start_automatic_is_inert_under_test() {
        let dir = tempfile::tempdir().unwrap();
        let controller = controller_over(temp_paths(&dir));
        controller.start_automatic();
        // No worker means no thread is holding a `Sender`, which is the only
        // way this crate reaches the network. Asserting on the *absence* of a
        // request rather than on a failed one: a test that asserted a
        // connection failed would pass here and fail on a machine with a
        // proxy that answers everything.
        assert!(controller.shared.lock().unwrap().worker.is_none());
        assert_eq!(controller.status(), UpdateStatus::Idle);
        assert!(!controller.tick());
    }

    /// The ordering rule, pinned from the reader's side: by the time the wake
    /// runs, there is already something to drain. Inverting the two lines in
    /// `Events::send` fails this and nothing else — which is exactly why it is
    /// worth a test rather than a comment.
    #[test]
    fn a_wake_arrives_after_its_event_is_queued_never_before() {
        let (tx, rx) = mpsc::channel();
        let inbox = Arc::new(Mutex::new(rx));
        let drained = Arc::new(AtomicBool::new(false));
        let events = Events {
            tx,
            notify: Some(Arc::new({
                let inbox = Arc::clone(&inbox);
                let drained = Arc::clone(&drained);
                move || {
                    let found = inbox.lock().unwrap().try_recv().is_ok();
                    drained.store(found, Ordering::Release);
                }
            })),
        };
        events.send(WorkerEvent::Progress {
            received: 1,
            total: 2,
        });
        assert!(
            drained.load(Ordering::Acquire),
            "the wake ran before the event was queued and drained nothing"
        );
    }

    #[test]
    fn turning_automatic_checks_off_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let controller = controller_over(paths.clone());
        assert!(controller.automatic_checks(), "on unless turned off");
        controller.set_automatic_checks(false);
        assert!(!UpdateState::load(&paths.state_file()).automatic_checks);
        assert!(!controller_over(paths).automatic_checks());
    }

    #[test]
    fn the_channel_is_this_builds_whatever_the_state_file_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let other = ReleaseChannel::ALL
            .into_iter()
            .find(|channel| *channel != ReleaseChannel::of_this_build())
            .expect("four channels, so one of them is not this build's");
        let state = UpdateState {
            selected_channel: other,
            ..UpdateState::default()
        };
        state.save(&paths.state_file()).unwrap();
        // The rule, pinned: koru-update only ever updates its own channel. A
        // stale preference — or a hand-edited file — cannot make a nightly
        // binary fetch stable's manifest and install it over itself.
        assert_eq!(
            controller_over(paths).channel(),
            ReleaseChannel::of_this_build()
        );
    }
}
