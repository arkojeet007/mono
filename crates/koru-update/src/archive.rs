// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use zip::ZipArchive;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_extracted_size: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_extracted_size: 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseFiles {
    pub schema_version: u32,
    /// Selects an immutable executable requirement set. Absence means the
    /// original five-binary inventory, preserving old package JSON. Only
    /// `koru-release` enforces the set it names (ADR-258): an updater built
    /// before a version existed must still install the package declaring it,
    /// which is why the installed side loads through
    /// [`load_incoming`](ReleaseFiles::load_incoming) and never resolves this
    /// field at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_version: Option<u32>,
    pub build_number: u64,
    pub files: Vec<String>,
}

/// Whether a package staged Windows notification identity.
///
/// **The one thing that varies between two packages of the same generation**,
/// and it is now said directly instead of being encoded as a version number.
/// Windows delivers a native notification only to a process with package
/// identity, which Koru supplies as a signed, code-free sparse package;
/// `koru-release` builds and signs it only when both
/// `KORU_WINDOWS_PACKAGE_PUBLISHER` and `KORU_SIGNTOOL_COMMAND` are set. With
/// neither, the package installs cleanly and silently cannot deliver a
/// reminder.
///
/// Both compositions are valid packages, and this type does not decide whether
/// they should be — that is release policy, in `docs/contributing/release/BACKLOG.md`. What
/// it decides is what each one must then carry: identity files are required
/// **exactly when identity was staged**, which is a fact the publisher knows
/// and hands in, not one inferred from the file list it is checking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsIdentity {
    /// The sparse package and its three visual assets are in this package.
    Staged,
    /// Signing was not configured, so no identity was staged. The natural state
    /// of every non-Windows package, and of a Windows package built without a
    /// certificate.
    NotStaged,
}

/// Every file a package must carry to be a Koru.
///
/// **One current executable list plus a conditional identity composition**, and
/// no version negotiation. There used to be seven frozen tables here, selected
/// by `inventory_version`; five of them (`V1`–`V4` and `V6`) were unreachable,
/// and the two that were live differed only in whether Windows identity had
/// been staged. Nothing in this repository has shipped to a customer, and the
/// installed side never resolved the field anyway — see
/// [`ReleaseFiles::load_incoming`] — so the tables were negotiation with
/// nobody.
///
/// The names come from [`crate::exe`] rather than being spelled here, so the
/// list is still right on a platform whose binaries carry no `.exe` suffix —
/// which is the whole point of that module, and which six literals here would
/// have quietly undone.
fn required(identity: WindowsIdentity) -> impl Iterator<Item = String> {
    let mut files = crate::exe::ALL
        .iter()
        .copied()
        .map(crate::exe::in_bundle)
        .chain(
            crate::exe::RELEASE_AUXILIARY
                .iter()
                .map(|path| (*path).to_owned()),
        )
        .chain([
            crate::exe::in_data("VERSION"),
            crate::exe::in_data("release-files.json"),
        ])
        // The documents a running Koru reads beside itself: the MCP model
        // contract and the delegated-agent preamble. Data, so `in_data`.
        .chain(
            crate::exe::RELEASE_DOCUMENTS
                .iter()
                .copied()
                .map(crate::exe::in_data),
        )
        // The license notices a binary distribution must carry. Data too.
        .chain(
            crate::exe::RELEASE_LEGAL
                .iter()
                .copied()
                .map(crate::exe::in_data),
        )
        .collect::<Vec<_>>();
    // `cfg!(windows)` as well as the flag, because identity is a Windows
    // mechanism and staging it anywhere else is not a thing that can happen.
    // Keeping the platform guard here means a caller cannot accidentally
    // demand an `.msix` of a macOS bundle.
    if identity == WindowsIdentity::Staged && cfg!(windows) {
        files.push(crate::exe::in_bundle(crate::exe::WINDOWS_IDENTITY_PACKAGE));
        files.extend(
            crate::exe::WINDOWS_IDENTITY_ASSETS
                .iter()
                .copied()
                .map(crate::exe::in_bundle),
        );
    }
    files.into_iter()
}

