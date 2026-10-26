// SPDX-License-Identifier: MPL-2.0

use koru_update::{
    ArchiveLimits, FailedBuild, PackageFormat, ReleaseFiles, UpdatePackage, UpdatePaths,
    UpdaterRequest, build_info, read_package_version, verify_package,
};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const WAIT_FOR_APP: Duration = Duration::from_secs(120);

/// How long the restarted Koru has to confirm it is healthy.
///
/// **Ninety seconds, not thirty.** The old value was sized for "launch an exe,
/// get a frame". A restart now brings up a *stack*: the agent binds its
/// endpoint, starts its gateway worker, and that worker's Store opens SQLite
/// before a shell can connect to it. On a slow disk, or a machine where an antivirus scans
/// three freshly-written binaries before it will let them run, that can pass
/// thirty seconds on a perfectly good build.
///
/// The consequence of getting this wrong is not a warning. A timeout triggers a
/// **full rollback** and records a `failed_build`, which
/// `koru_update::UpdateController` then suppresses from automatic re-offering —
/// so one slow machine strands itself on the old build until somebody thinks to
/// press *Check for updates* by hand.
const WAIT_FOR_HEALTH: Duration = Duration::from_secs(90);

/// A restored process must survive its immediate startup window before the
/// updater declares rollback complete. `Command::spawn` proves only that
/// Windows created a process; a root/endpoint or lease race can still make the
/// Agent exit zero a few milliseconds later, leaving a completely restored
/// installation with nothing running.
const WAIT_FOR_RESTORED_START: Duration = Duration::from_secs(1);
const RESTORED_RETRY_PAUSE: Duration = Duration::from_millis(250);

/// How long to wait for this installation's own services to let go of their
/// executables. See [`quiesce`].
const WAIT_FOR_SERVICES: Duration = Duration::from_secs(60);

/// How often to re-probe while waiting for them.
const PROBE_EVERY: Duration = Duration::from_millis(200);

/// Defensive stop asks for every process generation an installed manifest may
/// still own. The booleans close startup races by pausing before the repeated
/// ask; legacy names never enter the current package executable catalog.
///
/// **The rule is not "every retired name".** It is *every retired name that has
/// a `stop` verb*, because this sweep's only move is to run the installed
/// executable with `stop`. `koru-terminal.exe` is deliberately absent — the
/// retired Terminal was a window closed through Agent presence and never a stop
/// service — and `the_stop_sweep_asks_every_supported_store_generation` asserts
/// that absence so nobody adds it for symmetry.
///
/// **Retiring an executable and keeping it here are two changes, and only the
/// second lets the first happen.** [`quiesce`] runs `install.join(service)` —
/// the image already **installed**, not the one being staged — so the
/// pre-cutover build on disk still has the verb this asks for, however long ago
/// the package stopped shipping it. Removing a name from here would not tidy
/// anything; it would mean an upgrade that finds that process running waits out
/// the whole [`WAIT_FOR_SERVICES`] window and then fails, naming a process
/// nobody asked to stop.
const STOP_SEQUENCE: &[(&str, bool)] = &[
    (koru_update::AGENT_EXE, false),
    (koru_update::SESSION_HOST_EXE, false),
    (koru_update::SESSION_HOST_EXE, true),
    (koru_update::AI_CHAT_EXE, false),
    // **After the Agent, because the Agent is what starts one.** A voice worker
    // is started on demand and restarted by nothing, so once supervision is
    // down this ask is final. It is asked at all — rather than being left to the
    // probe below — because a worker that is mid-dictation holds its own
    // executable, and waiting sixty seconds for an idle timeout that is five
    // minutes away would fail the update for a process that would have stopped
    // at once if asked.
    (koru_update::VOICE_EXE, false),
    // **The pre-rename voice worker, which nothing here ever asked.** The
    // rename to `koru-voice-worker` retired the file name and left the program
    // alone: an installed `koru-voice.exe` answers the same `stop` verb, on the
    // same Store root. Until this row existed, an upgrade that found one
    // running had nothing to ask and failed sixty seconds later naming it.
    // Asked twice, like every other retired generation, because what supervises
    // a pre-cutover installation is a build this one cannot reason about.
    (koru_update::LEGACY_VOICE_EXE, false),
    (koru_update::LEGACY_VOICE_EXE, true),
    // **The retired standalone gateway and session host.** Both left the
    // package in Plan 7 and both keep their two asks, because an installation
    // upgrading *from* a pre-cutover build has them on disk and may have them
    // running — which is the whole of verification case 8. The gateway keeps
    // the paused second ask for the reason it always had one: a pre-cutover
    // Agent ensures it on a worker thread, so an ask can land while the process
    // is still binding and go to nobody.
    (koru_update::GATEWAY_EXE, false),
    (koru_update::GATEWAY_EXE, true),
    (koru_update::LEGACY_STORE_SERVER_EXE, false),
    (koru_update::LEGACY_STORE_SERVER_EXE, true),
    (koru_update::LEGACY_STORE_EXE, false),
    (koru_update::LEGACY_STORE_EXE, true),
];

const DISK_MARGIN: u64 = 256 * 1024 * 1024;

/// The package the flat-directory transaction in [`run`] installs from. One
/// per platform: the manifest's `validate` admits only this format there, so
/// a request reaching the updater carries it.
#[cfg(target_os = "linux")]
const PACKAGE_FORMAT: PackageFormat = PackageFormat::TarGz;
#[cfg(not(target_os = "linux"))]
const PACKAGE_FORMAT: PackageFormat = PackageFormat::Zip;

/// A Linux install is wherever somebody unpacked the tarball — their home
/// directory, or `/opt` as root — so whether this user may write there is a
/// real question, and the answer has to be a sentence they can act on.
#[cfg(target_os = "linux")]
fn ensure_writable(install: &Path) -> Result<(), String> {
    if koru_update::is_writable_install(install) {
        Ok(())
    } else {
        Err(format!(
            "{} is not writable by this user, so Koru cannot update itself there; \
             unpack the new release by hand, or move Koru to a folder you own",
            install.display()
        ))
    }
}

/// Unpack the package into an empty staging directory, under the same limits
/// and the same path, type and size refusals whichever the format.
fn extract_package(package: &Path, staging: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return koru_update::extract_tar_gz(package, staging, ArchiveLimits::default());
    #[cfg(not(target_os = "linux"))]
    return koru_update::extract_zip(package, staging, ArchiveLimits::default());
}

fn stop_arguments(service: &str, rehearsal_root: Option<&Path>) -> Vec<std::ffi::OsString> {
    let mut arguments = vec!["stop".into()];
    if let Some(root) = rehearsal_root {
        // The voice worker is rooted the way the gateway is: its `--root` is a
        // Store root, because that is where its endpoint, its logs and its
        // model cache are resolved from.
        let root = if service == koru_update::GATEWAY_EXE
            || service == koru_update::VOICE_EXE
            || service == koru_update::LEGACY_VOICE_EXE
            || service == koru_update::LEGACY_STORE_SERVER_EXE
            || service == koru_update::LEGACY_STORE_EXE
        {
            koru_channel::store_root_under(root)
        } else {
            root.to_path_buf()
        };
        arguments.push("--root".into());
        arguments.push(root.into_os_string());
    }
    arguments
}

fn launch_arguments(
    health_token: Option<&str>,
    rehearsal_root: Option<&Path>,
    rehearsal_manifest: Option<&Path>,
) -> Vec<std::ffi::OsString> {
    let mut arguments = Vec::new();
    if let Some(token) = health_token {
        arguments.push("--update-health-token".into());
        arguments.push(token.into());
    }
    if let Some(root) = rehearsal_root {
        arguments.push("--root".into());
        arguments.push(root.as_os_str().to_owned());
    }
    if let Some(manifest) = rehearsal_manifest {
        arguments.push("--rehearse-update".into());
        arguments.push(manifest.as_os_str().to_owned());
    }
    arguments
}

fn owned_files(incoming: &ReleaseFiles, installed: Option<&ReleaseFiles>) -> BTreeSet<String> {
    let mut owned: BTreeSet<String> = incoming.files.iter().cloned().collect();
    if let Some(installed) = installed {
        owned.extend(installed.files.iter().cloned());
    }
    owned
}

/// The executables this updater is willing to restart.
///
/// The real invariant is "inside the installation directory, and one of ours" —
/// the directory half is checked separately in [`validate_request`], and this is
/// the other half. It used to be a single hardcoded `"koru.exe"`, from when that
/// was the only thing a Koru installation could restart into.
///
/// `koru-agent.exe` is here because the agent is what owns the device: when it
/// is the process that asked for the update, it is also the process that must
/// come back, restore the store and reopen the windows it closed.
const RESTARTABLE: &[&str] = &[koru_update::APP_EXE, koru_update::AGENT_EXE];

pub enum Outcome {
    Installed,
    RolledBack(String),
}