impl ReleaseFiles {
    /// The full check, for a package that is about to be published.
    ///
    /// `identity` is what the publisher **staged**, not what this list claims:
    /// a package that meant to carry Windows notification identity and lost the
    /// `.msix` on the way must fail here, and it cannot if the requirement is
    /// derived from the very list being checked.
    pub fn validate(&self, identity: WindowsIdentity) -> Result<(), String> {
        self.validate_paths()?;
        // **The generation stamp is checked, not resolved.** Nothing selects a
        // requirement table with it any more, but a package that stamps a
        // generation this tree does not write is a publisher bug, and publish
        // time is where a publisher bug should stop. Install time still never
        // looks at it — see [`load_incoming`](Self::load_incoming).
        if self.inventory_version != Some(crate::exe::INVENTORY_VERSION) {
            return Err(format!(
                "release-files.json declares inventory version {:?}; this build publishes {}",
                self.inventory_version,
                crate::exe::INVENTORY_VERSION
            ));
        }
        for required in required(identity) {
            if !self.files.contains(&required) {
                return Err(format!("release-files.json does not list {required}"));
            }
        }
        Ok(())
    }

    /// The metadata and the path rules, without the required-files check.
    fn validate_paths(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.build_number == 0 || self.files.is_empty() {
            return Err("release-files.json has invalid metadata".into());
        }
        let mut seen = HashSet::new();
        for file in &self.files {
            let path = strict_relative_path(file)?;
            if !seen.insert(path) {
                return Err("release-files.json contains duplicate paths".into());
            }
        }
        Ok(())
    }

    /// The full check, for a package that is about to be **published**. The
    /// requirement set is compiled into this binary, so only a caller that is
    /// always built from the same tree as it — the publisher — may enforce it
    /// (ADR-258), and only the publisher knows whether it staged identity.
    pub fn load(path: &Path, identity: WindowsIdentity) -> Result<Self, String> {
        let value = Self::read(path)?;
        value.validate(identity)?;
        Ok(value)
    }

    /// The **incoming** package's list, read by the installed updater.
    ///
    /// **Deliberately structural, not strict** (ADR-258). The updater that
    /// performs a swap is the one copied out of the *installed* build, so the
    /// strict [`validate`](Self::validate) here would check the incoming
    /// package against a requirement list compiled into an older binary —
    /// refusing every package whose generation it has never heard of. That is
    /// not hypothetical: installs at build ≤ 43 rejected everything once the
    /// store left the package ("does not list koru-store.exe"), and build 74
    /// rejected build 75 for declaring `inventory_version: 4` ("unsupported
    /// inventory version"). No commit can retroactively fix an installed
    /// updater, so the strict check lives where the binary is always current:
    /// publish time, in `koru-release`, before the package is signed.
    ///
    /// What install time still proves: the Minisign signature (so this list
    /// was published by us *after* passing the strict check), the path rules
    /// (this list chooses what gets renamed), and — in the caller — that
    /// every listed file is actually staged.
    pub fn load_incoming(path: &Path) -> Result<Self, String> {
        let value = Self::read(path)?;
        value.validate_paths()?;
        Ok(value)
    }

    /// The **installed** list, read only to widen the set of files this update
    /// owns and may therefore back up.
    ///
    /// **Deliberately not strict**, and the difference is load-bearing. A build
    /// installed before the agent and the store joined the package lists
    /// neither, so [`validate`](Self::validate) refuses it — and the caller
    /// reads this with `.ok()`, so a refusal is indistinguishable from "there
    /// was no list". The union silently narrows to the incoming files, and a
    /// file the old package owned but the new one does not — `koru.cmd`, on
    /// exactly the upgrade this was written for — is orphaned in the
    /// installation instead of being moved aside.
    ///
    /// The path rules still apply: this list chooses which files get renamed,
    /// so a traversal in it is as dangerous here as anywhere.
    pub fn load_installed(path: &Path) -> Result<Self, String> {
        let value = Self::read(path)?;
        value.validate_paths()?;
        Ok(value)
    }

    fn read(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageVersion {
    pub version: String,
    pub build_number: u64,
    pub commit: String,
    pub channel: String,
}

pub fn read_package_version(path: &Path) -> Result<PackageVersion, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = PackageVersion::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "version" => out.version = value.trim().to_string(),
            "build_number" => {
                out.build_number = value
                    .trim()
                    .parse()
                    .map_err(|_| "VERSION contains an invalid build number")?
            }
            "commit" => out.commit = value.trim().to_string(),
            "channel" => out.channel = value.trim().to_string(),
            _ => {}
        }
    }
    if out.version.is_empty()
        || out.build_number == 0
        || out.commit.is_empty()
        || out.channel.is_empty()
    {
        return Err("VERSION is missing required metadata".into());
    }
    Ok(out)
}