// The rest of the body still type-checks where it is not run — that is the
// point of not `cfg`-ing it away — so the lints the unconditional `return`
// trips are allowed rather than the code hidden.
#[cfg_attr(
    not(any(windows, target_os = "linux")),
    allow(unreachable_code, unused_variables)
)]
pub fn run(request_path: &Path, wait_pid: u32) -> Result<Outcome, String> {
    // macOS has its own transaction: the package is a disk image, the swap is
    // one whole-bundle rename each way, and the quiesce proof is process
    // *absence* — the write-open probe below is a Windows fact and proves
    // nothing here (see [`macos`]).
    #[cfg(target_os = "macos")]
    return macos::run(request_path, wait_pid);
    // **Linux runs this transaction**, not one of its own. Its package is the
    // Windows zip's layout in a tarball — a flat directory of binaries and
    // data — so the file-by-file swap, the journal, the rollback and the
    // health commit all apply unchanged. Three things follow the platform:
    // the extractor ([`extract_package`]), the quiesce proof, which is process
    // absence as on macOS ([`still_running`]), and a writability check up
    // front, because a Linux install is wherever somebody unpacked it.
    //
    // **Refuse rather than half-work** everywhere else: without a proof that
    // nothing is running, the swap would proceed against live services. An
    // honest error beats a quiet maybe.
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    return Err(
        "koru-updater does not support installing on this platform yet; see docs/features/macos/RELEASING.md"
            .to_string(),
    );
    let bytes = fs::read(request_path).map_err(|e| format!("could not read request: {e}"))?;
    let request: UpdaterRequest =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid request: {e}"))?;
    validate_request(&request)?;
    validate_root_scope(request_path, &request)?;
    let paths = update_paths(&request)?;
    wait_for_exit(wait_pid, WAIT_FOR_APP)?;
    if let Err(error) = paths.ensure() {
        return restart_after_error(&request, format!("could not prepare update state: {error}"));
    }
    let mut log = Log::open(&paths.updater_log());
    log.line(&format!(
        "begin current={} target={} package={} install={}",
        request.current_build,
        request.target_build,
        request.package.display(),
        request.install_dir.display()
    ));
    // Every running Koru holds this file shared. After the requesting process
    // exits, exclusive acquisition is both the race-free same-install check and
    // the single-updater lock.
    let mut update_lock = match koru_update::UpdateLock::acquire(&paths, &request.install_dir) {
        Ok(lock) => Some(lock),
        Err(_) => {
            return restart_after_error(
                &request,
                "another Koru instance is still using this installation".into(),
            );
        }
    };
    let mut restart_gate = match koru_update::RestartGate::acquire(&paths, &request.install_dir) {
        Ok(gate) => Some(gate),
        Err(error) => {
            drop(update_lock.take());
            return restart_after_error(&request, error);
        }
    };
    let update_root = request.install_dir.join(".update");
    let journal = update_root.join("transaction.json");
    let recovery = recover_interrupted_files(
        &request.install_dir,
        &paths,
        command_root(&request),
        &journal,
        &mut log,
    )?;
    if let RecoveryOutcome::RestoredUnproven(error) = recovery {
        drop(update_lock.take());
        drop(restart_gate.take());
        return restart_after_rollback(&request, error);
    }
    let health = paths
        .health_dir()
        .join(format!("{}.ok", request.health_token));
    if let Err(error) = retire_health_marker(&health) {
        drop(update_lock.take());
        drop(restart_gate.take());
        return restart_after_error(&request, error);
    }
    let token = Uuid::new_v4().simple().to_string();
    let staging = update_root.join(format!("staging-{}-{token}", request.target_build));
    let previous_root = request.install_dir.join(".previous");
    let backup = previous_root.join(format!("build-{}-{token}", request.current_build));
    let prepared = (|| -> Result<(ReleaseFiles, BTreeSet<String>), String> {
        let package = UpdatePackage {
            url: "https://local.invalid/already-downloaded".into(),
            file_name: request
                .package
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("update.zip")
                .to_string(),
            size: request.package_size,
            sha256: request.expected_sha256.clone(),
            signature: request.signature.clone(),
            format: PACKAGE_FORMAT,
        };
        verify_package(&request.package, &package, build_info().update_public_key)?;
        log.line("package verification succeeded");
        // The agent asked the same question before handing over; asked again
        // here because this is the process about to write, and a refusal now
        // costs nothing — it restarts what was already there. Linux only, so
        // the Windows transaction is exactly what it was.
        #[cfg(target_os = "linux")]
        ensure_writable(&request.install_dir)?;
        check_space(&request)?;
        ensure_managed_path(&request.install_dir, &staging, ".update")?;
        ensure_managed_path(&request.install_dir, &backup, ".previous")?;
        fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        extract_package(&request.package, &staging)?;
        log.line(&format!("staged at {}", staging.display()));

        // **`load_incoming`, not `load`.** This updater came out of the build
        // being replaced, so its compiled-in inventory table may predate the
        // package's `inventory_version` — the strict required-files check ran
        // in `koru-release` before the package was signed, and repeating it
        // here refused build 75 from build 74 (ADR-258). The signature above
        // and the staged-file sweep below are the install-time proof.
        let incoming = ReleaseFiles::load_incoming(&staging.join("release-files.json"))?;
        if incoming.build_number != request.target_build {
            return Err("release-files build does not match the requested target".into());
        }
        let version = read_package_version(&staging.join("VERSION"))?;
        if version.build_number != request.target_build {
            return Err("VERSION build does not match the requested target".into());
        }
        for file in &incoming.files {
            if !staging.join(file).is_file() {
                return Err(format!("staged release is missing {file}"));
            }
        }

        // **`load_installed`, not `load`.** The installed list is read only to
        // widen what this update owns, and it was written by an *older* build —
        // one from before the agent and the store were part of a package, which
        // the strict check refuses. Read strictly and swallowed with `.ok()`,
        // that refusal is indistinguishable from "there was no list", the union
        // silently narrows to the incoming files, and a file the old package
        // owned but the new one does not — `koru.cmd`, on exactly the upgrade
        // this was written for — is orphaned instead of being moved aside.
        let old =
            match ReleaseFiles::load_installed(&request.install_dir.join("release-files.json")) {
                Ok(old) => Some(old),
                Err(error) => {
                    // Not fatal: a first install into an empty directory has no list
                    // at all. Logged, because the other reason to be here is a list
                    // that is present and unreadable, and that is worth knowing.
                    log.line(&format!("no usable installed file list ({error})"));
                    None
                }
            };
        let owned = owned_files(&incoming, old.as_ref());

        // **Before anything is moved.** A failure here takes the preparation
        // path below, which removes the staging and backup directories and
        // restarts what was already there — so refusing to start is free, and
        // the alternative is a half-replaced installation.
        quiesce(
            &request.install_dir,
            &owned,
            command_root(&request),
            &mut log,
        )?;

        fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
        write_journal(
            &journal,
            &request,
            &staging,
            &backup,
            &owned,
            &incoming.files,
        )?;
        Ok((incoming, owned))
    })();
    let (incoming, owned) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            log.line(&format!("update preparation failed: {error}"));
            let _ = remove_managed_dir(&request.install_dir, &staging, ".update");
            let _ = remove_managed_dir(&request.install_dir, &backup, ".previous");
            let _ = fs::remove_file(&journal);
            drop(update_lock.take());
            drop(restart_gate.take());
            return restart_after_error(&request, error);
        }
    };

    if let Err(failure) = replace_owned(
        &request.install_dir,
        &staging,
        &backup,
        &incoming,
        &owned,
        &mut log,
    ) {
        let mut reason = format!("installation failed: {}", failure.reason);
        if failure.restored {
            // `replace_owned` has already put every old file back. Leaving the
            // pre-move journal behind would make the next updater infer that
            // the staged files it briefly moved are still active and delete
            // the restored old copies during crash recovery.
            reason = restoration_reason(
                reason,
                finish_transaction(
                    &request.install_dir,
                    &staging,
                    &backup,
                    &journal,
                    &health,
                    &request.health_token,
                    TransactionProof::Restored,
                ),
            );
        }
        record_failure(&paths, request.target_build, &reason);
        drop(update_lock.take());
        drop(restart_gate.take());
        return if failure.restored {
            restart_after_rollback(&request, reason)
        } else {
            Err(reason)
        };
    }
    log.line(&format!("backup created at {}", backup.display()));

    // The replacement is complete. Release the exclusive lock so the new
    // process can acquire its normal shared instance lease during startup.
    drop(update_lock.take());
    let mut child = match launch(
        &request.restart,
        Some(&request.health_token),
        command_root(&request),
        None,
    ) {
        Ok(child) => child,
        Err(mut error) => {
            let _rollback_lock = koru_update::UpdateLock::acquire(&paths, &request.install_dir)?;
            rollback(&request.install_dir, &backup, &incoming.files, &mut log)?;
            error = restoration_reason(
                error,
                finish_transaction(
                    &request.install_dir,
                    &staging,
                    &backup,
                    &journal,
                    &health,
                    &request.health_token,
                    TransactionProof::Restored,
                ),
            );
            record_failure(&paths, request.target_build, "failed_launch");
            drop(_rollback_lock);
            drop(restart_gate.take());
            return restart_after_rollback(&request, error);
        }
    };
    match wait_for_health(&mut child, &health, WAIT_FOR_HEALTH) {
        Ok(()) => {
            log.line("health confirmation received");
            finish_transaction(
                &request.install_dir,
                &staging,
                &backup,
                &journal,
                &health,
                &request.health_token,
                TransactionProof::Committed,
            )?;
            let _ = fs::remove_file(request_path);
            drop(restart_gate.take());
            log.line("installation complete");
            Ok(Outcome::Installed)
        }
        Err(mut reason) => {
            let _ = child.kill();
            let _ = child.wait();
            let _rollback_lock = koru_update::UpdateLock::acquire(&paths, &request.install_dir)?;
            // **The stack that was just launched is still up, and killing the
            // child did not take it with it.** Whatever was restarted brought
            // the agent and the store back on its way to being healthy — that is
            // what a Koru does on launch — and `child.kill()` reaches only the
            // process this updater spawned. Neither of the survivors holds an
            // instance lease, so the exclusive lock above is granted anyway, and
            // `rollback` would then rename running images: the same trap as the
            // install path, except here it corrupts the *recovery*.
            //
            // Best effort, deliberately. A rollback that refused to proceed
            // because a service would not stop would leave the installation in
            // the state this branch exists to get it out of.
            if let Err(error) = quiesce(
                &request.install_dir,
                &owned,
                command_root(&request),
                &mut log,
            ) {
                log.line(&format!(
                    "rollback proceeding without a clean stop: {error}"
                ));
            }
            rollback(&request.install_dir, &backup, &incoming.files, &mut log)?;
            reason = restoration_reason(
                reason,
                finish_transaction(
                    &request.install_dir,
                    &staging,
                    &backup,
                    &journal,
                    &health,
                    &request.health_token,
                    TransactionProof::Restored,
                ),
            );
            record_failure(&paths, request.target_build, &reason);
            log.line(&format!("rolled back: {reason}"));
            drop(_rollback_lock);
            drop(restart_gate.take());
            restart_after_rollback(&request, reason)
        }
    }
}

#[derive(Copy, Clone)]
enum TransactionProof {
    Committed,
    Restored,
}

#[derive(Debug, PartialEq, Eq)]
enum RecoveryOutcome {
    Ready,
    /// The complete previous installation is active, but its durable restored
    /// proof could not be published. Do not begin another transaction over the
    /// retained journal/backup; restart the safe old build and retry later.
    RestoredUnproven(String),
}

impl TransactionProof {
    fn suffix(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Restored => "restored",
        }
    }
}

fn proof_path(journal: &Path, health_token: &str, proof: TransactionProof) -> PathBuf {
    journal
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("transaction-{health_token}.{}", proof.suffix()))
}

fn persist_proof(path: &Path, proof: TransactionProof) -> Result<(), String> {
    if path.exists() {
        return validate_proof(path, proof);
    }
    let parent = path
        .parent()
        .ok_or("the transaction proof has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| format!("creating proof directory: {error}"))?;
    let temporary = parent.join(format!(
        ".transaction-proof-{}.tmp",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("creating transaction proof: {error}"))?;
        file.write_all(proof.suffix().as_bytes())
            .map_err(|error| format!("writing transaction proof: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("flushing transaction proof: {error}"))?;
        match fs::rename(&temporary, path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_proof(path, proof)?;
                let _ = fs::remove_file(&temporary);
            }
            Err(error) => return Err(format!("committing transaction proof: {error}")),
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("flushing committed transaction proof: {error}"))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("flushing transaction proof directory: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_proof(path: &Path, proof: TransactionProof) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|error| format!("reading transaction proof: {error}"))?;
    if bytes == proof.suffix().as_bytes() {
        Ok(())
    } else {
        Err(format!(
            "transaction proof {} is incomplete or invalid",
            path.display()
        ))
    }
}

fn proof_exists(path: &Path, proof: TransactionProof) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            validate_proof(path, proof)?;
            Ok(true)
        }
        Ok(_) => Err(format!(
            "transaction proof {} is not a file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("reading transaction proof metadata: {error}")),
    }
}

fn health_confirmed(path: &Path) -> Result<bool, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes == b"ok\n"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("reading update health marker: {error}")),
    }
}

fn retire_health_marker(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => {
            #[cfg(unix)]
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| format!("flushing update health directory: {error}"))?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("retiring update health marker: {error}")),
    }
}

fn finish_transaction(
    install: &Path,
    staging: &Path,
    backup: &Path,
    journal: &Path,
    health: &Path,
    health_token: &str,
    proof: TransactionProof,
) -> Result<(), String> {
    // The durable proof precedes destructive cleanup. It is token-qualified and
    // intentionally retained: if a journal deletion is reordered across power
    // loss, recovery still knows whether active files are the committed new
    // bundle or the completely restored old one.
    persist_proof(&proof_path(journal, health_token, proof), proof)?;
    let _ = remove_managed_dir(install, staging, ".update");
    let _ = remove_managed_dir(install, backup, ".previous");
    // Retire health before the journal. Otherwise a crash after journal
    // deletion can strand a valid-looking marker with no transaction that
    // names it; replaying the same request would accept stale health before
    // the restarted build had done any work. Failure leaves the proven journal
    // intact so recovery retries this ordering safely.
    retire_health_marker(health)?;
    // The proof is the durable decision. If cleanup is interrupted or the
    // journal cannot be removed, the next updater validates that immutable
    // proof and repeats cleanup without reinterpreting active files.
    let _ = fs::remove_file(journal);
    Ok(())
}

fn restoration_reason(reason: String, finish: Result<(), String>) -> String {
    match finish {
        Ok(()) => reason,
        Err(error) => format!(
            "{reason}; the previous Koru was restored but its transaction proof could not be persisted: {error}"
        ),
    }
}

fn restart_after_error(request: &UpdaterRequest, reason: String) -> Result<Outcome, String> {
    restart_previous(request).map_err(|restart| {
        format!("{reason}; the previous Koru also failed to restart: {restart}")
    })?;
    Err(reason)
}

fn restart_after_rollback(request: &UpdaterRequest, reason: String) -> Result<Outcome, String> {
    restart_previous(request)
        .map_err(|restart| format!("{reason}; the restored Koru failed to restart: {restart}"))?;
    Ok(Outcome::RolledBack(reason))
}

/// Start the safe old build and prove it did not immediately stand down.
///
/// One retry closes the narrow post-quiesce race seen by the signed rooted
/// drill without turning a persistently broken rollback into an infinite
/// respawn loop. The child handle may be dropped after the observation window;
/// dropping a `Child` does not terminate the process.
fn restart_previous(request: &UpdaterRequest) -> Result<(), String> {
    let mut first = launch(
        &request.restart,
        None,
        command_root(request),
        request.rehearsal_manifest.as_deref(),
    )?;
    match survives_startup(&mut first, WAIT_FOR_RESTORED_START) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            thread::sleep(RESTORED_RETRY_PAUSE);
            let mut second = launch(
                &request.restart,
                None,
                command_root(request),
                request.rehearsal_manifest.as_deref(),
            )?;
            survives_startup(&mut second, WAIT_FOR_RESTORED_START).map_err(|second_error| {
                format!("first restart {first_error}; retry {second_error}")
            })
        }
    }
}

fn survives_startup(child: &mut Child, window: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < window {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("exited during startup: {status}"));
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn validate_request(request: &UpdaterRequest) -> Result<(), String> {
    if request.current_build == 0
        || request.target_build <= request.current_build
        || request.package_size == 0
        || request.health_token.len() != 32
        || !request
            .health_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("update request contains invalid build metadata".into());
    }
    if !request.package.is_file() || !request.install_dir.is_dir() {
        return Err("update package or installation directory does not exist".into());
    }
    let restart = fs::canonicalize(&request.restart).map_err(|e| e.to_string())?;
    let install = fs::canonicalize(&request.install_dir).map_err(|e| e.to_string())?;
    let named = restart.file_name().and_then(|v| v.to_str());
    if !restart.starts_with(&install) || !named.is_some_and(|name| RESTARTABLE.contains(&name)) {
        return Err("restart executable is not this Koru installation".into());
    }
    if let Some(root) = &request.rehearsal_root {
        let root = fs::canonicalize(root)
            .map_err(|e| format!("the rehearsal root does not exist: {e}"))?;
        if install == root || !install.starts_with(&root) {
            return Err("the rehearsal installation is not beneath its explicit root".into());
        }
    }
    if request.rehearsal_root.is_some() != request.rehearsal_manifest.is_some() {
        return Err("a rehearsal request must carry both its root and signed manifest".into());
    }
    if let Some(manifest) = &request.rehearsal_manifest
        && !manifest.is_file()
    {
        return Err("the rehearsal manifest no longer exists".into());
    }
    if let (Some(root), Some(rehearsal)) = (&request.root, &request.rehearsal_root)
        && fs::canonicalize(root).map_err(|e| e.to_string())?
            != fs::canonicalize(rehearsal).map_err(|e| e.to_string())?
    {
        return Err("rooted update and rehearsal roots disagree".into());
    }
    Ok(())
}

/// A named root confines both inputs and every output before the updater opens
/// a log, creates a lock, or stages a byte. Canonical paths make junctions and
/// symlinks unable to turn a lexical child into a shared checkout or install.
fn validate_root_scope(request_path: &Path, request: &UpdaterRequest) -> Result<(), String> {
    let Some(root) = request.root.as_deref() else {
        return Ok(());
    };
    if !UpdatePaths::root_owns_install(root, &request.install_dir) {
        return Err("a rooted update may replace only a copied bundle beneath its root".into());
    }
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    for (kind, path) in [
        ("request", request_path),
        ("package", request.package.as_path()),
        ("restart", request.restart.as_path()),
    ] {
        let resolved = fs::canonicalize(path).map_err(|e| e.to_string())?;
        if !resolved.starts_with(&root) {
            return Err(format!("rooted update {kind} is outside its selected root"));
        }
    }
    Ok(())
}