pub fn extract_zip(
    archive: &Path,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|e| e.to_string())?;
    if zip.len() > limits.max_entries {
        return Err("archive contains too many entries".into());
    }
    let mut names = HashSet::new();
    let mut expanded = 0u64;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        // The raw bytes, because a name that does not decode is one of the
        // ways `enclosed_name` refuses an entry, and the message still has to
        // say which entry it was.
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            format!(
                "unsafe archive path: {}",
                String::from_utf8_lossy(entry.name_raw())
            )
        })?;
        strict_relative_path(&enclosed.to_string_lossy())?;
        if !names.insert(enclosed) {
            return Err("archive contains duplicate paths".into());
        }
        if is_symlink(entry.unix_mode()) || (!entry.is_dir() && !entry.is_file()) {
            return Err("archive contains a link or unsupported file type".into());
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or("archive expanded-size overflow")?;
        if expanded > limits.max_extracted_size {
            return Err("archive expands beyond the configured limit".into());
        }
    }
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let relative = entry.enclosed_name().ok_or("unsafe archive path")?;
        let output = destination.join(relative);
        ensure_within(destination, &output)?;
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|e| e.to_string())?;
        io::copy(&mut entry.take(limits.max_extracted_size + 1), &mut file)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The Linux package: a gzipped tarball laid out exactly like the Windows zip,
/// extracted under exactly [`extract_zip`]'s rules.
///
/// **Two passes, as the zip path does.** Every header is checked — path,
/// type, duplicates, entry count, the running expanded size — before a single
/// byte is written, so a refused archive leaves nothing behind for the caller
/// to clean up beyond the staging directory it created. A tarball has no
/// central directory to read up front, so the first pass is a full read that
/// skips the data; the expanded-size check runs per header, which bounds what
/// that pass decompresses to the limit itself.
///
/// The same refusals as the zip: an absolute path, a `..`, a backslash, a NUL,
/// a duplicate, and anything that is not a regular file or a directory —
/// symlinks and hard links above all, because either one would let a later
/// entry write through it to wherever it points. `./` prefixes, which GNU
/// `tar -C dir .` writes, are dropped rather than refused.
///
/// **The mode comes along**, which is the reason this format exists here: a
/// binary extracted without its execute bit is an update that cannot start.
/// The permission bits are kept and nothing above them — no setuid, setgid or
/// sticky — and the owner can always read and write, because the updater
/// reopens a restored file for writing to flush it during a rollback.
pub fn extract_tar_gz(
    archive: &Path,
    destination: &Path,
    limits: ArchiveLimits,
) -> Result<(), String> {
    let mut names = HashSet::new();
    let mut expanded = 0u64;
    let mut count = 0usize;
    let mut tar = open_tar_gz(archive)?;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        count += 1;
        if count > limits.max_entries {
            return Err("archive contains too many entries".into());
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err("archive contains a link or unsupported file type".into());
        }
        let Some(relative) = tar_entry_path(&entry)? else {
            continue;
        };
        if !names.insert(relative) {
            return Err("archive contains duplicate paths".into());
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or("archive expanded-size overflow")?;
        if expanded > limits.max_extracted_size {
            return Err("archive expands beyond the configured limit".into());
        }
    }
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut tar = open_tar_gz(archive)?;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let Some(relative) = tar_entry_path(&entry)? else {
            continue;
        };
        let output = destination.join(relative);
        ensure_within(destination, &output)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&output).map_err(|e| e.to_string())?;
            continue;
        }
        if !kind.is_file() {
            return Err("archive contains a link or unsupported file type".into());
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mode = entry.header().mode().map_err(|e| e.to_string())?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|e| e.to_string())?;
        io::copy(
            &mut (&mut entry).take(limits.max_extracted_size + 1),
            &mut file,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode((mode & 0o777) | 0o600))
                .map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn open_tar_gz(archive: &Path) -> Result<tar::Archive<flate2::read::GzDecoder<File>>, String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    Ok(tar::Archive::new(flate2::read::GzDecoder::new(file)))
}

/// An entry's path, under [`strict_relative_path`], with `.` components
/// dropped. `None` is the archive root itself (`./`), which names nothing to
/// create. The raw bytes must be UTF-8, because a lossy decode could turn two
/// different names into one.
fn tar_entry_path<R: Read>(entry: &tar::Entry<'_, R>) -> Result<Option<PathBuf>, String> {
    let raw = entry.path_bytes();
    let name = std::str::from_utf8(&raw)
        .map_err(|_| format!("unsafe archive path: {}", String::from_utf8_lossy(&raw)))?;
    let path = strict_relative_path(name).map_err(|_| format!("unsafe archive path: {name}"))?;
    let normal: PathBuf = path
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .collect();
    Ok((!normal.as_os_str().is_empty()).then_some(normal))
}

/// Validate one package-owned relative path before it is joined to an install,
/// staging, or backup directory. Journals reuse this because a corrupted local
/// transaction file must not gain more path authority than an archive entry.
pub fn strict_relative_path(value: &str) -> Result<PathBuf, String> {
    if value.contains('\0') || value.contains('\\') {
        return Err(format!("invalid release path: {value}"));
    }
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("invalid release path: {value}"));
    }
    Ok(path.to_path_buf())
}

fn ensure_within(root: &Path, candidate: &Path) -> Result<(), String> {
    let relative = candidate
        .strip_prefix(root)
        .map_err(|_| "archive path escaped staging")?;
    strict_relative_path(&relative.to_string_lossy()).map(|_| ())
}