/// The explicit root passed to every stopped or restarted Koru process.
/// Rehearsals and ordinary rooted runs use different request fields for wire
/// compatibility, but both must keep every child on the selected channel root.
fn command_root(request: &UpdaterRequest) -> Option<&Path> {
    request
        .rehearsal_root
        .as_deref()
        .or(request.root.as_deref())
}

fn update_paths(request: &UpdaterRequest) -> Result<UpdatePaths, String> {
    match command_root(request) {
        Some(root) => Ok(UpdatePaths::under(root)),
        None => UpdatePaths::discover().ok_or("could not resolve Koru update paths".into()),
    }
}

fn check_space(request: &UpdaterRequest) -> Result<(), String> {
    let install_size = directory_size(&request.install_dir)?;
    let needed = request
        .package_size
        .saturating_mul(5)
        .saturating_add(install_size)
        .saturating_add(DISK_MARGIN);
    let available = koru_free_space::available(&request.install_dir).map_err(|e| e.to_string())?;
    if available < needed {
        return Err(format!(
            "insufficient disk space: {available} bytes available, {needed} required"
        ));
    }
    Ok(())
}

fn directory_size(root: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.starts_with(root.join(".update")) || path.starts_with(root.join(".previous")) {
                continue;
            }
            let ty = entry.file_type().map_err(|e| e.to_string())?;
            if ty.is_dir() {
                pending.push(path);
            } else if ty.is_file() {
                total = total.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
            }
        }
    }
    Ok(total)
}

/// Whether nothing holds this path as a running image.
///
/// **Windows opens an executing image without write sharing**, so asking to open
/// it for writing is the direct question "may I replace this" — and it is the
/// only reliable one, because the obvious alternative does not work (see
/// [`quiesce`]). `.write(true)` on its own does not truncate, so this is a
/// question rather than a change; a path that does not exist is trivially free.
///
/// **This states a Windows fact.** macOS and Linux happily write-open a running
/// image, so here "free" would mean nothing — which is why [`run`] routes
/// macOS to [`macos`], whose proof is process absence, and why
/// [`still_running`] asks Linux's process table instead of this.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn is_free(path: &Path) -> bool {
    !path.exists() || OpenOptions::new().write(true).open(path).is_ok()
}

/// Every executable this update is about to replace.
///
/// Derived from `owned` rather than listed, so a fifth binary is covered the day
/// it is added and nobody has to remember this function exists.
///
/// **The `.exe` filter is a Windows fact too**: off Windows the binaries carry
/// no extension and this answers an empty set. Safe because only the Windows
/// proof reads it — macOS proves quiet by process absence in
/// [`macos::bundle_is_quiet`], and Linux in [`still_running`], which asks
/// about the whole installation rather than a list of names.
fn owned_executables(install: &Path, owned: &BTreeSet<String>) -> Vec<(String, PathBuf)> {
    owned
        .iter()
        .filter(|relative| {
            std::path::Path::new(relative)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        })
        .map(|relative| (relative.clone(), install.join(relative)))
        .collect()
}

/// Bring this installation's own background services down, and prove it.
///
/// ## Why this exists, and why it must not be "simplified" away
///
/// **Renaming a *running* executable succeeds on Windows.** Only deleting or
/// overwriting one is refused. So without this, `replace_owned` cheerfully moves
/// a live `koru-agent.exe` into `.previous/`, installs the new one, passes the
/// health check — and only then fails, at `remove_managed_dir`, *after the
/// install is already committed*, with the old agent still resident in memory
/// serving old code against new databases.
///
/// That is the whole trap: the swap does **not** fail loudly when the services
/// are still up. It fails quietly, late, and in a state nothing can undo. A
/// reviewer will eventually observe that `replace_owned` "would fail anyway" and
/// propose deleting this. It would not, and it does not.
///
/// ## Ask, then verify
///
/// Asking is `<service> stop` against **the executable already installed**,
/// this installation's own CLIs — which is why a retired name is still worth
/// asking: the file on disk predates the retirement and still has the verb.
/// They are ordinary clients: each dials the endpoint its *own*
/// `koru_channel::segment()` names, which is the same-channel rule enforced by
/// construction rather than by a check, and none can reach another channel's
/// services even if one is running. All print "not running" and succeed when
/// there is nothing there, so this is idempotent.
///
/// **The agent goes first.** It supervises the gateway and the sessions host
/// and will restart one it finds stopped, so stopping a worker first simply
/// asks the agent to put it back. Each supervised generation is then asked
/// twice with a gap, because a supervisor ensures its child on a worker thread
/// and an ask can land while the process is still binding — by which point the
/// first request has gone to nobody.
///
/// **After the cutover, the Agent's own workers need no ask at all.** They are
/// `koru-agent.exe` under a role, ended by the Job Object with their parent, and
/// they hold no file the new package replaces separately. What is still asked
/// by name is what an *older* installation may have running: a standalone
/// gateway, a standalone session host, a pre-rename voice worker, and both
/// pre-gateway Store generations.
///
/// Verifying is [`still_running`] — [`is_free`] over every executable in
/// `owned` on Windows, the process table on Linux — because asking is not
/// proof: a hung process ignores its shutdown request, and something the agent
/// never started may be holding an image.
fn quiesce(
    install: &Path,
    owned: &BTreeSet<String>,
    rehearsal_root: Option<&Path>,
    log: &mut Log,
) -> Result<(), String> {
    let images = owned_executables(install, owned);
    if still_running(install, &images).is_empty() {
        // Nothing of ours is running — the ordinary case when the requester
        // brought the stack down itself before spawning this helper.
        return Ok(());
    }

    for &(service, pause_before) in STOP_SEQUENCE {
        let binary = install.join(service);
        if !binary.is_file() {
            continue;
        }
        if pause_before {
            thread::sleep(Duration::from_millis(500));
        }
        log.line(&format!("asking {service} to stop"));
        let mut command = Command::new(&binary);
        command.args(stop_arguments(service, rehearsal_root));
        match command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            Ok(status) => log.line(&format!("{service} stop exited {status}")),
            // Not fatal on its own. The probe below is what decides, and a
            // service whose exe will not launch is one that is not running.
            Err(error) => log.line(&format!("{service} stop could not be started: {error}")),
        }
    }

    let started = Instant::now();
    loop {
        let held = still_running(install, &images);
        if held.is_empty() {
            log.line("every executable this update owns is free");
            return Ok(());
        }
        if started.elapsed() >= WAIT_FOR_SERVICES {
            // Name them. The updater log is the only diagnostic anyone gets.
            return Err(format!(
                "still running after {}s: {}",
                WAIT_FOR_SERVICES.as_secs(),
                held.join(", ")
            ));
        }
        thread::sleep(PROBE_EVERY);
    }
}

/// What still holds this installation, named for the log; empty is quiet.
///
/// Windows: every owned executable that will not open for writing.
#[cfg(not(target_os = "linux"))]
fn still_running(_install: &Path, images: &[(String, PathBuf)]) -> Vec<String> {
    images
        .iter()
        .filter(|(_, path)| !is_free(path))
        .map(|(relative, _)| relative.clone())
        .collect()
}

/// Linux: every process whose image is anywhere under the installation, by
/// [`koru_update::processes::running_from`] — process absence, as on macOS,
/// because a running image on Linux write-opens, renames and unlinks without
/// complaint, and [`is_free`] would vouch for a live service.
///
/// **The whole directory, not the owned names**: a process running from
/// `.previous/` (an image a crashed transaction moved aside) or a binary no
/// package lists still holds what is about to move. An unreadable process
/// table is reported as holding, never as quiet.
#[cfg(target_os = "linux")]
fn still_running(install: &Path, _images: &[(String, PathBuf)]) -> Vec<String> {
    match koru_update::processes::running_from(install) {
        Ok(found) => found
            .into_iter()
            .map(|(pid, image)| {
                let shown = fs::canonicalize(install)
                    .ok()
                    .and_then(|root| image.strip_prefix(root).ok().map(Path::to_path_buf))
                    .unwrap_or(image);
                format!("{} (pid {pid})", shown.display())
            })
            .collect(),
        Err(error) => vec![format!("the process table could not be read: {error}")],
    }
}

fn replace_owned(
    install: &Path,
    staging: &Path,
    backup: &Path,
    incoming: &ReleaseFiles,
    owned: &BTreeSet<String>,
    log: &mut Log,
) -> Result<(), ReplaceFailure> {
    replace_owned_with(
        install,
        staging,
        backup,
        incoming,
        owned,
        log,
        |from, to| fs::rename(from, to),
    )
}

#[derive(Debug)]
struct ReplaceFailure {
    reason: String,
    /// Every file moved before the failure was returned to its old location.
    restored: bool,
}

fn replacement_failed(reason: String, restore: Result<(), String>) -> ReplaceFailure {
    match restore {
        Ok(()) => ReplaceFailure {
            reason,
            restored: true,
        },
        Err(error) => ReplaceFailure {
            reason: format!("{reason}; partial restore failed: {error}"),
            restored: false,
        },
    }
}

/// Replace the owned set while remembering exactly what this invocation moved.
///
/// The callback exists for the fault-injection tests. Recovery itself always
/// uses the real filesystem operation: a simulated failure must exercise the
/// same restoration path production uses.
fn replace_owned_with(
    install: &Path,
    staging: &Path,
    backup: &Path,
    incoming: &ReleaseFiles,
    owned: &BTreeSet<String>,
    log: &mut Log,
    mut move_file: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), ReplaceFailure> {
    let mut backed_up = Vec::new();
    let mut installed = Vec::new();
    for relative in owned {
        let active = install.join(relative);
        if active.is_file() {
            let saved = backup.join(relative);
            if let Some(parent) = saved.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    let reason = format!("creating backup directory for {relative}: {error}");
                    return Err(replacement_failed(
                        reason,
                        restore_partial(install, backup, &installed, &backed_up, log),
                    ));
                }
            }
            if let Err(error) = move_file(&active, &saved) {
                let reason = format!("backing up {relative}: {error}");
                return Err(replacement_failed(
                    reason,
                    restore_partial(install, backup, &installed, &backed_up, log),
                ));
            }
            backed_up.push(relative.clone());
        }
    }
    for relative in &incoming.files {
        let source = staging.join(relative);
        let active = install.join(relative);
        if let Some(parent) = active.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                let reason = format!("creating install directory for {relative}: {error}");
                return Err(replacement_failed(
                    reason,
                    restore_partial(install, backup, &installed, &backed_up, log),
                ));
            }
        }
        if let Err(error) = move_file(&source, &active) {
            let reason = format!("installing {relative}: {error}");
            return Err(replacement_failed(
                reason,
                restore_partial(install, backup, &installed, &backed_up, log),
            ));
        }
        installed.push(relative.clone());
    }
    Ok(())
}

/// Undo only the paths this replacement actually moved.
///
/// Calling the broad final-state [`rollback`] after a backup-loop failure is
/// unsafe: incoming-named old files that the loop had not reached are still in
/// place, and deleting every incoming name would delete those untouched old
/// files. This progress list is the distinction between "old and untouched"
/// and "new and installed".
fn restore_partial(
    install: &Path,
    backup: &Path,
    installed: &[String],
    backed_up: &[String],
    log: &mut Log,
) -> Result<(), String> {
    for relative in installed.iter().rev() {
        let active = install.join(relative);
        if active.is_file() {
            fs::remove_file(&active).map_err(|e| format!("removing new {relative}: {e}"))?;
        }
    }
    for relative in backed_up.iter().rev() {
        let saved = backup.join(relative);
        if !saved.is_file() {
            continue;
        }
        let active = install.join(relative);
        if let Some(parent) = active.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::copy(&saved, &active).map_err(|e| format!("restoring {relative}: {e}"))?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&active)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("flushing restored {relative}: {e}"))?;
    }
    log.line("partial application replacement restored");
    Ok(())
}

fn rollback(
    install: &Path,
    backup: &Path,
    incoming: &[String],
    log: &mut Log,
) -> Result<(), String> {
    for relative in incoming {
        let active = install.join(relative);
        if active.is_file() {
            fs::remove_file(active).map_err(|e| e.to_string())?;
        }
    }
    if backup.is_dir() {
        let mut files = collect_files(backup)?;
        files.sort();
        for source in files {
            let relative = source.strip_prefix(backup).map_err(|e| e.to_string())?;
            let active = install.join(relative);
            if let Some(parent) = active.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(&source, &active).map_err(|e| e.to_string())?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&active)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    log.line("previous application files restored");
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                dirs.push(entry.path());
            } else {
                out.push(entry.path());
            }
        }
    }
    Ok(out)
}

fn launch(
    executable: &Path,
    health_token: Option<&str>,
    rehearsal_root: Option<&Path>,
    rehearsal_manifest: Option<&Path>,
) -> Result<Child, String> {
    let mut command = Command::new(executable);
    command.args(launch_arguments(
        health_token,
        rehearsal_root,
        rehearsal_manifest,
    ));
    command.spawn().map_err(|e| e.to_string())
}