fn is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170000 == 0o120000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RELEASE_NOTES_FILE;
    use serde::Deserialize;

    /// A current package built **without** a signing configuration: every
    /// executable, no Windows notification identity. The composition Koru
    /// publishes today when `KORU_SIGNTOOL_COMMAND` is unset.
    fn unsigned_koru() -> ReleaseFiles {
        ReleaseFiles {
            schema_version: 1,
            inventory_version: Some(crate::exe::INVENTORY_VERSION),
            build_number: 2,
            files: required(WindowsIdentity::NotStaged).collect(),
        }
    }

    /// A current package built **with** one: the same executables plus the
    /// sparse identity package and its three visual assets.
    fn identity_koru() -> ReleaseFiles {
        ReleaseFiles {
            schema_version: 1,
            inventory_version: Some(crate::exe::INVENTORY_VERSION),
            build_number: 2,
            files: required(WindowsIdentity::Staged).collect(),
        }
    }

    #[derive(Deserialize)]
    struct LegacyReader {
        schema_version: u32,
        build_number: u64,
        files: Vec<String>,
    }

    /// **Both compositions this publisher can produce are valid packages**, and
    /// each is validated against what it actually staged. This is the property
    /// the collapse to one contract had to preserve: retiring two executables
    /// changed *what a package carries*, never *which packages are allowed to
    /// exist*.
    #[test]
    fn both_windows_compositions_are_whole_packages() {
        unsigned_koru()
            .validate(WindowsIdentity::NotStaged)
            .expect("a package built without signing is a whole Koru");
        identity_koru()
            .validate(WindowsIdentity::Staged)
            .expect("a package built with signing is a whole Koru");
    }

    /// **Identity files are required exactly when identity was staged.**
    ///
    /// Both directions, because only one of them is the interesting failure: a
    /// package that meant to carry identity and lost a file must be refused,
    /// and a package that never staged any must not be asked for them. The
    /// requirement follows the publisher's answer, not the list being checked —
    /// which is why the last assertion here would be impossible under a rule
    /// that inferred staging from the list.
    #[cfg(windows)]
    #[test]
    fn the_identity_requirement_follows_what_was_staged() {
        for missing in std::iter::once(crate::exe::WINDOWS_IDENTITY_PACKAGE)
            .chain(crate::exe::WINDOWS_IDENTITY_ASSETS.iter().copied())
        {
            let mut incomplete = identity_koru();
            incomplete
                .files
                .retain(|path| path != &crate::exe::in_bundle(missing));
            let error = incomplete
                .validate(WindowsIdentity::Staged)
                .expect_err("a staged identity missing a file is not a whole package");
            assert!(error.contains(missing), "{error} names the missing file");
        }

        let unsigned = unsigned_koru();
        for absent in std::iter::once(crate::exe::WINDOWS_IDENTITY_PACKAGE)
            .chain(crate::exe::WINDOWS_IDENTITY_ASSETS.iter().copied())
        {
            assert!(
                !unsigned.files.contains(&crate::exe::in_bundle(absent)),
                "the unsigned composition stages no identity"
            );
        }
        unsigned
            .validate(WindowsIdentity::NotStaged)
            .expect("an unsigned package is not asked for identity it never staged");
        assert!(
            unsigned.validate(WindowsIdentity::Staged).is_err(),
            "claiming a staged identity that is not there is refused"
        );
    }

    /// **A package without a runtime document is refused, and the error names
    /// it.** The preamble is the one that would otherwise fail quietly: a
    /// package that lost it installs, runs, and hands every delegated agent
    /// the compiled-in copy while the person's edits go to a file nothing
    /// reads.
    #[test]
    fn a_package_without_a_runtime_document_is_not_a_koru() {
        for missing in crate::exe::RELEASE_DOCUMENTS {
            let mut list = unsigned_koru();
            let path = crate::exe::in_data(missing);
            assert!(list.files.contains(&path), "{path} is required");
            list.files.retain(|file| *file != path);
            let error = list
                .validate(WindowsIdentity::NotStaged)
                .expect_err("missing document is refused");
            assert!(error.contains(missing), "{error} names the missing file");
        }
    }

    /// **A package without its license notices cannot be published.** Nothing
    /// at run time would notice them missing, which is exactly why the
    /// publisher has to.
    #[test]
    fn a_package_without_its_license_notices_is_not_a_koru() {
        for missing in crate::exe::RELEASE_LEGAL {
            let mut list = unsigned_koru();
            let path = crate::exe::in_data(missing);
            assert!(list.files.contains(&path), "{path} is required");
            list.files.retain(|file| *file != path);
            let error = list
                .validate(WindowsIdentity::NotStaged)
                .expect_err("missing notice is refused");
            assert!(error.contains(missing), "{error} names the missing file");
        }
    }

    /// A package that installs and then cannot open a workspace is the failure
    /// this list exists to make impossible — so every name in it, not a
    /// hand-picked three.
    #[test]
    fn a_package_without_a_required_runtime_binary_is_not_a_koru() {
        for missing in crate::exe::ALL {
            let mut list = unsigned_koru();
            let path = crate::exe::in_bundle(missing);
            list.files.retain(|file| *file != path);
            let error = list
                .validate(WindowsIdentity::NotStaged)
                .expect_err("missing binary is refused");
            assert!(error.contains(missing), "{error} names the missing file");
        }
    }

    /// **The two retired executables are gone from the contract, both ways.**
    ///
    /// Nothing requires them, so a package that ships neither is whole; and the
    /// required set names none of the retired generations either. The updater
    /// is what still knows these names, and it knows them in order to stop them
    /// and move their files aside — `crates/koru-updater/src/install.rs`.
    #[test]
    fn the_retired_executables_are_no_longer_part_of_a_package() {
        let required = required(WindowsIdentity::Staged).collect::<Vec<_>>();
        for retired in [
            crate::exe::GATEWAY,
            crate::exe::SESSION_HOST,
            crate::exe::TERMINAL,
            crate::exe::LEGACY_STORE,
            crate::exe::LEGACY_STORE_SERVER,
            crate::exe::LEGACY_VOICE,
        ] {
            let path = crate::exe::in_bundle(retired);
            assert!(
                !required.contains(&path),
                "{retired} is retired and must not be required"
            );
        }
        unsigned_koru()
            .validate(WindowsIdentity::NotStaged)
            .expect("a package carrying neither retired executable is whole");
    }

    /// **The COM broker is required of every Windows package, signed or not.**
    ///
    /// Deliberate, and the one place the collapse tightened rather than
    /// preserved. `koru-release`'s Windows build table stages
    /// `koru-com-notification.exe` unconditionally, so no package this
    /// publisher can produce is refused by requiring it — but the retired
    /// unsigned inventory did not list it, which meant a Windows package could
    /// lose the process that receives reminder toast actions and still
    /// validate. Requiring it says what the build already does. It decides
    /// nothing about whether the unsigned variant should exist; the case above
    /// shows it still does.
    #[cfg(windows)]
    #[test]
    fn the_notification_com_broker_is_required_of_the_unsigned_variant_too() {
        let mut unsigned = unsigned_koru();
        unsigned
            .files
            .retain(|path| path != &crate::exe::in_bundle(crate::exe::COM_NOTIFICATION));
        let error = unsigned
            .validate(WindowsIdentity::NotStaged)
            .expect_err("an unsigned Windows package still carries the COM broker");
        assert!(error.contains(crate::exe::COM_NOTIFICATION));
    }

    /// The generation stamp is checked at publish and never resolved at
    /// install. A publisher that stamps something this tree does not write has
    /// a bug, and this is where it stops.
    #[test]
    fn an_unexpected_inventory_version_is_rejected_at_publish() {
        for stamped in [None, Some(1), Some(5), Some(7), Some(99)] {
            let mut list = unsigned_koru();
            list.inventory_version = stamped;
            let error = list
                .validate(WindowsIdentity::NotStaged)
                .expect_err("only this build's generation is published");
            assert!(error.contains("inventory version"), "{error}");
        }
    }

    /// The trap ADR-258 exists for: the updater performing a swap was built
    /// with an older inventory table than the package it is installing. Build
    /// 74 refused build 75 over `inventory_version: 4` (2026-08-31); installs
    /// at build <= 43 refused everything after the store left the package. The
    /// install-time load is structural so a signed future package installs.
    #[test]
    fn an_updater_installs_a_package_whose_inventory_it_has_never_heard_of() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-files.json");
        let mut future = unsigned_koru();
        future.inventory_version = Some(99);
        future.files.push("koru-something-newer".into());
        fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();

        assert!(
            ReleaseFiles::load(&path, WindowsIdentity::NotStaged).is_err(),
            "publish time stays strict"
        );
        let incoming = ReleaseFiles::load_incoming(&path).expect("install time is structural");
        assert_eq!(incoming.inventory_version, Some(99));
    }

    /// The same leniency, downwards: an already-published five-binary list from
    /// before the agent and the store joined the package still installs, which
    /// is what makes a downgrade possible at all. Spelled out rather than
    /// generated, because the table that generated it is deliberately gone.
    #[test]
    fn a_new_updater_accepts_an_already_published_archive_for_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-files.json");
        let published = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 1,
            files: [
                crate::exe::APP,
                crate::exe::AGENT,
                crate::exe::GATEWAY,
                crate::exe::MCP,
                crate::exe::UPDATER,
            ]
            .into_iter()
            .map(crate::exe::in_bundle)
            .chain([
                crate::exe::in_data("VERSION"),
                crate::exe::in_data("release-files.json"),
            ])
            .collect(),
        };
        fs::write(&path, serde_json::to_vec(&published).unwrap()).unwrap();
        let downgrade =
            ReleaseFiles::load_incoming(&path).expect("a published archive remains installable");
        assert_eq!(downgrade.inventory_version, None);
        assert!(
            downgrade
                .files
                .contains(&crate::exe::in_bundle(crate::exe::GATEWAY)),
            "the retired gateway is exactly what an already-published package still lists"
        );
    }

    /// Structural is not lax about *where a path points*: the incoming list
    /// chooses which files get renamed, so it keeps the full path rules.
    #[test]
    fn an_incoming_list_still_may_not_escape_the_installation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-files.json");
        let mut list = unsigned_koru();
        list.files.push("../elsewhere.exe".into());
        fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        assert!(ReleaseFiles::load_incoming(&path).is_err());
    }

    #[test]
    fn an_old_updater_can_read_a_new_inventory() {
        let json = serde_json::to_vec(&unsigned_koru()).unwrap();
        let old: LegacyReader = serde_json::from_slice(&json).unwrap();
        assert_eq!(old.schema_version, 1);
        assert_eq!(old.build_number, 2);
        for executable in crate::exe::ALL {
            assert!(old.files.contains(&crate::exe::in_bundle(executable)));
        }
    }

    #[test]
    fn release_list_rejects_traversal_duplicates_and_missing_required_files() {
        let mut list = unsigned_koru();
        list.validate(WindowsIdentity::NotStaged).unwrap();
        list.files.push("../outside".into());
        assert!(list.validate(WindowsIdentity::NotStaged).is_err());
        list.files.pop();
        list.files.push(crate::exe::in_bundle(crate::exe::APP));
        assert!(
            list.validate(WindowsIdentity::NotStaged).is_err(),
            "a duplicate is refused"
        );
    }

    /// **The notes are a nicety, not a binary**, and this is what keeps them
    /// that way.
    ///
    /// `RELEASE_NOTES_FILE` ships in every package `koru-release` builds from a
    /// descriptor, and Koru opens a *What's new* tab from it after an update. It
    /// is deliberately **not** in `required()`: this list is the strict check
    /// applied to an *incoming* package, so a name added here makes every
    /// package built by an older `koru-release` — including the ones already
    /// published and installable today — fail to validate and refuse to install.
    /// A build with no notes must still install; the reader treats the absence
    /// as "no notes for this build" and says so.
    ///
    /// Someone will eventually propose adding it for symmetry with `VERSION`.
    /// `VERSION` is load-bearing — the installer reads the build number out of
    /// it — and this is not.
    #[test]
    fn a_package_with_no_release_notes_is_still_a_koru() {
        let list = unsigned_koru();
        assert!(
            !list.files.iter().any(|file| file == RELEASE_NOTES_FILE),
            "the notes are not part of the required set"
        );
        list.validate(WindowsIdentity::NotStaged)
            .expect("a package that ships no notes still installs");
    }

    /// The union in the updater's `owned` set has to survive reading a list
    /// written before the agent and the store were part of a package —
    /// otherwise the file that *was* owned and no longer is gets orphaned.
    #[test]
    fn the_installed_list_of_an_older_build_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-files.json");
        let two_binaries = ReleaseFiles {
            schema_version: 1,
            inventory_version: None,
            build_number: 15,
            // The two binaries a build from before the agent and the store
            // shipped, plus the launcher that was retired with them.
            files: vec![
                crate::exe::APP.into(),
                crate::exe::UPDATER.into(),
                "koru.cmd".into(),
                "VERSION".into(),
                "release-files.json".into(),
            ],
        };
        fs::write(&path, serde_json::to_vec(&two_binaries).unwrap()).unwrap();

        assert!(
            ReleaseFiles::load(&path, WindowsIdentity::NotStaged).is_err(),
            "not a whole Koru"
        );
        let installed = ReleaseFiles::load_installed(&path).expect("still readable");
        assert!(installed.files.iter().any(|file| file == "koru.cmd"));
    }

    /// Lenient about the required set, never about where a path points: this
    /// list chooses which files get renamed.
    #[test]
    fn an_installed_list_still_may_not_escape_the_installation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("release-files.json");
        let mut list = unsigned_koru();
        list.files.push("../elsewhere.exe".into());
        fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        assert!(ReleaseFiles::load_installed(&path).is_err());
    }

    /// A list naming the *other* platform's binaries is rejected, which is what
    /// makes [`crate::exe`] load-bearing rather than cosmetic. Windows is the
    /// one platform where the two spellings coincide, so there is nothing to
    /// assert there.
    #[test]
    #[cfg(not(windows))]
    fn a_windows_release_list_does_not_satisfy_a_unix_build() {
        let list = ReleaseFiles {
            schema_version: 1,
            inventory_version: Some(crate::exe::INVENTORY_VERSION),
            build_number: 2,
            files: vec![
                "koru.exe".into(),
                "koru-updater.exe".into(),
                "VERSION".into(),
                "release-files.json".into(),
            ],
        };
        assert!(list.validate(WindowsIdentity::NotStaged).is_err());
    }

    /// One crafted tarball entry. The name is written into the header raw,
    /// because `tar::Builder` refuses the very paths these tests need.
    struct Raw<'a> {
        name: &'a [u8],
        kind: tar::EntryType,
        mode: u32,
        data: &'a [u8],
        link: &'a [u8],
    }

    fn file<'a>(name: &'a str, mode: u32, data: &'a [u8]) -> Raw<'a> {
        Raw {
            name: name.as_bytes(),
            kind: tar::EntryType::Regular,
            mode,
            data,
            link: b"",
        }
    }

    fn dir(name: &str) -> Raw<'_> {
        Raw {
            name: name.as_bytes(),
            kind: tar::EntryType::Directory,
            mode: 0o755,
            data: b"",
            link: b"",
        }
    }

    fn tarball(entries: &[Raw<'_>]) -> tempfile::NamedTempFile {
        let out = tempfile::NamedTempFile::new().unwrap();
        let gz = flate2::write::GzEncoder::new(out.reopen().unwrap(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            {
                let old = header.as_old_mut();
                old.name[..entry.name.len()].copy_from_slice(entry.name);
                old.linkname[..entry.link.len()].copy_from_slice(entry.link);
            }
            header.set_entry_type(entry.kind);
            header.set_mode(entry.mode);
            header.set_size(entry.data.len() as u64);
            header.set_cksum();
            builder.append(&header, entry.data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        out
    }

    fn extract(
        entries: &[Raw<'_>],
        limits: ArchiveLimits,
    ) -> (tempfile::TempDir, PathBuf, Result<(), String>) {
        let archive = tarball(entries);
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("staging");
        let result = extract_tar_gz(archive.path(), &staging, limits);
        (root, staging, result)
    }

    /// The Windows zip's layout, files at the root and data in folders, with
    /// the `./` prefix GNU tar writes — and every mode where it belongs.
    #[cfg(unix)]
    #[test]
    fn a_tarball_extracts_like_the_zip_and_keeps_its_modes() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, staging, result) = extract(
            &[
                dir("./"),
                file("./koru", 0o755, b"elf"),
                file("koru-updater", 0o4755, b"elf"),
                dir("assets/"),
                file("assets/fonts/a.ttf", 0o644, b"font"),
                file("VERSION", 0o444, b"build_number=2\n"),
            ],
            ArchiveLimits::default(),
        );
        result.unwrap();
        let mode = |path: &str| {
            fs::metadata(staging.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(fs::read(staging.join("koru")).unwrap(), b"elf");
        assert_eq!(mode("koru"), 0o755, "the execute bit survives");
        assert_eq!(
            mode("koru-updater"),
            0o755,
            "nothing above the permission bits"
        );
        assert_eq!(mode("assets/fonts/a.ttf"), 0o644);
        assert_eq!(mode("VERSION"), 0o644, "the owner can always write");
    }

    /// Every way a name can leave the staging directory is refused, and the
    /// refusal comes before anything is written.
    #[test]
    fn a_tarball_path_cannot_leave_staging() {
        for name in [
            "../outside",
            "a/../../outside",
            "/etc/outside",
            "a\\..\\outside",
            "",
        ] {
            let (_root, staging, result) = extract(
                &[file("koru", 0o755, b"ok"), file(name, 0o644, b"x")],
                ArchiveLimits::default(),
            );
            assert!(result.is_err(), "{name:?} was accepted");
            assert!(
                !staging.exists(),
                "{name:?}: nothing is written before the check"
            );
        }
        let (_root, _staging, result) = extract(
            &[Raw {
                name: b"bad\xff",
                ..file("", 0o644, b"x")
            }],
            ArchiveLimits::default(),
        );
        assert!(result.is_err(), "a name that is not UTF-8 is refused");
    }

    /// A link is refused whatever it points at; so is every other type that is
    /// not a file or a directory.
    #[test]
    fn a_tarball_link_or_special_file_is_refused() {
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Fifo,
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Continuous,
        ] {
            let (_root, staging, result) = extract(
                &[
                    Raw {
                        name: b"koru",
                        kind,
                        mode: 0o755,
                        data: b"",
                        link: b"koru-agent",
                    },
                    file("koru-agent", 0o755, b"elf"),
                ],
                ArchiveLimits::default(),
            );
            let error = result.expect_err(&format!("{kind:?} was accepted"));
            assert!(error.contains("link or unsupported"), "{error}");
            assert!(!staging.exists());
        }
    }

    #[test]
    fn a_tarball_with_duplicate_paths_is_refused() {
        let (_root, _staging, result) = extract(
            &[file("koru", 0o755, b"one"), file("./koru", 0o755, b"two")],
            ArchiveLimits::default(),
        );
        assert!(result.unwrap_err().contains("duplicate"));
    }

    #[test]
    fn a_tarball_over_its_limits_is_refused() {
        let (_root, staging, result) = extract(
            &[file("a", 0o644, &[0; 600]), file("b", 0o644, &[0; 600])],
            ArchiveLimits {
                max_entries: 10,
                max_extracted_size: 1000,
            },
        );
        assert!(result.unwrap_err().contains("limit"));
        assert!(!staging.exists());
        let (_root, _staging, result) = extract(
            &[
                file("a", 0o644, b"1"),
                file("b", 0o644, b"2"),
                file("c", 0o644, b"3"),
            ],
            ArchiveLimits {
                max_entries: 2,
                max_extracted_size: 1000,
            },
        );
        assert!(result.unwrap_err().contains("too many"));
    }

    #[test]
    fn version_file_is_keyed_not_positional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("VERSION");
        fs::write(
            &path,
            "commit=abc\nchannel=stable\nbuild_number=42\nversion=1.2.3\n",
        )
        .unwrap();
        let version = read_package_version(&path).unwrap();
        assert_eq!(version.build_number, 42);
        assert_eq!(version.version, "1.2.3");
    }
}