fn wait_for_health(child: &mut Child, health: &Path, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if health_confirmed(health)? {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!(
                "updated Koru exited before health confirmation: {status}"
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("health_check_timeout".into())
}

fn wait_for_exit(pid: u32, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !process_exists(pid) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("Koru did not exit before the updater timeout".into())
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

/// Whether `pid` is still running.
///
/// **`/proc` is a Linux fact, not a Unix one.** This asked
/// `/proc/<pid>`.exists(), which on macOS is always `false` — so
/// [`wait_for_exit`] returned `Ok(())` for a *live* Koru on the first poll and
/// the updater went on to overwrite the installation it was still running from.
/// Silent, and the worst shape of bug this codebase has: it loses content.
///
/// `kill(pid, 0)` performs no signal delivery and only runs the error checks.
/// `EPERM` means the process exists and is not ours to signal, which is still
/// *exists* — the distinction matters more than it looks, since answering
/// "gone" is what causes the damage. Anything else (`ESRCH`) is gone.
///
/// Two caveats, neither worth code. A pid can be reused, but the window here is
/// one short timeout and the Windows path has the same hole. A zombie answers
/// "exists" until its parent reaps it — the updater is Koru's *child*, so
/// Koru's parent is the shell or launchd, both of which reap.
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 delivers nothing and touches no memory; it
    // is a pure permission-and-existence probe on an integer.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Neither Windows nor Unix: refuse to claim the process has gone.
///
/// `true` is the safe answer — it costs a timeout, where `false` costs the
/// installation.
#[cfg(not(any(windows, unix)))]
fn process_exists(_pid: u32) -> bool {
    true
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct TransactionJournal {
    schema_version: u32,
    current_build: u64,
    target_build: u64,
    staging: PathBuf,
    backup: PathBuf,
    owned_files: BTreeSet<String>,
    incoming_files: Vec<String>,
    health_token: String,
}

fn write_journal(
    path: &Path,
    request: &UpdaterRequest,
    staging: &Path,
    backup: &Path,
    owned: &BTreeSet<String>,
    incoming: &[String],
) -> Result<(), String> {
    let value = TransactionJournal {
        schema_version: 2,
        current_build: request.current_build,
        target_build: request.target_build,
        staging: staging.to_path_buf(),
        backup: backup.to_path_buf(),
        owned_files: owned.clone(),
        incoming_files: incoming.to_vec(),
        health_token: request.health_token.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let parent = path
        .parent()
        .ok_or("the update journal has no parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".transaction-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("creating update journal: {e}"))?;
        file.write_all(&bytes)
            .map_err(|e| format!("writing update journal: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("flushing update journal: {e}"))?;
        fs::rename(&temporary, path).map_err(|e| format!("committing update journal: {e}"))?;
        // Flush through the final name as well. On Windows this commits the
        // renamed file's data and metadata; on Unix the directory entry also
        // needs its own sync below.
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("flushing committed update journal: {e}"))?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| format!("flushing update journal directory: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Recover an interrupted Windows file transaction before starting another.
///
/// The journal is written only after staging is complete and before the first
/// active file moves. Therefore a missing staged incoming path means this
/// updater moved it into the installation, while a present backup path means
/// it moved that old file aside. Those two filesystem facts reconstruct exact
/// progress even if power failed between a rename and any later bookkeeping.
fn recover_interrupted_files(
    install: &Path,
    paths: &UpdatePaths,
    rehearsal_root: Option<&Path>,
    journal_path: &Path,
    log: &mut Log,
) -> Result<RecoveryOutcome, String> {
    if !journal_path.is_file() {
        return Ok(RecoveryOutcome::Ready);
    }
    let bytes = fs::read(journal_path).map_err(|e| format!("reading update journal: {e}"))?;
    let journal: TransactionJournal =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid update journal: {e}"))?;
    if journal.schema_version != 2 || journal.incoming_files.is_empty() {
        return Err(format!(
            "unsupported update journal schema {}; manual recovery is required",
            journal.schema_version
        ));
    }
    validate_journal(&journal)?;
    ensure_managed_path(install, &journal.staging, ".update")?;
    ensure_managed_path(install, &journal.backup, ".previous")?;

    let health = paths
        .health_dir()
        .join(format!("{}.ok", journal.health_token));
    let committed = proof_path(
        journal_path,
        &journal.health_token,
        TransactionProof::Committed,
    );
    let restored = proof_path(
        journal_path,
        &journal.health_token,
        TransactionProof::Restored,
    );
    let committed_exists = proof_exists(&committed, TransactionProof::Committed)?;
    let restored_exists = proof_exists(&restored, TransactionProof::Restored)?;
    if committed_exists && restored_exists {
        return Err("update journal has conflicting committed and restored proofs".into());
    }
    if committed_exists || restored_exists {
        let proof = if committed_exists {
            TransactionProof::Committed
        } else {
            TransactionProof::Restored
        };
        finish_transaction(
            install,
            &journal.staging,
            &journal.backup,
            journal_path,
            &health,
            &journal.health_token,
            proof,
        )?;
        log.line("completed cleanup for an interrupted proven update state");
        return Ok(RecoveryOutcome::Ready);
    }
    if health_confirmed(&health)? {
        // Health is the commit point. A helper crash after observing it must
        // finish cleanup, not roll a ready build back on the next update.
        finish_transaction(
            install,
            &journal.staging,
            &journal.backup,
            journal_path,
            &health,
            &journal.health_token,
            TransactionProof::Committed,
        )?;
        log.line("completed cleanup for an interrupted committed update");
        return Ok(RecoveryOutcome::Ready);
    }

    quiesce(install, &journal.owned_files, rehearsal_root, log)?;
    let installed: Vec<String> = journal
        .incoming_files
        .iter()
        .filter(|relative| !journal.staging.join(relative).is_file())
        .cloned()
        .collect();
    let backed_up: Vec<String> = journal
        .owned_files
        .iter()
        .filter(|relative| journal.backup.join(relative).is_file())
        .cloned()
        .collect();
    restore_partial(install, &journal.backup, &installed, &backed_up, log)?;
    let finish = finish_transaction(
        install,
        &journal.staging,
        &journal.backup,
        journal_path,
        &health,
        &journal.health_token,
        TransactionProof::Restored,
    );
    match finish {
        Ok(()) => {
            log.line("recovered an interrupted uncommitted update");
            Ok(RecoveryOutcome::Ready)
        }
        Err(error) => Ok(RecoveryOutcome::RestoredUnproven(format!(
            "the interrupted update was restored, but its transaction proof could not be persisted: {error}"
        ))),
    }
}

fn ensure_managed_path(install: &Path, path: &Path, expected_root: &str) -> Result<(), String> {
    let relative = path
        .strip_prefix(install)
        .map_err(|_| "managed updater path escaped the installation")?;
    let components: Vec<_> = relative.components().collect();
    if components.len() < 2
        || components[0].as_os_str().to_str() != Some(expected_root)
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("managed updater path has the wrong root".into());
    }
    Ok(())
}

fn validate_journal(journal: &TransactionJournal) -> Result<(), String> {
    if journal.health_token.len() != 32
        || !journal
            .health_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("update journal contains an invalid health token".into());
    }
    for relative in journal
        .owned_files
        .iter()
        .chain(journal.incoming_files.iter())
    {
        koru_update::strict_relative_path(relative)?;
    }
    Ok(())
}

fn remove_managed_dir(install: &Path, path: &Path, expected_root: &str) -> Result<(), String> {
    ensure_managed_path(install, path, expected_root)?;
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn record_failure(paths: &UpdatePaths, build_number: u64, reason: &str) {
    let mut state = koru_update::UpdateState::load(&paths.state_file());
    state.failed_build = Some(FailedBuild {
        build_number,
        failed_at: timestamp(),
        reason: reason.to_string(),
    });
    let _ = state.save(&paths.state_file());
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

struct Log(Option<File>);

impl Log {
    fn open(path: &Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self(OpenOptions::new().create(true).append(true).open(path).ok())
    }

    fn line(&mut self, message: &str) {
        if let Some(file) = &mut self.0 {
            let _ = writeln!(file, "{} {message}", timestamp());
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooted_request(root: &Path, install: &Path, package: &Path) -> (PathBuf, UpdaterRequest) {
        let helper = root.join("agent/updates/cache/updates/helpers/token");
        fs::create_dir_all(&helper).unwrap();
        let request_path = helper.join("request.json");
        fs::write(&request_path, b"{}").unwrap();
        let restart = install.join(koru_update::APP_EXE);
        fs::write(&restart, b"exe").unwrap();
        (
            request_path,
            UpdaterRequest {
                package: package.to_path_buf(),
                install_dir: install.to_path_buf(),
                restart,
                current_build: 1,
                target_build: 2,
                package_size: 1,
                expected_sha256: "00".repeat(32),
                signature: "test".into(),
                health_token: TEST_HEALTH_TOKEN.into(),
                root: Some(root.to_path_buf()),
                rehearsal_root: None,
                rehearsal_manifest: None,
            },
        )
    }

    #[test]
    fn a_rooted_request_cannot_name_a_shared_or_other_roots_bundle() {
        let root_a = tempfile::tempdir().unwrap();
        let root_b = tempfile::tempdir().unwrap();
        let install_a = root_a.path().join("copied-bundle");
        let install_b = root_b.path().join("copied-bundle");
        fs::create_dir(&install_a).unwrap();
        fs::create_dir(&install_b).unwrap();
        let package_a = root_a.path().join("package.zip");
        fs::write(&package_a, b"z").unwrap();
        let (request_path, request) = rooted_request(root_a.path(), &install_a, &package_a);

        validate_request(&request).unwrap();
        validate_root_scope(&request_path, &request).unwrap();

        let mut crossed = request.clone();
        crossed.install_dir = install_b.clone();
        crossed.restart = install_b.join(koru_update::APP_EXE);
        fs::write(&crossed.restart, b"exe").unwrap();
        assert!(validate_root_scope(&request_path, &crossed).is_err());

        let outside = tempfile::tempdir().unwrap();
        let shared = outside.path().join("checkout-target");
        fs::create_dir(&shared).unwrap();
        crossed.install_dir = shared.clone();
        crossed.restart = shared.join(koru_update::APP_EXE);
        fs::write(&crossed.restart, b"exe").unwrap();
        assert!(validate_root_scope(&request_path, &crossed).is_err());
        assert!(
            !UpdatePaths::under(root_a.path()).data_dir.exists(),
            "refusal happened before update paths were created"
        );
    }

    #[test]
    fn rooted_package_and_handoff_must_share_the_selected_root() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("copied-bundle");
        fs::create_dir(&install).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let package = outside.path().join("package.zip");
        fs::write(&package, b"z").unwrap();
        let (request_path, request) = rooted_request(root.path(), &install, &package);
        let error = validate_root_scope(&request_path, &request).unwrap_err();
        assert!(error.contains("package"), "{error}");

        let package = root.path().join("package.zip");
        fs::write(&package, b"z").unwrap();
        let (_, request) = rooted_request(root.path(), &install, &package);
        let outside_request = outside.path().join("request.json");
        fs::write(&outside_request, b"{}").unwrap();
        let error = validate_root_scope(&outside_request, &request).unwrap_err();
        assert!(error.contains("request"), "{error}");
    }

    #[test]
    fn two_rooted_install_transactions_replace_rollback_and_clean_independently() {
        struct Fixture {
            _root: tempfile::TempDir,
            install: PathBuf,
            staging: PathBuf,
            backup: PathBuf,
            outside_sentinel: PathBuf,
        }
        fn fixture(label: &str) -> Fixture {
            let root = tempfile::tempdir().unwrap();
            let install = root.path().join("copied-bundle");
            let staging = install.join(format!(".update/staging-{label}"));
            let backup = install.join(format!(".previous/build-{label}"));
            fs::create_dir_all(&staging).unwrap();
            fs::write(install.join("koru.exe"), format!("old-{label}")).unwrap();
            fs::write(staging.join("koru.exe"), format!("new-{label}")).unwrap();
            let outside_sentinel = root.path().join("outside-sentinel");
            fs::write(&outside_sentinel, label).unwrap();
            Fixture {
                _root: root,
                install,
                staging,
                backup,
                outside_sentinel,
            }
        }
        fn replace(fixture: &Fixture) {
            let incoming = ReleaseFiles {
                schema_version: 1,
                inventory_version: None,
                build_number: 2,
                files: vec!["koru.exe".into()],
            };
            replace_owned(
                &fixture.install,
                &fixture.staging,
                &fixture.backup,
                &incoming,
                &owned(&["koru.exe"]),
                &mut Log(None),
            )
            .unwrap();
        }
        fn roll_back_and_clean(fixture: &Fixture) {
            rollback(
                &fixture.install,
                &fixture.backup,
                &["koru.exe".into()],
                &mut Log(None),
            )
            .unwrap();
            remove_managed_dir(&fixture.install, &fixture.staging, ".update").unwrap();
        }

        let a = fixture("a");
        let b = fixture("b");
        replace(&a);
        assert_eq!(
            fs::read_to_string(a.install.join("koru.exe")).unwrap(),
            "new-a"
        );
        assert_eq!(
            fs::read_to_string(b.install.join("koru.exe")).unwrap(),
            "old-b"
        );
        assert!(b.staging.join("koru.exe").is_file());

        roll_back_and_clean(&a);
        assert_eq!(
            fs::read_to_string(a.install.join("koru.exe")).unwrap(),
            "old-a"
        );
        assert!(a.outside_sentinel.is_file());
        assert!(b.outside_sentinel.is_file());

        replace(&b);
        roll_back_and_clean(&b);
        assert_eq!(
            fs::read_to_string(b.install.join("koru.exe")).unwrap(),
            "old-b"
        );
        assert!(a.outside_sentinel.is_file());
        assert!(b.outside_sentinel.is_file());
    }

    const TEST_HEALTH_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    /// A real child for the startup-observation tests below. The test harness
    /// is already a platform-native executable, so using it avoids a shell or
    /// a system utility whose name and exit behavior differ by target.
    #[test]
    fn startup_probe_child() {
        let Some(milliseconds) = std::env::var_os("UPDATER_STARTUP_PROBE_CHILD_MS") else {
            return;
        };
        let milliseconds = milliseconds
            .to_string_lossy()
            .parse::<u64>()
            .expect("probe child duration");
        thread::sleep(Duration::from_millis(milliseconds));
    }

    fn spawn_startup_probe_child(milliseconds: u64) -> Child {
        Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "install::tests::startup_probe_child",
                "--nocapture",
            ])
            .env("UPDATER_STARTUP_PROBE_CHILD_MS", milliseconds.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start probe child")
    }

    #[test]
    fn an_immediate_exit_is_not_a_successful_restored_restart() {
        let mut child = spawn_startup_probe_child(0);
        let error = survives_startup(&mut child, Duration::from_millis(500))
            .expect_err("an exited child is not live recovery");
        assert!(error.contains("exited during startup"), "{error}");
        let _ = child.wait();
    }

    #[test]
    fn a_child_that_survives_the_observation_window_is_a_successful_restart() {
        let mut child = spawn_startup_probe_child(500);
        survives_startup(&mut child, Duration::from_millis(100))
            .expect("child survives the startup window");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn health_requires_the_complete_marker_and_stale_health_can_be_retired() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("health.ok");
        assert!(!health_confirmed(&marker).unwrap());

        for incomplete in [b"".as_slice(), b"o", b"ok", b"ok\r\n", b"ok\nextra"] {
            fs::write(&marker, incomplete).unwrap();
            assert!(
                !health_confirmed(&marker).unwrap(),
                "an incomplete or non-canonical marker was accepted: {incomplete:?}"
            );
        }

        fs::write(&marker, b"ok\n").unwrap();
        assert!(health_confirmed(&marker).unwrap());
        retire_health_marker(&marker).unwrap();
        assert!(
            !health_confirmed(&marker).unwrap(),
            "a valid marker left by an earlier launch must not satisfy a new wait"
        );
    }

    /// The macOS geometry rule: the request names `Contents/MacOS`, and the
    /// transaction derives the bundle and its container from it — refusing
    /// anything that is not actually inside a `.app`, because two renames of
    /// the wrong directory is an uninstaller.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_bundle_is_derived_from_contents_macos_and_nothing_else() {
        let (bundle, container) =
            macos::bundle_of(Path::new("/Applications/Koru.app/Contents/MacOS")).unwrap();
        assert_eq!(bundle, Path::new("/Applications/Koru.app"));
        assert_eq!(container, Path::new("/Applications"));
        for wrong in [
            "/Applications/Koru.app/Contents",
            "/Applications/Koru.app",
            "/Applications/Koru/Contents/MacOS",
            "/Applications",
        ] {
            assert!(macos::bundle_of(Path::new(wrong)).is_err(), "{wrong}");
        }
    }

    /// The macOS quiesce proof is process absence, asserted against a real
    /// process running from under a bundle-shaped directory — the twin of
    /// `a_running_executable_is_not_free`, for the platform where that probe
    /// proves nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_under_the_bundle_is_not_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Koru.app");
        let binaries = bundle.join("Contents/MacOS");
        fs::create_dir_all(&binaries).unwrap();
        // A real executable with a bundle-resident path: /bin/sleep, copied in.
        let sleeper = binaries.join("koru-fake-service");
        fs::copy("/bin/sleep", &sleeper).expect("copy a real executable");
        assert!(
            macos::bundle_is_quiet(&bundle),
            "quiet before anything runs"
        );

        let mut child = Command::new(&sleeper)
            .arg("30")
            .spawn()
            .expect("run the sleeper");
        // pgrep sees the process table with no settling delay needed, but the
        // spawn itself can still be mid-exec on a loaded machine.
        let started = Instant::now();
        while macos::bundle_is_quiet(&bundle) && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !macos::bundle_is_quiet(&bundle),
            "a running process under the bundle must be seen"
        );
        // And the platform fact that makes absence the only valid proof:
        // renaming a directory holding a running image succeeds on macOS.
        let renamed = dir.path().join("Koru-moved.app");
        fs::rename(&bundle, &renamed).expect("macOS renames a running image's home");
        fs::rename(&renamed, &bundle).expect("and back");

        child.kill().expect("stop the sleeper");
        child.wait().expect("reap it");
        let started = Instant::now();
        while !macos::bundle_is_quiet(&bundle) && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(macos::bundle_is_quiet(&bundle), "quiet again once it exits");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_quiescence_treats_rehearsal_root_metacharacters_literally() {
        assert_eq!(
            macos::ere_literal("/tmp/Koru [A]+(test).app"),
            "/tmp/Koru \\[A\\]\\+\\(test\\)\\.app"
        );
    }

    #[test]
    fn managed_delete_can_only_name_the_expected_child_tree() {
        let install = Path::new("C:/Koru");
        assert!(
            ensure_managed_path(install, &install.join(".update/staging-2"), ".update").is_ok()
        );
        assert!(ensure_managed_path(install, &install.join("assets"), ".update").is_err());
        assert!(ensure_managed_path(install, Path::new("C:/Other"), ".update").is_err());
        assert!(
            ensure_managed_path(
                install,
                &install.join(".update/staging-2/../../outside"),
                ".update"
            )
            .is_err()
        );
        assert!(ensure_managed_path(install, &install.join(".update"), ".update").is_err());
    }

    #[test]
    fn a_corrupted_journal_cannot_name_paths_or_health_outside_the_transaction() {
        let journal = TransactionJournal {
            schema_version: 2,
            current_build: 1,
            target_build: 2,
            staging: PathBuf::from("C:/Koru/.update/staging-2"),
            backup: PathBuf::from("C:/Koru/.previous/build-1"),
            owned_files: owned(&["koru.exe"]),
            incoming_files: vec!["../outside".into()],
            health_token: TEST_HEALTH_TOKEN.into(),
        };
        assert!(validate_journal(&journal).is_err());
        let mut bad_token = journal;
        bad_token.incoming_files = vec!["koru.exe".into()];
        bad_token.health_token = "../../outside-health-marker".into();
        assert!(validate_journal(&bad_token).is_err());
    }

    #[test]
    fn rollback_preserves_unlisted_files() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Koru");
        let backup = install.join(".previous/build-1-test");
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("koru.exe"), b"old").unwrap();
        fs::write(install.join("koru.exe"), b"new").unwrap();
        fs::write(install.join("my-notes.txt"), b"mine").unwrap();
        let mut log = Log(None);
        rollback(&install, &backup, &["koru.exe".into()], &mut log).unwrap();
        assert_eq!(fs::read(install.join("koru.exe")).unwrap(), b"old");
        assert_eq!(fs::read(install.join("my-notes.txt")).unwrap(), b"mine");
    }

    fn replacement_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        PathBuf,
        ReleaseFiles,
        BTreeSet<String>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Koru");
        let staging = install.join(".update/staging-2-test");
        let backup = install.join(".previous/build-1-test");
        fs::create_dir_all(&staging).unwrap();
        let files = ["a.txt", "b.txt", "c.txt"];
        for file in files {
            fs::write(install.join(file), format!("old-{file}")).unwrap();
            fs::write(staging.join(file), format!("new-{file}")).unwrap();
        }
        let incoming = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 2,
            files: files.into_iter().map(str::to_owned).collect(),
        };
        let owned = incoming.files.iter().cloned().collect();
        (dir, install, staging, backup, incoming, owned)
    }

    fn assert_old_fixture(install: &Path) {
        for file in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(
                fs::read_to_string(install.join(file)).unwrap(),
                format!("old-{file}"),
                "{file} was not restored exactly"
            );
        }
    }

    /// A failure halfway through moving old files aside must not feed the broad
    /// rollback path: names later in the set are untouched old files, not new
    /// files to delete.
    #[test]
    fn partial_backup_failure_restores_every_old_file() {
        let (_dir, install, staging, backup, incoming, owned) = replacement_fixture();
        let mut move_number = 0;
        let mut log = Log(None);
        let result = replace_owned_with(
            &install,
            &staging,
            &backup,
            &incoming,
            &owned,
            &mut log,
            |from, to| {
                move_number += 1;
                if move_number == 2 {
                    Err(std::io::Error::other("injected backup failure"))
                } else {
                    fs::rename(from, to)
                }
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().restored);
        assert_old_fixture(&install);
    }

    /// The same guarantee after all backups and some incoming files have moved:
    /// remove exactly those new files, then restore exactly those old files.
    #[test]
    fn partial_install_failure_restores_every_old_file() {
        let (_dir, install, staging, backup, incoming, owned) = replacement_fixture();
        let mut move_number = 0;
        let mut log = Log(None);
        let result = replace_owned_with(
            &install,
            &staging,
            &backup,
            &incoming,
            &owned,
            &mut log,
            |from, to| {
                move_number += 1;
                if move_number == 5 {
                    Err(std::io::Error::other("injected install failure"))
                } else {
                    fs::rename(from, to)
                }
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().restored);
        assert_old_fixture(&install);
    }

    /// An internally restored replace failure is no longer a crash transaction.
    /// Leaving its journal behind made the next updater treat files missing
    /// from staging as still-installed new files and delete the restored old
    /// copies.
    #[test]
    fn restored_replace_failure_cannot_corrupt_the_next_recovery() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        let journal = install.join(".update/transaction.json");
        fs::write(&journal, b"stale journal").unwrap();
        let mut move_number = 0;
        let mut log = Log(None);
        let failure = replace_owned_with(
            &install,
            &staging,
            &backup,
            &incoming,
            &owned,
            &mut log,
            |from, to| {
                move_number += 1;
                if move_number == 5 {
                    Err(std::io::Error::other("injected install failure"))
                } else {
                    fs::rename(from, to)
                }
            },
        )
        .unwrap_err();
        assert!(failure.restored);
        finish_transaction(
            &install,
            &staging,
            &backup,
            &journal,
            &temp_update_paths(dir.path())
                .health_dir()
                .join(format!("{TEST_HEALTH_TOKEN}.ok")),
            TEST_HEALTH_TOKEN,
            TransactionProof::Restored,
        )
        .unwrap();

        let paths = temp_update_paths(dir.path());
        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();
        assert_old_fixture(&install);
        assert!(!journal.exists());
    }

    fn temp_update_paths(root: &Path) -> UpdatePaths {
        UpdatePaths {
            config_dir: root.join("update/config"),
            data_dir: root.join("update/data"),
            cache_dir: root.join("update/cache"),
            logs_dir: root.join("update/logs"),
        }
    }

    fn write_test_journal(
        path: &Path,
        staging: &Path,
        backup: &Path,
        incoming: &ReleaseFiles,
        owned: &BTreeSet<String>,
        health_token: &str,
    ) {
        let journal = TransactionJournal {
            schema_version: 2,
            current_build: 1,
            target_build: 2,
            staging: staging.to_path_buf(),
            backup: backup.to_path_buf(),
            owned_files: owned.clone(),
            incoming_files: incoming.files.clone(),
            health_token: health_token.into(),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
    }

    /// The journal plus filesystem reconstruct progress after a process/power
    /// failure; no in-memory counter is required to restore the exact old set.
    #[test]
    fn a_crashed_partial_replacement_is_recovered_before_the_next_update() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        fs::create_dir_all(&backup).unwrap();
        fs::rename(install.join("a.txt"), backup.join("a.txt")).unwrap();
        fs::rename(staging.join("a.txt"), install.join("a.txt")).unwrap();
        fs::rename(install.join("b.txt"), backup.join("b.txt")).unwrap();
        let journal = install.join(".update/transaction.json");
        write_test_journal(
            &journal,
            &staging,
            &backup,
            &incoming,
            &owned,
            TEST_HEALTH_TOKEN,
        );
        let paths = temp_update_paths(dir.path());
        let mut log = Log(None);

        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

        assert_old_fixture(&install);
        assert!(!journal.exists());
        assert!(!staging.exists());
        assert!(!backup.exists());
    }

    #[test]
    fn an_interrupted_temporary_proof_never_masks_recoverable_journal_state() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        fs::create_dir_all(&backup).unwrap();
        fs::rename(install.join("a.txt"), backup.join("a.txt")).unwrap();
        fs::rename(staging.join("a.txt"), install.join("a.txt")).unwrap();
        let journal = install.join(".update/transaction.json");
        write_test_journal(
            &journal,
            &staging,
            &backup,
            &incoming,
            &owned,
            TEST_HEALTH_TOKEN,
        );
        fs::write(
            journal
                .parent()
                .unwrap()
                .join(".transaction-proof-crashed.tmp"),
            b"comm",
        )
        .unwrap();
        let paths = temp_update_paths(dir.path());
        let mut log = Log(None);

        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

        assert_old_fixture(&install);
        assert!(!journal.exists());
    }

    /// Rollback keeps its old-file copies until a durable Restored proof. A
    /// crash after copying only some of them can therefore be replayed without
    /// mistaking the copied old active file for a committed new file.
    #[test]
    fn a_crash_during_rollback_replays_from_the_complete_backup() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        fs::create_dir_all(&backup).unwrap();
        for file in &incoming.files {
            fs::rename(install.join(file), backup.join(file)).unwrap();
            fs::rename(staging.join(file), install.join(file)).unwrap();
        }
        fs::copy(backup.join("a.txt"), install.join("a.txt")).unwrap();
        let journal = install.join(".update/transaction.json");
        write_test_journal(
            &journal,
            &staging,
            &backup,
            &incoming,
            &owned,
            TEST_HEALTH_TOKEN,
        );
        let paths = temp_update_paths(dir.path());
        let mut log = Log(None);

        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

        assert_old_fixture(&install);
        assert!(!journal.exists());
    }

    /// Health is the commit point. If the helper dies after the new Agent
    /// writes it, recovery completes cleanup and never restores the old build.
    #[test]
    fn a_crash_after_health_keeps_the_committed_new_files() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        fs::create_dir_all(&backup).unwrap();
        for file in &incoming.files {
            fs::rename(install.join(file), backup.join(file)).unwrap();
            fs::rename(staging.join(file), install.join(file)).unwrap();
        }
        let journal = install.join(".update/transaction.json");
        write_test_journal(
            &journal,
            &staging,
            &backup,
            &incoming,
            &owned,
            TEST_HEALTH_TOKEN,
        );
        let paths = temp_update_paths(dir.path());
        paths.signal_health(TEST_HEALTH_TOKEN).unwrap();
        let marker = paths.health_dir().join(format!("{TEST_HEALTH_TOKEN}.ok"));
        let mut log = Log(None);

        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

        for file in &incoming.files {
            assert_eq!(
                fs::read_to_string(install.join(file)).unwrap(),
                format!("new-{file}")
            );
        }
        assert!(!journal.exists());
        assert!(!backup.exists());
        assert!(!marker.exists(), "health retires before the journal");
        assert!(
            proof_path(&journal, TEST_HEALTH_TOKEN, TransactionProof::Committed).is_file(),
            "the durable decision remains after ordinary cleanup"
        );
    }

    #[test]
    fn empty_and_torn_health_markers_cannot_commit_an_interrupted_update() {
        for marker_bytes in [b"".as_slice(), b"o"] {
            let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
            fs::create_dir_all(&backup).unwrap();
            for file in &incoming.files {
                fs::rename(install.join(file), backup.join(file)).unwrap();
                fs::rename(staging.join(file), install.join(file)).unwrap();
            }
            let journal = install.join(".update/transaction.json");
            write_test_journal(
                &journal,
                &staging,
                &backup,
                &incoming,
                &owned,
                TEST_HEALTH_TOKEN,
            );
            let paths = temp_update_paths(dir.path());
            let marker = paths.health_dir().join(format!("{TEST_HEALTH_TOKEN}.ok"));
            fs::create_dir_all(paths.health_dir()).unwrap();
            fs::write(&marker, marker_bytes).unwrap();
            let mut log = Log(None);

            recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

            assert_old_fixture(&install);
            assert!(!journal.exists());
            assert!(!marker.exists());
            assert!(proof_path(&journal, TEST_HEALTH_TOKEN, TransactionProof::Restored).is_file());
        }
    }

    #[test]
    fn a_durable_commit_proof_preserves_new_files_after_cleanup_reordering() {
        let (dir, install, staging, backup, incoming, owned) = replacement_fixture();
        fs::create_dir_all(&backup).unwrap();
        for file in &incoming.files {
            fs::rename(install.join(file), backup.join(file)).unwrap();
            fs::rename(staging.join(file), install.join(file)).unwrap();
        }
        let journal = install.join(".update/transaction.json");
        write_test_journal(
            &journal,
            &staging,
            &backup,
            &incoming,
            &owned,
            TEST_HEALTH_TOKEN,
        );
        persist_proof(
            &proof_path(&journal, TEST_HEALTH_TOKEN, TransactionProof::Committed),
            TransactionProof::Committed,
        )
        .unwrap();
        fs::remove_dir_all(&backup).unwrap();
        fs::remove_dir_all(&staging).unwrap();
        let paths = temp_update_paths(dir.path());
        let mut log = Log(None);

        recover_interrupted_files(&install, &paths, None, &journal, &mut log).unwrap();

        for file in &incoming.files {
            assert_eq!(
                fs::read_to_string(install.join(file)).unwrap(),
                format!("new-{file}")
            );
        }
    }

    fn owned(files: &[&str]) -> BTreeSet<String> {
        files.iter().map(|file| (*file).to_string()).collect()
    }

    #[test]
    fn package_change_keeps_removed_files_in_the_owned_union() {
        let installed = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 40,
            files: vec!["koru.exe".into(), "old-helper.exe".into()],
        };
        let incoming = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 41,
            files: vec!["koru.exe".into(), "koru-api-gateway.exe".into()],
        };

        let union = owned_files(&incoming, Some(&installed));

        assert!(union.contains("old-helper.exe"));
        assert!(union.contains("koru-api-gateway.exe"));
    }

    /// Only the executables, and by extension rather than by a list somebody has
    /// to remember to extend.
    #[test]
    fn quiescing_concerns_itself_with_executables_and_nothing_else() {
        let install = Path::new("C:/Koru");
        let images = owned_executables(
            install,
            &owned(&[
                "koru.exe",
                "koru-agent.exe",
                "koru-store-server.exe",
                "koru-session-host.exe",
                "koru-terminal.exe",
                "koru-updater.exe",
                "VERSION",
                "assets/fonts/Inter.ttf",
                "koru.bat",
            ]),
        );
        let named: Vec<&str> = images.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            named,
            [
                "koru-agent.exe",
                "koru-session-host.exe",
                "koru-store-server.exe",
                "koru-terminal.exe",
                "koru-updater.exe",
                "koru.exe"
            ],
            "every .exe, in the set's order, and no data file"
        );
        assert_eq!(images[0].1, install.join("koru-agent.exe"));
    }

    #[test]
    fn the_stop_sweep_asks_every_supported_store_generation() {
        assert!(STOP_SEQUENCE.contains(&(koru_update::AGENT_EXE, false)));
        assert!(STOP_SEQUENCE.contains(&(koru_update::AI_CHAT_EXE, false)));
        let agent = STOP_SEQUENCE
            .iter()
            .position(|(name, _)| *name == koru_update::AGENT_EXE)
            .unwrap();
        let chat = STOP_SEQUENCE
            .iter()
            .position(|(name, _)| *name == koru_update::AI_CHAT_EXE)
            .unwrap();
        let gateway = STOP_SEQUENCE
            .iter()
            .position(|(name, _)| *name == koru_update::GATEWAY_EXE)
            .unwrap();
        // **The voice worker is asked after the Agent**, because the Agent is
        // the only thing that starts one: asking first would be asking a
        // supervisor to put it back.
        let voice = STOP_SEQUENCE
            .iter()
            .position(|(name, _)| *name == koru_update::VOICE_EXE)
            .expect("the voice worker holds its own executable and must be asked to stop");
        assert!(agent < chat && chat < gateway);
        assert!(agent < voice);
        assert_eq!(
            STOP_SEQUENCE
                .iter()
                .filter(|(name, _)| *name == koru_update::SESSION_HOST_EXE)
                .count(),
            2
        );
        assert!(
            STOP_SEQUENCE
                .iter()
                .all(|(name, _)| *name != koru_update::TERMINAL_EXE),
            "the retired Terminal was a window closed through Agent presence, never a stop service"
        );
        assert_eq!(
            STOP_SEQUENCE
                .iter()
                .filter(|(name, _)| *name == koru_update::GATEWAY_EXE)
                .count(),
            2
        );
        assert_eq!(
            STOP_SEQUENCE
                .iter()
                .filter(|(name, _)| *name == koru_update::LEGACY_STORE_SERVER_EXE)
                .count(),
            2
        );
        assert_eq!(
            STOP_SEQUENCE
                .iter()
                .filter(|(name, _)| *name == koru_update::LEGACY_STORE_EXE)
                .count(),
            2
        );
        // **The pre-rename voice worker.** It answers the same `stop` verb the
        // current one does — the rename retired the file name, not the program
        // — and until Plan 7 nothing here named it, so an upgrade that found
        // one running waited out the whole window and then failed.
        assert_eq!(
            STOP_SEQUENCE
                .iter()
                .filter(|(name, _)| *name == koru_update::LEGACY_VOICE_EXE)
                .count(),
            2,
            "the retired koru-voice must be asked, and asked again after a pause"
        );
        let legacy_voice = STOP_SEQUENCE
            .iter()
            .position(|(name, _)| *name == koru_update::LEGACY_VOICE_EXE)
            .unwrap();
        assert!(agent < legacy_voice, "a supervisor would put one back");
        // Every retired generation is asked twice: once at once, once after a
        // pause, so an ask cannot land on a process that is still binding.
        for retired in [
            koru_update::GATEWAY_EXE,
            koru_update::SESSION_HOST_EXE,
            koru_update::LEGACY_VOICE_EXE,
            koru_update::LEGACY_STORE_SERVER_EXE,
            koru_update::LEGACY_STORE_EXE,
        ] {
            assert!(
                STOP_SEQUENCE.contains(&(retired, false))
                    && STOP_SEQUENCE.contains(&(retired, true)),
                "{retired} needs an immediate ask and a paused one"
            );
        }
    }

    /// **Verification case 8, the half a machine can prove: the ask.**
    ///
    /// An installation whose standalone gateway and session host are running is
    /// upgraded to a package that has neither. Both must be *asked* to stop —
    /// they are gone from every current list, so nothing else names them — and
    /// neither may be treated as something this updater would restart into.
    #[test]
    fn an_upgrade_still_asks_the_retired_gateway_and_session_host_to_stop() {
        for retired in [koru_update::GATEWAY_EXE, koru_update::SESSION_HOST_EXE] {
            assert!(
                STOP_SEQUENCE.iter().any(|(name, _)| *name == retired),
                "{retired} left the package and must still be asked to stop"
            );
            assert!(
                !koru_update::ALL_EXE.contains(&retired),
                "{retired} must not be required of a current package"
            );
            assert!(
                !RESTARTABLE.contains(&retired),
                "{retired} is not something an update restarts into"
            );
        }
        // The gateway's stop is rooted at the *Store* root, not the
        // installation directory, because that is where its endpoint lives.
        // The session host's is the ordinary root. Getting either wrong asks a
        // service that is not there and reports success.
        let root = Path::new("C:/isolated-koru");
        assert_eq!(
            stop_arguments(koru_update::GATEWAY_EXE, Some(root)),
            [
                "stop".into(),
                "--root".into(),
                koru_channel::store_root_under(root).into_os_string(),
            ]
        );
        assert_eq!(
            stop_arguments(koru_update::SESSION_HOST_EXE, Some(root)),
            ["stop".into(), "--root".into(), root.as_os_str().to_owned()]
        );
    }

    /// **Verification case 8, the half a machine can prove: the files.**
    ///
    /// A pre-cutover installation carries `koru-api-gateway.exe` and
    /// `koru-session-host.exe`; the incoming package carries neither. Both must
    /// end in `.previous/` and neither may remain in place — which happens only
    /// because the update owns the **union** of the incoming and installed
    /// lists. Narrowing that union to the incoming list is the change that
    /// would leave a stale signed executable beside the new build forever, and
    /// this is what notices.
    #[test]
    fn upgrading_away_from_the_standalone_hosts_leaves_neither_behind() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Koru");
        let staging = install.join(".update/staging-2-test");
        let backup = install.join(".previous/build-1-test");
        fs::create_dir_all(&staging).unwrap();

        let retired = [koru_update::GATEWAY_EXE, koru_update::SESSION_HOST_EXE];
        let carried = [koru_update::APP_EXE, koru_update::AGENT_EXE];
        for file in carried.iter().chain(retired.iter()) {
            fs::write(install.join(file), format!("old-{file}")).unwrap();
        }
        for file in carried {
            fs::write(staging.join(file), format!("new-{file}")).unwrap();
        }

        let installed = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 1,
            files: carried
                .iter()
                .chain(retired.iter())
                .map(|file| (*file).to_owned())
                .collect(),
        };
        let incoming = ReleaseFiles {
            schema_version: 1,
            inventory_version: Some(koru_update::INVENTORY_VERSION),
            build_number: 2,
            files: carried.iter().map(|file| (*file).to_owned()).collect(),
        };
        let union = owned_files(&incoming, Some(&installed));
        for file in retired {
            assert!(
                union.contains(file),
                "{file} is owned through the installed list, or it is orphaned"
            );
        }

        let mut log = Log(None);
        replace_owned(&install, &staging, &backup, &incoming, &union, &mut log)
            .expect("the swap succeeds");

        for file in retired {
            assert!(
                !install.join(file).exists(),
                "{file} is still in the installation after the upgrade"
            );
            assert_eq!(
                fs::read_to_string(backup.join(file)).unwrap(),
                format!("old-{file}"),
                "{file} was not moved into .previous/"
            );
        }
        for file in carried {
            assert_eq!(
                fs::read_to_string(install.join(file)).unwrap(),
                format!("new-{file}")
            );
        }
    }

    #[test]
    fn a_rehearsal_root_reaches_every_stop_and_restart_command() {
        let root = Path::new("C:/isolated-koru");
        let manifest = Path::new("C:/packages/build-b.update.json");
        let agent = stop_arguments(koru_update::AGENT_EXE, Some(root));
        let chat = stop_arguments(koru_update::AI_CHAT_EXE, Some(root));
        let gateway = stop_arguments(koru_update::GATEWAY_EXE, Some(root));
        assert_eq!(
            agent,
            ["stop".into(), "--root".into(), root.as_os_str().to_owned()]
        );
        assert_eq!(chat, agent);
        assert_eq!(
            gateway,
            [
                "stop".into(),
                "--root".into(),
                koru_channel::store_root_under(root).into_os_string(),
            ]
        );
        // The worker resolves its endpoint, its logs and its model cache from a
        // Store root, so a rehearsal has to hand it the same one the gateway
        // gets rather than the installation directory.
        assert_eq!(stop_arguments(koru_update::VOICE_EXE, Some(root)), gateway);
        // The retired name resolves the same root the current one does: it is
        // the same program, and an ask sent to the installation directory would
        // find nothing and report success.
        assert_eq!(
            stop_arguments(koru_update::LEGACY_VOICE_EXE, Some(root)),
            gateway
        );
        assert_eq!(
            launch_arguments(Some("health-token"), Some(root), None),
            [
                "--update-health-token".into(),
                "health-token".into(),
                "--root".into(),
                root.as_os_str().to_owned(),
            ]
        );
        assert_eq!(
            launch_arguments(None, Some(root), Some(manifest)),
            [
                "--root".into(),
                root.as_os_str().to_owned(),
                "--rehearse-update".into(),
                manifest.as_os_str().to_owned(),
            ]
        );
        assert!(launch_arguments(None, None, None).is_empty());
        assert_eq!(stop_arguments(koru_update::AGENT_EXE, None), ["stop"]);
    }

    /// A file nothing holds is free; a missing one is trivially free.
    ///
    /// The other half of this — that a **running** image is *not* free — is
    /// `a_running_executable_is_not_free`, which is the behaviour the whole
    /// quiesce design rests on and is therefore worth its own test.
    #[test]
    fn an_ordinary_file_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("koru.exe");
        assert!(is_free(&path), "a path that does not exist");
        fs::write(&path, b"not running").unwrap();
        assert!(is_free(&path), "a file nothing has open");
    }

    /// **The fact the design rests on.** Windows opens an executing image
    /// without write sharing, so a write-open is refused while it runs — and
    /// `fs::rename` of that same file *succeeds*, which is why the rename cannot
    /// be used as the check and this probe exists.
    ///
    /// Asserted against a real running process, because "Windows does this" is
    /// exactly the kind of claim that is true until a build of Windows decides
    /// otherwise, and the failure mode if it stops being true is a silently
    /// corrupted installation.
    #[cfg(windows)]
    #[test]
    fn a_running_executable_is_not_free() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("koru-fake-service.exe");
        // Any real executable will do; this one is guaranteed present and
        // waits without doing anything.
        let system = PathBuf::from(std::env::var("SystemRoot").unwrap_or("C:/Windows".into()));
        fs::copy(system.join("System32/timeout.exe"), &copy).expect("copy a real exe");
        assert!(is_free(&copy), "free before it runs");

        let mut child = Command::new(&copy)
            .args(["/t", "30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the copy");
        // Give the loader time to map the image.
        let held = {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if !is_free(&copy) {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(50));
            }
        };
        // Renaming it succeeds *while it runs* — the trap in one line.
        let moved = dir.path().join("moved-aside.exe");
        let rename_worked = fs::rename(&copy, &moved).is_ok();

        let _ = child.kill();
        let _ = child.wait();

        assert!(held, "a running image refuses a write-open");
        assert!(
            rename_worked,
            "renaming a running image succeeds — which is why is_free exists"
        );
    }

    /// The restart target is one of ours, inside the installation.
    #[test]
    fn only_this_installations_own_binaries_may_be_restarted() {
        assert!(RESTARTABLE.contains(&koru_update::APP_EXE));
        assert!(
            RESTARTABLE.contains(&koru_update::AGENT_EXE),
            "the agent restarts the stack when it is the one that asked"
        );
        assert!(!RESTARTABLE.contains(&koru_update::GATEWAY_EXE));
        assert!(!RESTARTABLE.contains(&"cmd.exe"));
    }

    /// The Linux arm's own facts — the tarball, the modes, the process-table
    /// proof and the writability refusal — on the one platform that has them.
    /// The swap, journal and rollback they feed are the cases above, which run
    /// here unchanged.
    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        fn tarball(path: &Path, files: &[(&str, u32, &[u8])]) {
            let gz = flate2::write::GzEncoder::new(
                File::create(path).unwrap(),
                flate2::Compression::fast(),
            );
            let mut builder = tar::Builder::new(gz);
            for (name, mode, data) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(*mode);
                header.set_size(data.len() as u64);
                header.set_cksum();
                builder.append(&header, *data).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }

        fn release_files(build_number: u64, files: &[&str]) -> Vec<u8> {
            serde_json::to_vec(&ReleaseFiles {
                schema_version: 1,
                inventory_version: Some(koru_update::INVENTORY_VERSION),
                build_number,
                files: files.iter().map(|file| (*file).to_owned()).collect(),
            })
            .unwrap()
        }

        fn run_agent(install: &Path) -> String {
            let output = Command::new(install.join(koru_update::AGENT_EXE))
                .output()
                .expect("the installed agent is executable");
            String::from_utf8(output.stdout).unwrap()
        }

        /// **The staging half of the transaction, from a tarball to a binary
        /// that runs**, then back. Everything `run` does between verifying the
        /// signature and launching — extract, read the incoming list and
        /// `VERSION`, prove quiet, swap — with the execute bit checked at the
        /// one place it matters: the installed file starts.
        #[test]
        fn a_tarball_stages_swaps_and_rolls_back_into_a_runnable_installation() {
            let root = tempfile::tempdir().unwrap();
            let install = root.path().join("koru");
            fs::create_dir(&install).unwrap();
            let agent = koru_update::AGENT_EXE;
            let listed = [agent, "VERSION", "release-files.json"];
            fs::write(install.join(agent), b"#!/bin/sh\necho old\n").unwrap();
            fs::set_permissions(install.join(agent), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                install.join("VERSION"),
                b"version=1\nbuild_number=1\ncommit=a\nchannel=nightly\n",
            )
            .unwrap();
            fs::write(
                install.join("release-files.json"),
                release_files(1, &listed),
            )
            .unwrap();
            fs::write(install.join("notes.txt"), b"a file the person left here").unwrap();

            let package = root.path().join("koru-2-linux-x86_64.tar.gz");
            let incoming_list = release_files(2, &listed);
            tarball(
                &package,
                &[
                    (agent, 0o755, b"#!/bin/sh\necho new\n"),
                    (
                        "VERSION",
                        0o644,
                        b"version=2\nbuild_number=2\ncommit=b\nchannel=nightly\n",
                    ),
                    ("release-files.json", 0o644, &incoming_list),
                ],
            );
            let staging = install.join(".update/staging-2-test");
            let backup = install.join(".previous/build-1-test");
            fs::create_dir_all(&staging).unwrap();
            extract_package(&package, &staging).unwrap();
            assert_eq!(mode(&staging.join(agent)), 0o755);

            let incoming =
                ReleaseFiles::load_incoming(&staging.join("release-files.json")).unwrap();
            assert_eq!(
                read_package_version(&staging.join("VERSION"))
                    .unwrap()
                    .build_number,
                2
            );
            for file in &incoming.files {
                assert!(staging.join(file).is_file(), "{file} is staged");
            }
            let installed = ReleaseFiles::load_installed(&install.join("release-files.json")).ok();
            let owned = owned_files(&incoming, installed.as_ref());
            quiesce(&install, &owned, None, &mut Log(None)).expect("nothing runs from here");
            replace_owned(
                &install,
                &staging,
                &backup,
                &incoming,
                &owned,
                &mut Log(None),
            )
            .unwrap();
            assert_eq!(run_agent(&install), "new\n");
            assert_eq!(mode(&install.join(agent)), 0o755);

            rollback(&install, &backup, &incoming.files, &mut Log(None)).unwrap();
            assert_eq!(
                run_agent(&install),
                "old\n",
                "the restored binary still starts"
            );
            assert_eq!(mode(&install.join(agent)), 0o755);
            assert!(
                install.join("notes.txt").is_file(),
                "an unlisted file is never touched"
            );
        }

        /// A process running from the installation — or from its backup, where
        /// a crashed transaction may have moved an image — is named, and its
        /// exit is seen.
        #[test]
        fn a_process_under_the_installation_is_not_quiet() {
            let root = tempfile::tempdir().unwrap();
            let install = root.path().join("koru");
            let aside = install.join(".previous/build-1-test");
            fs::create_dir_all(&aside).unwrap();
            assert!(
                still_running(&install, &[]).is_empty(),
                "an idle installation is quiet"
            );

            let mut children = Vec::new();
            for binary in [
                install.join(koru_update::AGENT_EXE),
                aside.join(koru_update::MCP_EXE),
            ] {
                // This test harness, copied, running the probe child: a real
                // image under the installation. Not `/bin/sleep`, which on a
                // multicall coreutils exits at once under any other name.
                fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
                let mut command = Command::new(&binary);
                command
                    .args([
                        "--exact",
                        "install::tests::startup_probe_child",
                        "--nocapture",
                    ])
                    .env("UPDATER_STARTUP_PROBE_CHILD_MS", "30000")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                // A parallel test can fork while this copy's write handle is
                // open here; that child holds it until it execs, so
                // `ETXTBSY` passes.
                let spawned = Instant::now();
                children.push(loop {
                    match command.spawn() {
                        Err(error)
                            if error.kind() == std::io::ErrorKind::ExecutableFileBusy
                                && spawned.elapsed() < Duration::from_secs(5) =>
                        {
                            thread::sleep(Duration::from_millis(20));
                        }
                        result => break result.unwrap(),
                    }
                });
            }
            let started = Instant::now();
            let held = loop {
                let held = still_running(&install, &[]);
                if held.len() == 2 || started.elapsed() > Duration::from_secs(10) {
                    break held;
                }
                thread::sleep(Duration::from_millis(20));
            };
            for child in &mut children {
                child.kill().unwrap();
                child.wait().unwrap();
            }
            assert_eq!(held.len(), 2, "{held:?}");
            assert!(
                held.iter()
                    .any(|line| line.starts_with(&format!("{} (pid ", koru_update::AGENT_EXE))),
                "named relative to the installation: {held:?}"
            );
            assert!(
                held.iter().any(|line| line.starts_with(".previous/")),
                "{held:?}"
            );
            assert!(
                still_running(&install, &[]).is_empty(),
                "exited processes are gone"
            );
        }

        /// A directory this user cannot write is refused with a sentence that
        /// says what to do, before anything is staged.
        #[test]
        fn a_read_only_installation_is_refused_plainly() {
            // Root writes through any mode, so the case cannot be staged.
            if unsafe { libc::geteuid() } == 0 {
                return;
            }
            let root = tempfile::tempdir().unwrap();
            let install = root.path().join("koru");
            fs::create_dir(&install).unwrap();
            ensure_writable(&install).unwrap();
            fs::set_permissions(&install, fs::Permissions::from_mode(0o555)).unwrap();
            let refused = ensure_writable(&install);
            fs::set_permissions(&install, fs::Permissions::from_mode(0o755)).unwrap();
            let error = refused.expect_err("a read-only installation is refused");
            assert!(error.contains("not writable"), "{error}");
            assert!(error.contains("folder you own"), "{error}");
        }
    }
}

/// The macOS transaction: a disk image in, one bundle swap, absence as proof.
///
/// The shape mirrors the Windows `run` — verify, stage, quiesce, journal,
/// swap, health-check, roll back — but each mechanism follows the platform:
/// the package is a **dmg** attached read-only and dittoed out rather than a
/// zip extracted; the quiesce proof is **no process running from under the
/// bundle** (macOS renames running images without complaint, so the
/// write-open probe proves nothing and the proof has to be about processes,
/// not files); and the swap is **two renames of the whole `Koru.app`** —
/// which is also why `release-files.json` lives inside the bundle and does
/// not list the signature artifacts: everything travels together or not at
/// all. One free property worth naming: the agent downloaded the package
/// over HTTP, so nothing here carries a quarantine xattr and the restarted
/// build meets no Gatekeeper dialog.
#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    pub fn run(request_path: &Path, wait_pid: u32) -> Result<Outcome, String> {
        let bytes = fs::read(request_path).map_err(|e| format!("could not read request: {e}"))?;
        let request: UpdaterRequest =
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid request: {e}"))?;
        validate_request(&request)?;
        validate_root_scope(request_path, &request)?;
        // `install_dir` is where the binaries live — `Koru.app/Contents/MacOS`
        // — and the transaction works on the bundle above it and stages in the
        // directory that *contains* the bundle, so the renames stay on one
        // filesystem.
        let (bundle, container) = bundle_of(&request.install_dir)?;
        let paths = update_paths(&request)?;
        wait_for_exit(wait_pid, WAIT_FOR_APP)?;
        if let Err(error) = paths.ensure() {
            return restart_after_error(
                &request,
                format!("could not prepare update state: {error}"),
            );
        }
        let mut log = Log::open(&paths.updater_log());
        log.line(&format!(
            "begin(macos) current={} target={} package={} bundle={}",
            request.current_build,
            request.target_build,
            request.package.display(),
            bundle.display()
        ));
        let mut update_lock = match koru_update::UpdateLock::acquire(&paths, &request.install_dir) {
            Ok(lock) => Some(lock),
            Err(_) => {
                return restart_after_error(
                    &request,
                    "another Koru instance is still using this installation".into(),
                );
            }
        };
        let mut restart_gate = match koru_update::RestartGate::acquire(&paths, &request.install_dir)
        {
            Ok(gate) => Some(gate),
            Err(error) => {
                drop(update_lock.take());
                return restart_after_error(&request, error);
            }
        };
        let update_root = container.join(".update");
        let journal = update_root.join("transaction.json");
        let recovery = recover_interrupted_bundle(
            &bundle,
            &container,
            &paths,
            command_root(&request),
            &journal,
            &mut log,
        )?;
        if let RecoveryOutcome::RestoredUnproven(error) = recovery {
            drop(update_lock.take());
            drop(restart_gate.take());
            return restart_after_rollback(&request, error);
        }
        let health = paths
            .health_dir()
            .join(format!("{}.ok", request.health_token));
        if let Err(error) = retire_health_marker(&health) {
            drop(update_lock.take());
            drop(restart_gate.take());
            return restart_after_error(&request, error);
        }
        let token = Uuid::new_v4().simple().to_string();
        let staging = update_root.join(format!("staging-{}-{token}", request.target_build));
        let previous_root = container.join(".previous");
        let backup = previous_root.join(format!("build-{}-{token}", request.current_build));

        let prepared = (|| -> Result<(), String> {
            let package = UpdatePackage {
                url: "https://local.invalid/already-downloaded".into(),
                file_name: request
                    .package
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("update.dmg")
                    .to_string(),
                size: request.package_size,
                sha256: request.expected_sha256.clone(),
                signature: request.signature.clone(),
                format: PackageFormat::Dmg,
            };
            verify_package(&request.package, &package, build_info().update_public_key)?;
            log.line("package verification succeeded");
            if !koru_update::is_writable_install(&container) {
                return Err(format!("{} is not writable", container.display()));
            }
            check_space_at(&container, request.package_size)?;
            ensure_managed_path(&container, &staging, ".update")?;
            ensure_managed_path(&container, &backup, ".previous")?;
            fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
            stage_from_dmg(&request.package, &staging, &mut log)?;

            // `load_incoming` for the same reason as the zip path: the strict
            // inventory check belongs to the publisher, not to an updater that
            // may predate the package's inventory version (ADR-258).
            let incoming = ReleaseFiles::load_incoming(
                &staging.join(koru_update::in_data("release-files.json")),
            )?;
            if incoming.build_number != request.target_build {
                return Err("release-files build does not match the requested target".into());
            }
            let version = read_package_version(&staging.join(koru_update::in_data("VERSION")))?;
            if version.build_number != request.target_build {
                return Err("VERSION build does not match the requested target".into());
            }
            for file in &incoming.files {
                if !staging.join(file).is_file() {
                    return Err(format!("staged release is missing {file}"));
                }
            }

            quiesce_bundle(&bundle, command_root(&request), &mut log)?;
            fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
            let owned: BTreeSet<String> = incoming.files.iter().cloned().collect();
            write_journal(
                &journal,
                &request,
                &staging,
                &backup,
                &owned,
                &incoming.files,
            )?;
            Ok(())
        })();
        if let Err(error) = prepared {
            log.line(&format!("update preparation failed: {error}"));
            let _ = remove_managed_dir(&container, &staging, ".update");
            let _ = remove_managed_dir(&container, &backup, ".previous");
            let _ = fs::remove_file(&journal);
            drop(update_lock.take());
            drop(restart_gate.take());
            return restart_after_error(&request, error);
        }

        // The swap. Two renames; each failure mode leaves exactly one whole
        // bundle in the active place or a clear statement that it could not.
        let saved = backup.join("Koru.app");
        if let Err(e) = fs::rename(&bundle, &saved) {
            let mut error = format!("moving the old bundle aside: {e}");
            log.line(&error);
            error = restoration_reason(
                error,
                finish_transaction(
                    &container,
                    &staging,
                    &backup,
                    &journal,
                    &health,
                    &request.health_token,
                    TransactionProof::Restored,
                ),
            );
            drop(update_lock.take());
            drop(restart_gate.take());
            return restart_after_error(&request, error);
        }
        if let Err(e) = fs::rename(staging.join("Koru.app"), &bundle) {
            let reason = format!("installing the new bundle: {e}");
            log.line(&reason);
            let restored = fs::rename(&saved, &bundle);
            record_failure(&paths, request.target_build, "swap_failed");
            return match restored {
                Ok(()) => {
                    let reason = restoration_reason(
                        reason,
                        finish_transaction(
                            &container,
                            &staging,
                            &backup,
                            &journal,
                            &health,
                            &request.health_token,
                            TransactionProof::Restored,
                        ),
                    );
                    drop(update_lock.take());
                    drop(restart_gate.take());
                    restart_after_rollback(&request, reason)
                }
                Err(restore) => {
                    drop(update_lock.take());
                    drop(restart_gate.take());
                    Err(format!(
                        "{reason}; and the old bundle could not be restored from {}: {restore}",
                        saved.display()
                    ))
                }
            };
        }
        log.line("bundle swapped");

        drop(update_lock.take());
        let mut child = match launch(
            &request.restart,
            Some(&request.health_token),
            command_root(&request),
            None,
        ) {
            Ok(child) => child,
            Err(mut error) => {
                let _rollback_lock =
                    koru_update::UpdateLock::acquire(&paths, &request.install_dir)?;
                rollback_bundle(&bundle, &saved, &update_root, &token, &mut log)?;
                error = restoration_reason(
                    error,
                    finish_transaction(
                        &container,
                        &staging,
                        &backup,
                        &journal,
                        &health,
                        &request.health_token,
                        TransactionProof::Restored,
                    ),
                );
                record_failure(&paths, request.target_build, "failed_launch");
                drop(_rollback_lock);
                drop(restart_gate.take());
                return restart_after_rollback(&request, error);
            }
        };
        match wait_for_health(&mut child, &health, WAIT_FOR_HEALTH) {
            Ok(()) => {
                log.line("health confirmation received");
                finish_transaction(
                    &container,
                    &staging,
                    &backup,
                    &journal,
                    &health,
                    &request.health_token,
                    TransactionProof::Committed,
                )?;
                let _ = fs::remove_file(request_path);
                drop(restart_gate.take());
                log.line("installation complete");
                Ok(Outcome::Installed)
            }
            Err(mut reason) => {
                let _ = child.kill();
                let _ = child.wait();
                let _rollback_lock =
                    koru_update::UpdateLock::acquire(&paths, &request.install_dir)?;
                // The restarted stack is up and healthy-ish even though the
                // health file never came — same shape as Windows: stop it
                // before moving its bundle, best effort.
                if let Err(error) = quiesce_bundle(&bundle, command_root(&request), &mut log) {
                    log.line(&format!(
                        "rollback proceeding without a clean stop: {error}"
                    ));
                }
                rollback_bundle(&bundle, &saved, &update_root, &token, &mut log)?;
                reason = restoration_reason(
                    reason,
                    finish_transaction(
                        &container,
                        &staging,
                        &backup,
                        &journal,
                        &health,
                        &request.health_token,
                        TransactionProof::Restored,
                    ),
                );
                record_failure(&paths, request.target_build, &reason);
                log.line(&format!("rolled back: {reason}"));
                drop(_rollback_lock);
                drop(restart_gate.take());
                restart_after_rollback(&request, reason)
            }
        }
    }

    /// Restore the previous whole bundle after an updater or machine crash.
    /// A bundle transaction has only two meaningful progress facts: whether
    /// `backup/Koru.app` exists (the old bundle moved) and whether the active
    /// bundle exists (the new one moved). Health remains the commit point.
    fn recover_interrupted_bundle(
        bundle: &Path,
        container: &Path,
        paths: &UpdatePaths,
        rehearsal_root: Option<&Path>,
        journal_path: &Path,
        log: &mut Log,
    ) -> Result<RecoveryOutcome, String> {
        if !journal_path.is_file() {
            return Ok(RecoveryOutcome::Ready);
        }
        let bytes = fs::read(journal_path).map_err(|e| format!("reading update journal: {e}"))?;
        let journal: TransactionJournal =
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid update journal: {e}"))?;
        if journal.schema_version != 2 {
            return Err(format!(
                "unsupported update journal schema {}; manual recovery is required",
                journal.schema_version
            ));
        }
        validate_journal(&journal)?;
        ensure_managed_path(container, &journal.staging, ".update")?;
        ensure_managed_path(container, &journal.backup, ".previous")?;
        let health = paths
            .health_dir()
            .join(format!("{}.ok", journal.health_token));
        let committed = proof_path(
            journal_path,
            &journal.health_token,
            TransactionProof::Committed,
        );
        let restored = proof_path(
            journal_path,
            &journal.health_token,
            TransactionProof::Restored,
        );
        let committed_exists = proof_exists(&committed, TransactionProof::Committed)?;
        let restored_exists = proof_exists(&restored, TransactionProof::Restored)?;
        if committed_exists && restored_exists {
            return Err("update journal has conflicting committed and restored proofs".into());
        }
        if committed_exists || restored_exists {
            let proof = if committed_exists {
                TransactionProof::Committed
            } else {
                TransactionProof::Restored
            };
            finish_transaction(
                container,
                &journal.staging,
                &journal.backup,
                journal_path,
                &health,
                &journal.health_token,
                proof,
            )?;
            log.line("completed cleanup for an interrupted proven bundle state");
            return Ok(RecoveryOutcome::Ready);
        }
        if health_confirmed(&health)? {
            finish_transaction(
                container,
                &journal.staging,
                &journal.backup,
                journal_path,
                &health,
                &journal.health_token,
                TransactionProof::Committed,
            )?;
            log.line("completed cleanup for an interrupted committed bundle update");
            return Ok(RecoveryOutcome::Ready);
        }

        let saved = journal.backup.join("Koru.app");
        if saved.is_dir() {
            if bundle.exists() {
                quiesce_bundle(bundle, rehearsal_root, log)?;
                // Keep the interrupted target only until the known-good bundle
                // is restored. Placing it inside this transaction's unique
                // staging directory makes normal cleanup remove it and cannot
                // block a later independent recovery.
                let failed = journal.staging.join("failed-interrupted-Koru.app");
                if failed.exists() {
                    return Err(format!(
                        "interrupted update recovery target already exists at {}",
                        failed.display()
                    ));
                }
                fs::rename(bundle, &failed)
                    .map_err(|e| format!("preserving interrupted bundle: {e}"))?;
            }
            fs::rename(&saved, bundle).map_err(|e| format!("restoring interrupted bundle: {e}"))?;
        }
        let finish = finish_transaction(
            container,
            &journal.staging,
            &journal.backup,
            journal_path,
            &health,
            &journal.health_token,
            TransactionProof::Restored,
        );
        match finish {
            Ok(()) => {
                log.line("recovered an interrupted uncommitted bundle update");
                Ok(RecoveryOutcome::Ready)
            }
            Err(error) => Ok(RecoveryOutcome::RestoredUnproven(format!(
                "the interrupted bundle update was restored, but its transaction proof could not be persisted: {error}"
            ))),
        }
    }

    /// `…/Koru.app/Contents/MacOS` → the bundle and the directory holding it.
    pub(super) fn bundle_of(macos_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
        let contents = macos_dir
            .parent()
            .filter(|_| macos_dir.file_name().and_then(|n| n.to_str()) == Some("MacOS"))
            .filter(|contents| contents.file_name().and_then(|n| n.to_str()) == Some("Contents"))
            .ok_or("the installation directory is not a bundle's Contents/MacOS")?;
        let bundle = contents
            .parent()
            .filter(|bundle| bundle.extension().and_then(|e| e.to_str()) == Some("app"))
            .ok_or("the installation is not inside a .app bundle")?;
        let container = bundle
            .parent()
            .ok_or("the bundle has no containing directory")?;
        Ok((bundle.to_path_buf(), container.to_path_buf()))
    }

    /// Attach the image read-only, ditto `Koru.app` into the staging
    /// directory, detach. ditto rather than a copy loop for the same reason
    /// the packager uses it: the sealed bundle must arrive byte-for-byte.
    fn stage_from_dmg(dmg: &Path, staging: &Path, log: &mut Log) -> Result<(), String> {
        let mount = staging.join(".mount");
        fs::create_dir_all(&mount).map_err(|e| e.to_string())?;
        let attached = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
            .arg(&mount)
            .arg(dmg)
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("could not run hdiutil attach: {e}"))?;
        if !attached.success() {
            return Err("hdiutil could not attach the update image".into());
        }
        let copied = Command::new("ditto")
            .arg(mount.join("Koru.app"))
            .arg(staging.join("Koru.app"))
            .status()
            .map_err(|e| format!("could not run ditto: {e}"));
        let detached = Command::new("hdiutil")
            .args(["detach"])
            .arg(&mount)
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("could not run hdiutil detach: {e}"))?;
        let _ = fs::remove_dir(&mount);
        match copied {
            Ok(status) if status.success() => {}
            Ok(_) => return Err("ditto could not copy Koru.app out of the image".into()),
            Err(error) => return Err(error),
        }
        if !detached.success() {
            log.line("the update image did not detach cleanly; continuing");
        }
        log.line("staged from the disk image");
        Ok(())
    }

    /// Ask this bundle's own CLIs to stop, then prove it by **absence**: no
    /// process is running from under the bundle. The ask is the same
    /// Agent-first, session-host, gateway-twice sequence as Windows; only the proof differs,
    /// because macOS renames running images without a murmur and the one
    /// thing a rename must never do here is move the ground out from under a
    /// live service.
    fn quiesce_bundle(
        bundle: &Path,
        rehearsal_root: Option<&Path>,
        log: &mut Log,
    ) -> Result<(), String> {
        if bundle_is_quiet(bundle) {
            return Ok(());
        }
        let binaries = bundle.join("Contents/MacOS");
        for &(service, pause_before) in STOP_SEQUENCE {
            let binary = binaries.join(service);
            if !binary.is_file() {
                continue;
            }
            if pause_before {
                thread::sleep(Duration::from_millis(500));
            }
            log.line(&format!("asking {service} to stop"));
            let mut command = Command::new(&binary);
            command.args(stop_arguments(service, rehearsal_root));
            match command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Ok(status) => log.line(&format!("{service} stop exited {status}")),
                Err(error) => log.line(&format!("{service} stop could not be started: {error}")),
            }
        }
        let started = Instant::now();
        loop {
            if bundle_is_quiet(bundle) {
                log.line("no process is running from under the bundle");
                return Ok(());
            }
            if started.elapsed() >= WAIT_FOR_SERVICES {
                return Err(format!(
                    "processes still running from under {} after {}s",
                    bundle.display(),
                    WAIT_FOR_SERVICES.as_secs()
                ));
            }
            thread::sleep(PROBE_EVERY);
        }
    }

    /// Whether nothing runs from under `bundle`, by asking the process table.
    ///
    /// `pgrep -f` over the bundle path plus a separator, so `Koru.app` does
    /// not match `Koru.app 2` next door. The updater itself never matches: it
    /// runs from the hash-checked copy in the temp directory, which is also
    /// what lets it keep running while the bundle under it is renamed.
    pub(super) fn bundle_is_quiet(bundle: &Path) -> bool {
        let pattern = format!("{}/", ere_literal(&bundle.to_string_lossy()));
        match Command::new("pgrep").arg("-f").arg(&pattern).output() {
            // 0 = matches exist; 1 = none; anything else is pgrep failing,
            // and claiming quiet on a broken probe is how installs corrupt.
            Ok(output) => output.status.code() == Some(1),
            Err(_) => false,
        }
    }

    /// One literal for the extended regular expression accepted by `pgrep`.
    /// Rehearsal roots are arbitrary user paths; metacharacters in one must not
    /// weaken the process-absence proof that protects a whole-bundle rename.
    pub(super) fn ere_literal(value: &str) -> String {
        let mut escaped = String::with_capacity(value.len());
        for character in value.chars() {
            if matches!(
                character,
                '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
            ) {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        escaped
    }

    /// Put the old bundle back, keeping the failed new one for diagnosis.
    fn rollback_bundle(
        bundle: &Path,
        saved: &Path,
        update_root: &Path,
        token: &str,
        log: &mut Log,
    ) -> Result<(), String> {
        if bundle.exists() {
            let failed = update_root.join(format!("failed-{token}-Koru.app"));
            fs::rename(bundle, &failed)
                .map_err(|e| format!("moving the failed bundle aside: {e}"))?;
            log.line(&format!("failed bundle kept at {}", failed.display()));
        }
        fs::rename(saved, bundle).map_err(|e| format!("restoring the old bundle: {e}"))?;
        log.line("previous bundle restored");
        Ok(())
    }

    /// Unlike Windows, no copy of the installation is ever made — the backup
    /// is a rename — so the requirement is the staged bundle (roughly the
    /// package decompressed) plus margin, and **not** the size of the
    /// container: that would sum every app in /Applications and demand more
    /// free space than most disks have.
    fn check_space_at(container: &Path, package_size: u64) -> Result<(), String> {
        let needed = package_size.saturating_mul(5).saturating_add(DISK_MARGIN);
        let available = koru_free_space::available(container).map_err(|e| e.to_string())?;
        if available < needed {
            return Err(format!(
                "insufficient disk space: {available} bytes available, {needed} required"
            ));
        }
        Ok(())
    }
}
