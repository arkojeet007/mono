// SPDX-License-Identifier: MPL-2.0

use koru_update::{ReleaseFiles, sha256_file};
use std::ffi::OsString;
use std::fs;
// The zip and tar.gz writers are every platform's but macOS, which ships a
// disk image; the tests build both everywhere, for the layout-parity check.
#[cfg(any(not(target_os = "macos"), test))]
use std::fs::File;
#[cfg(any(not(target_os = "macos"), test))]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(any(not(target_os = "macos"), test))]
use zip::write::SimpleFileOptions;

/// Every face the app registers, named rather than globbed.
///
/// **The source of truth is `crates/koru-draw/src/theme.rs`** —
/// `BUNDLED_FAMILIES` plus the three loose constants `NOTO_KR_FILE`,
/// `PHOSPHOR_FILE` and `PHOSPHOR_FILL_FILE`. This list is a second copy of it
/// because `koru-release` must not link `koru-draw`: a release tool has no
/// business dragging vello and parley into its build. The drift guard is
/// therefore split in two — `koru-draw`'s asset tests assert the registry
/// names files that exist under `assets/fonts/`, and `fonts_are_the_whole_set` below
/// asserts this list is the same size, has no duplicates, and names files that
/// exist too.
///
/// **Named rather than globbed**, so a half-finished `fetch-fonts` is a refusal
/// to package rather than a bundle that is quietly short of a face. That
/// mattered more once the list stopped being ten: `read_asset` treats a missing
/// file as non-fatal by design, so every omission here shipped as a silent
/// fallback to a system face. Three of them were visible bugs —
/// `Phosphor-Fill.ttf` is the family every *filled* icon is drawn in, so a
/// pinned window's pin came out as an outline; `JetBrainsMono` is the default
/// family for code; and Poppins, Manrope, Lora, Nunito, Source Serif 4 and
/// Source Code Pro are all offered by the font picker, which listed faces the
/// bundle did not carry.
pub(crate) const FONTS: &[&str] = &[
    // sans
    "Inter.ttf",
    "Inter-Italic.ttf",
    "SourceSans3.ttf",
    "SourceSans3-Italic.ttf",
    "PlusJakartaSans.ttf",
    "PlusJakartaSans-Italic.ttf",
    "IBMPlexSans.ttf",
    "IBMPlexSans-Italic.ttf",
    // Poppins has no variable font, so it ships a static instance per weight
    // and its italic members are separate files — eight rather than two.
    "Poppins-Regular.ttf",
    "Poppins-Italic.ttf",
    "Poppins-Medium.ttf",
    "Poppins-MediumItalic.ttf",
    "Poppins-SemiBold.ttf",
    "Poppins-SemiBoldItalic.ttf",
    "Poppins-Bold.ttf",
    "Poppins-BoldItalic.ttf",
    // Manrope ships a variable roman and no italic face at all; Ctrl+I
    // faux-skews it. There is no second file to miss here.
    "Manrope.ttf",
    // serif
    "SourceSerif4.ttf",
    "SourceSerif4-Italic.ttf",
    "Lora.ttf",
    "Lora-Italic.ttf",
    // rounded
    "Nunito.ttf",
    "Nunito-Italic.ttf",
    // mono
    "JetBrainsMono.ttf",
    "JetBrainsMono-Italic.ttf",
    "SourceCodePro.ttf",
    "SourceCodePro-Italic.ttf",
    // CJK, and the icons
    "NotoSansKR-Regular.ttf",
    "Phosphor.ttf",
    // A separate family, not a weight of the one above: Phosphor ships the
    // filled shapes as a second file at the same codepoints.
    "Phosphor-Fill.ttf",
];

/// Where `fetch-fonts` puts the faces, relative to the workspace root, and
/// where the package carries them relative to its data directory — the same
/// layout `assets/reminders/` has. The runtime reads `<assets>/fonts/<file>`.
const FONTS_DIR: &str = "assets/fonts";

const FONT_MANIFEST: &str = include_str!("../../../scripts/setup/fonts.manifest");

fn pinned_font_hash(name: &str) -> Option<&'static str> {
    FONT_MANIFEST.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// `fonts` is the directory the manifest's keys are relative to:
/// `<root>/assets/fonts`.
fn validate_pinned_fonts(fonts: &Path) -> Result<(), String> {
    for font in ["JetBrainsMono.ttf", "JetBrainsMono-Italic.ttf"] {
        let expected = pinned_font_hash(font)
            .ok_or_else(|| format!("{font} has no scripts/setup/fonts.manifest hash"))?;
        let path = fonts.join(font);
        let (_, actual) = sha256_file(&path).map_err(|error| error.to_string())?;
        if actual != expected {
            return Err(format!(
                "{font} differs from scripts/setup/fonts.manifest (run koru.bat fonts)"
            ));
        }
    }
    Ok(())
}

// The documents a running Koru reads beside its executables — the MCP model
// contract and the delegated-agent preamble — are `koru_update::RELEASE_DOCUMENTS`,
// not a list here: that is the list `ReleaseFiles::validate` requires, and a
// second spelling of it would be a package that fails its own validation the
// day one entry moved. Named rather than globbed for the same reason as
// fonts: the package must fail loudly if a document is absent, and must not
// accidentally publish internal docs. **The path is the contract, not just
// the name** — each document has a runtime reader looking for exactly its
// relative path, so the package keeps the `docs/` layout every shipped
// updater expects rather than flattening it. It did drift once: this listed
// `MCP.md` while the file had moved to `mcp/README.md`, and `dist-cloud`
// failed at the check in `build` rather than shipping a package whose bridge
// could not find its own contract. `every_runtime_document_is_explicit_and_present`
// is what notices now.
//
// **The installed path is not the source path.** The sources live beside the
// crate that reads them — the repository's `docs/` is a separate, private
// checkout that a contributor may not have, and a package must build without
// it — so `RELEASE_DOCUMENT_SOURCES` says which repository file is copied to
// each installed path.

/// Each of `koru_update::RELEASE_DOCUMENTS` (the installed path, under the
/// package's data root) paired with its source in the repository, relative
/// to the workspace root.
const RELEASE_DOCUMENT_SOURCES: &[(&str, &str)] = &[
    ("docs/mcp/README.md", "crates/koru-mcp-server/MCP.md"),
    (
        "docs/prompts/delegated-agent.md",
        "crates/koru-delegate-preamble/delegated-agent.md",
    ),
];

/// The repository file staged at `document`, an entry of
/// `koru_update::RELEASE_DOCUMENTS`.
fn release_document_source(document: &str) -> Result<&'static str, String> {
    RELEASE_DOCUMENT_SOURCES
        .iter()
        .find(|(installed, _)| *installed == document)
        .map(|(_, source)| *source)
        .ok_or_else(|| {
            format!("runtime document {document} has no source in RELEASE_DOCUMENT_SOURCES")
        })
}

/// Every channel-carrying binary built after the updater, paired with the
/// package-relative executable name the release owns.
///
/// Build, native signing, staging, and macOS nested-code signing all derive
/// from this table. Keeping those paths on one inventory matters more than the
/// small loop: omitting a binary from any one of four independent lists makes
/// an installed feature fail only after publication.
/// **The gateway and the session host are not here, and their absence is the
/// whole of what a package lost.** Both are roles of `koru-agent.exe` now —
/// `--worker gateway` and `--worker sessions` — so the Agent carries their
/// composition and nothing separate is built, signed or staged. The names
/// survive in `koru_update::exe` because an installation being upgraded may
/// still have the old files, and running.
#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) const CHANNEL_BINARIES: &[(&str, &str)] = &[
    ("koru", koru_update::APP_EXE),
    ("koru-agent", koru_update::AGENT_EXE),
    ("koru-mcp", koru_update::MCP_EXE),
    ("koru-ai-chat", koru_update::AI_CHAT_EXE),
    // `crates/koru-voice` already holds the contract, so both the worker
    // package and its binary take the `-worker` suffix. The executable name
    // still comes from the shared release/updater inventory rather than a
    // second literal here.
    ("koru-voice-worker", koru_update::VOICE_EXE),
];

#[cfg(windows)]
pub(crate) const CHANNEL_BINARIES: &[(&str, &str)] = &[
    ("koru", koru_update::APP_EXE),
    ("koru-agent", koru_update::AGENT_EXE),
    ("koru-com-notification", koru_update::COM_NOTIFICATION_EXE),
    ("koru-mcp", koru_update::MCP_EXE),
    ("koru-ai-chat", koru_update::AI_CHAT_EXE),
    ("koru-voice-worker", koru_update::VOICE_EXE),
];

/// **Linux ships without the voice worker** until Linux rung 3 builds it
/// (`docs/features/linux/`). A stub binary was rejected: a binary that
/// pretends. This arm is deleted when voice lands on Linux, together with the
/// Linux arm of `koru_update::ALL_EXE`.
#[cfg(target_os = "linux")]
pub(crate) const CHANNEL_BINARIES: &[(&str, &str)] = &[
    ("koru", koru_update::APP_EXE),
    ("koru-agent", koru_update::AGENT_EXE),
    ("koru-mcp", koru_update::MCP_EXE),
    ("koru-ai-chat", koru_update::AI_CHAT_EXE),
];

#[cfg(any(target_os = "macos", test))]
const MAC_NOTIFICATION_OWNER_PACKAGE: &str = "koru-notification-owner";

pub struct Package {
    pub archive: PathBuf,
    pub sha256: String,
    pub size: u64,
}

/// Where a version's archive lands. One spelling, because `dist-local` has to
/// be able to name last run's archive in order to replace it.
pub fn archive_path(root: &Path, version: &str, target: &str) -> PathBuf {
    root.join("dist/cloud").join(format!(
        "koru-{version}-{target}.{}",
        archive_extension(target)
    ))
}

/// What a target's release artifact is: the zip the Windows updater
/// extracts, the disk image a mac person drags an app out of, or the tarball a
/// Linux person extracts (`tar_gz_dir` says why its layout is the zip's).
pub fn archive_extension(target: &str) -> &'static str {
    if target.starts_with("macos") {
        "dmg"
    } else if target.starts_with("linux") {
        "tar.gz"
    } else {
        "zip"
    }
}

/// The manifest's name for [`archive_extension`]'s answer, so a manifest can
/// never describe a tarball as a zip.
pub fn package_format(target: &str) -> koru_update::PackageFormat {
    match archive_extension(target) {
        "dmg" => koru_update::PackageFormat::Dmg,
        "tar.gz" => koru_update::PackageFormat::TarGz,
        _ => koru_update::PackageFormat::Zip,
    }
}

/// The package's `NOTICE`: the repository's own, plus where this build's
/// source code is (`crate::source`).
fn write_notice(root: &Path, data: &Path, commit: &str) -> Result<(), String> {
    let repository = fs::read_to_string(root.join("NOTICE"))
        .map_err(|error| format!("NOTICE at the repository root: {error}"))?;
    fs::write(
        data.join("NOTICE"),
        crate::source::notice(&repository, commit),
    )
    .map_err(|error| error.to_string())
}

/// The baked values (`koru_env::BAKED`) this build would compile in empty.
fn unset_baked(is_set: impl Fn(&str) -> bool) -> Vec<&'static str> {
    koru_env::BAKED
        .iter()
        .copied()
        .filter(|name| !is_set(name))
        .collect()
}

/// **What a local bundle without our values does instead**, one line per
/// feature, so a community build says what it lacks before anybody clicks.
/// Each line names the behaviour the app already has for a missing value.
fn unconfigured_features(unset: &[&str], no_update_key: bool) -> Vec<String> {
    use koru_env::name as n;
    let missing = |names: &[&'static str]| -> Vec<&'static str> {
        names
            .iter()
            .copied()
            .filter(|name| unset.contains(name))
            .collect()
    };
    let mut lines = Vec::new();
    let cloud = missing(&[
        n::KORU_PUBLIC_API_ORIGIN,
        n::KORU_PUBLIC_API_ISSUER,
        n::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT,
    ]);
    if unset.contains(&n::KORU_PUBLIC_API_ORIGIN) {
        lines.push(format!(
            "Koru Cloud sign-in is not configured in this bundle ({} unset)",
            cloud.join(", ")
        ));
    } else if !cloud.is_empty() {
        lines.push(format!(
            "Koru Cloud sign-in is partly configured: {} unset",
            cloud.join(", ")
        ));
    }
    let google = missing(&[
        n::KORU_GOOGLE_DOCS_CLIENT_ID,
        n::KORU_GOOGLE_DOCS_CLIENT_SECRET,
    ]);
    if !google.is_empty() {
        lines.push(format!(
            "Google Docs import is not set up in this build ({} unset)",
            google.join(", ")
        ));
    }
    for name in missing(&[n::KORU_SUPPORT_URL, n::KORU_FEEDBACK_URL]) {
        lines.push(format!("{name} unset: the app uses its built-in default"));
    }
    if no_update_key {
        lines.push("updates are off: the bundle carries no update key".to_string());
    }
    lines
}

/// What a package bakes in from outside the tree, beyond its own identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baked<'a> {
    /// `koru_env::BAKED` from `.env.production` and the shell, as `dist-local`
    /// and `dist-cloud` have always built: loaded when the file is there, and
    /// required of every value under `native::Rule::Official`.
    Production,
    /// **None of `koru_env::BAKED`, whatever the file or the shell holds**, and
    /// the update feed at `update_base_url` — the GitHub nightly, which is
    /// built without Koru Cloud for now (NIGHTLY N5) and reads its feed from
    /// the release it is published on.
    WithoutCloud { update_base_url: &'a str },
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    root: &Path,
    version: &str,
    build_number: u64,
    channel: &str,
    commit: &str,
    released_at: &str,
    public_key: &str,
    notes: Option<&str>,
    native_rule: crate::native::Rule,
    baked: Baked<'_>,
) -> Result<Package, String> {
    if crate::host_target() == "unsupported" {
        return Err("dist-cloud does not support this host's target triple".into());
    }

    let fonts_dir = root.join(FONTS_DIR);
    let missing: Vec<_> = FONTS
        .iter()
        .filter(|font| !fonts_dir.join(font).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "bundled fonts are missing (run koru.bat fonts): {}",
            missing.into_iter().copied().collect::<Vec<_>>().join(", ")
        ));
    }
    validate_pinned_fonts(&fonts_dir)?;
    let roadmap_templates = root.join("assets/templates");
    if !roadmap_templates.is_dir() {
        return Err("Workspace templates are missing: assets/templates".into());
    }

    let reminder_sound_root = root.join("assets/reminders");
    let missing_sounds: Vec<_> = koru_native_notifications::BUNDLED_TONES
        .iter()
        .filter(|asset| !reminder_sound_root.join(asset.file).is_file())
        .map(|asset| asset.file)
        .collect();
    if !missing_sounds.is_empty() {
        return Err(format!(
            "bundled reminder sounds are missing: {}",
            missing_sounds.join(", ")
        ));
    }
    let mut missing_docs = Vec::new();
    for document in koru_update::RELEASE_DOCUMENTS {
        let source = release_document_source(document)?;
        if !root.join(source).is_file() {
            missing_docs.push(source);
        }
    }
    if !missing_docs.is_empty() {
        return Err(format!(
            "runtime documents are missing: {}",
            missing_docs.join(", ")
        ));
    }
    // The license notices (`koru_update::RELEASE_LEGAL`): `LICENSE` and
    // `NOTICE` are the repository's own and are checked before anything is
    // built; `THIRD-PARTY-NOTICES.txt` is generated after the build, from the
    // dependency graph of the binaries this package carries.
    let missing_legal: Vec<_> = koru_update::RELEASE_LEGAL
        .iter()
        .filter(|file| **file != crate::notices::FILE && !root.join(file).is_file())
        .copied()
        .collect();
    if !missing_legal.is_empty() {
        return Err(format!(
            "license files are missing from the repository root: {}",
            missing_legal.join(", ")
        ));
    }
    // **A published package names the commit it was built from** (MPL-2.0
    // §3.2; `crate::source`). `dist-cloud` reads it with the strict `git`, so
    // this fires only for a tree that is not what that path expects; a local
    // bundle without git says so in its NOTICE instead.
    if native_rule == crate::native::Rule::Official && crate::source::url(commit).is_none() {
        return Err(format!(
            "an official package must name its source commit, and {commit:?} is not a full commit id"
        ));
    }

    // **The updater is built alone and first**, because the others are
    // built with its hash in the environment and a hash cannot be taken of a
    // binary that does not exist yet. Hashing happens *after* `native_sign`:
    // Authenticode rewrites the file, so a hash taken before signing describes
    // a binary nobody will ever run.
    // **A release carries its API origin, or it is not a release.** The
    // binaries bake `koru_env::BAKED` from the process environment at build
    // time; a dev launch gets those from the shell, an installed build has
    // no shell. Loading the file here is what puts them in the environment
    // `cargo build` inherits. The first nightly with Cloud sync shipped
    // without them and could not sign in.
    //
    // **A local bundle needs none of them**: anyone can package the public
    // source with nothing from us. `dist-local` loads the file when it is there and
    // otherwise says which features stay unconfigured; `dist-cloud` keeps
    // refusing to publish without every value.
    let production = root.join(".env.production");
    let official = native_rule == crate::native::Rule::Official;
    if let Baked::WithoutCloud { .. } = baked {
        if official {
            return Err("an official release bakes in .env.production; it cannot be built without Koru Cloud".into());
        }
        println!(
            "[Koru] building without Koru Cloud: nothing from {} or the shell's {} is baked in",
            production.display(),
            koru_env::BAKED.join(", "),
        );
    } else if production.is_file() || official {
        // A file that is there but wrong is an error for either rule.
        koru_env::load_file(&production).map_err(|error| {
            format!(
                "{} holds the values a published release bakes in ({}): {error:?}",
                production.display(),
                koru_env::BAKED.join(", "),
            )
        })?;
    } else {
        println!(
            "[Koru] no {}: building a community bundle with nothing of ours baked in",
            production.display()
        );
    }
    let unset = match baked {
        Baked::Production => unset_baked(|name| koru_env::value(name).is_some()),
        Baked::WithoutCloud { .. } => koru_env::BAKED.to_vec(),
    };
    if official {
        if let Some(missing) = unset.first() {
            return Err(format!(
                "{} does not set {missing}, which a published release bakes in",
                production.display()
            ));
        }
    } else {
        for line in unconfigured_features(&unset, public_key.is_empty()) {
            println!("[Koru] WARNING: {line}");
        }
    }
    run_cargo(
        root,
        &["koru-updater"],
        version,
        build_number,
        channel,
        commit,
        released_at,
        public_key,
        None,
        baked,
    )?;
    let updater = root.join("target/release").join(koru_update::UPDATER_EXE);
    native_sign(&updater)?;
    // The mac signature must land before the hash for the same reason the
    // Authenticode one does — codesign rewrites the file. It used to happen in
    // `mac_sign`, after this hash was taken and compiled into the agent, so
    // every mac package shipped an agent whose updater check could never pass:
    // clicking Install closed the windows, stopped the gateway, failed the
    // hash, and silently restored everything — "nothing happened". `mac_sign`
    // now skips the updater; signing it twice would rewrite it again.
    // Hardened runtime, no entitlements: the updater opens no microphone, and
    // `mac_codesign_args` is tested to omit the flag rather than hand every
    // Mach-O the outer bundle's plist. (This branch used to give it the same
    // entitlements as its siblings for uniformity; `dev`'s least-privilege
    // rule for the notification owner settled the question the other way.)
    #[cfg(target_os = "macos")]
    mac_sign_one(&updater)?;
    let (_, updater_sha) = sha256_file(&updater).map_err(|error| error.to_string())?;
    let channel_packages = CHANNEL_BINARIES
        .iter()
        .map(|(package, _)| *package)
        .collect::<Vec<_>>();
    #[cfg(target_os = "macos")]
    let channel_packages = channel_packages
        .into_iter()
        .chain([MAC_NOTIFICATION_OWNER_PACKAGE])
        .collect::<Vec<_>>();
    run_cargo(
        root,
        &channel_packages,
        version,
        build_number,
        channel,
        commit,
        released_at,
        public_key,
        Some(&updater_sha),
        baked,
    )?;
    // **What the voice runtime linked, checked against the pin before anything
    // is signed or staged.** An archive under the right name with a different
    // hash still links; an official release links the canonical one or is not
    // packaged.
    crate::native::check_this_build(root, native_rule)?;
    let channel_binaries = CHANNEL_BINARIES
        .iter()
        .map(|(_, executable)| (*executable, root.join("target/release").join(*executable)))
        .collect::<Vec<_>>();
    // **Every one is signed, including the helpers nobody looks at.** Agent is
    // the process in the Run key and supervises the gateway, which makes an
    // unsigned service chain the worst possible split for
    // SmartScreen: the executable the person double-clicked is trusted and the
    // two that run behind them without being asked are not. `native_sign` is a
    // no-op when `KORU_SIGNTOOL_COMMAND` is unset, so this costs nothing locally.
    for (_, binary) in &channel_binaries {
        native_sign(binary)?;
    }

    let dist = root.join("dist/cloud");
    fs::create_dir_all(&dist).map_err(|error| error.to_string())?;
    let stage = dist.join(format!("koru-{version}-{}", crate::host_target()));
    if stage.exists() {
        ensure_under(&dist, &stage)?;
        fs::remove_dir_all(&stage).map_err(|error| error.to_string())?;
    }
    // Where things live inside the package: on Windows everything shares the
    // package root; on macOS the binaries go in `Koru.app/Contents/MacOS/`
    // and the data they read — `assets/`, `docs/`, `VERSION`, the notes —
    // goes in `Contents/Resources/`, because codesign treats everything
    // under MacOS/ as nested code and refuses to seal a font. Each runtime
    // reader carries the Resources fallback; `required()` in koru-update
    // carries the same two prefixes. Only `release-files.json` and the
    // human-facing README stay at the package root.
    let beside = stage.join(koru_update::BUNDLE_PREFIX);
    let data = stage.join(koru_update::DATA_PREFIX);
    fs::create_dir_all(&beside).map_err(|error| error.to_string())?;
    fs::create_dir_all(data.join("assets")).map_err(|error| error.to_string())?;
    fs::create_dir_all(data.join(FONTS_DIR)).map_err(|error| error.to_string())?;
    fs::create_dir_all(data.join("assets/reminders")).map_err(|error| error.to_string())?;
    fs::create_dir_all(data.join("docs")).map_err(|error| error.to_string())?;
    // **The names come from `koru_update`, not from literals here.** This is the
    // code that *writes* `release-files.json`, and `ReleaseFiles::validate` is
    // the code that reads it back and refuses a package missing any of them.
    // Two independent spellings of the same names is a package that fails
    // its own validation the day a platform stops using `.exe` — which is the
    // whole reason `koru_update::exe` exists.
    for (executable, binary) in &channel_binaries {
        fs::copy(binary, beside.join(*executable)).map_err(|error| error.to_string())?;
    }
    fs::copy(updater, beside.join(koru_update::UPDATER_EXE)).map_err(|error| error.to_string())?;
    for font in FONTS {
        fs::copy(fonts_dir.join(font), data.join(FONTS_DIR).join(font))
            .map_err(|error| error.to_string())?;
    }
    // The source moved to `assets/templates`; the installed path did not, because
    // `koru-workspace-host` looks for the templates there beside the exe.
    copy_tree(&roadmap_templates, &data.join("docs/templates/roadmap"))?;
    // The sparse package's visual identity resolves against the external
    // installation. Keep a real PNG there even though `AppListEntry=none`;
    // other shell surfaces may still ask for the package logo.
    fs::copy(root.join("assets/logo.png"), data.join("assets/logo.png"))
        .map_err(|error| error.to_string())?;
    for asset in koru_native_notifications::BUNDLED_TONES {
        fs::copy(
            reminder_sound_root.join(asset.file),
            data.join("assets/reminders").join(asset.file),
        )
        .map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    let windows_identity_packaged =
        stage_windows_identity(root, &stage, version, build_number, channel)?;
    #[cfg(not(windows))]
    let windows_identity_packaged = false;
    // Copy after the binaries are built so a package always carries the
    // repository's latest documents, not a stale build-time snapshot. The
    // preamble is also compiled into `koru-agent` as its fallback; the copy
    // shipped here is the one a person edits, and it must be at the path
    // `koru_delegate_preamble::RELATIVE_PATH` reads. Each is copied from its
    // source beside its crate to its fixed installed path.
    for document in koru_update::RELEASE_DOCUMENTS {
        let destination = data.join(document);
        // The documents are nested, so the directory may not exist yet.
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::copy(root.join(release_document_source(document)?), destination)
            .map_err(|error| error.to_string())?;
    }
    // **What every binary in this package is built from, and under which
    // licenses.** The graph is walked from the same packages that were just
    // built — the channel binaries and the updater — so the notices describe
    // this package and not the workspace.
    let notice_packages = channel_packages
        .iter()
        .copied()
        .chain(["koru-updater"])
        .collect::<Vec<_>>();
    for file in koru_update::RELEASE_LEGAL {
        if *file == crate::notices::FILE {
            crate::notices::write(root, &notice_packages, &data.join(file))?;
        } else if *file == "NOTICE" {
            write_notice(root, &data, commit)?;
        } else {
            fs::copy(root.join(file), data.join(file)).map_err(|error| error.to_string())?;
        }
    }
    fs::write(
        data.join("VERSION"),
        format!(
            "version={version}\nbuild_number={build_number}\ncommit={commit}\n\
             channel={channel}\nreleased_at={released_at}\n"
        ),
    )
    .map_err(|error| error.to_string())?;
    // No README on macOS: the disk image ships only the app and the
    // Applications symlink — the drag window is the instruction, and the
    // release page carries this build's first-open story.
    if cfg!(windows) {
        fs::write(
            stage.join("README.txt"),
            format!(
                "Koru {version}\r\n\r\n\
                 Run koru.bat (or koru.exe directly).\r\n\r\n\
                 Koru runs background processes from this same folder: the \
                 Agent, which owns the tray icon, and the workers it supervises \
                 — the API Gateway, which contains the Store worker that owns \
                 your content, and the terminal session host. Both are the \
                 Agent's own program under a different role, so you will see \
                 more than one koru-agent running. Sessions open as Workspace \
                 tabs and Koru AI Chat opens on demand. Everything is started \
                 for you; there is nothing to install or launch first.\r\n\r\n\
                 Your content is not in this folder. It lives under \
                 %LOCALAPPDATA%\\Koru\\, in a directory named for this build's \
                 release channel ({channel}), so moving, replacing or deleting this \
                 folder does not touch it.\r\n\r\n\
                 Use koru.bat --help for command-line options.\r\n\r\n\
                 Koru is free software under the Mozilla Public License 2.0. \
                 LICENSE, NOTICE and THIRD-PARTY-NOTICES.txt in this folder \
                 say what that covers and what Koru is built from.\r\n"
            ),
        )
        .map_err(|error| error.to_string())?;
    }
    // **`start ""` rather than running the exe directly, and `.bat` rather than
    // `.cmd`.** Both look like style and neither is.
    //
    // `cmd.exe` keeps a batch file *open* while it runs it, and re-reads it by
    // byte offset between lines. The old `koru.cmd` invoked `koru.exe` and
    // blocked until it exited — for the whole session — so an update that
    // landed while it was blocked renamed the launcher into `.previous/` and
    // `cmd` resumed reading at the old offset in a different file, which is
    // arbitrary text executed as commands. `start ""` returns immediately, so
    // `cmd` reaches the end of the file and closes it before anything can move
    // it. The empty `""` is the window title `start` insists on when its first
    // quoted argument would otherwise be read as one.
    //
    // **It deliberately does not start `koru-agent.exe` first.** That belongs in
    // `koru.exe` and is already there: `apps/koru/src/main.rs` dials the agent,
    // spawns one beside itself if nothing answers, waits a bounded three
    // seconds, and treats a failure as a degradation rather than something
    // fatal. A batch file can do none of those four things. And because the
    // released agent is `windows_subsystem = "windows"`, `koru-agent.exe status`
    // from a `.bat` prints nothing and `cmd` does not wait for it — so a check
    // here would be a line that always looks like it passed.
    //
    // Renaming `koru.cmd` out of the package is safe because of the `owned`
    // union in `crates/koru-updater/src/install.rs`: the update owns the union
    // of the incoming and the installed file lists, so the old launcher is moved
    // into `.previous/` and never restored. This is the one upgrade where that
    // union visibly earns its keep — narrowing it to the incoming list would
    // leave a stale `koru.cmd` beside the new `koru.bat` forever.
    if cfg!(windows) {
        fs::write(
            stage.join("koru.bat"),
            "@echo off\r\n\
             rem Koru. Everything after this is passed through, so:\r\n\
             rem   koru.bat --root \"D:\\my koru\"\r\n\
             start \"\" \"%~dp0koru.exe\" %*\r\n",
        )
        .map_err(|error| error.to_string())?;
    }
    // **A launcher entry to copy, not one that installs itself.** A tarball has
    // no install step, and a `.desktop` file names absolute paths, so the entry
    // assumes the folder the release notes suggest and says how to change it.
    // Desktop integration proper waits for an AppImage or a `.deb`.
    if cfg!(target_os = "linux") {
        let (file, text) = linux_desktop_entry(channel);
        fs::write(beside.join(file), text).map_err(|error| error.to_string())?;
    }

    // **The notes travel with the build that they describe.**
    //
    // The manifest carries only the title, the summary lines and a URL — the
    // body is rendered to HTML once, server-side, and lives on R2. That is right
    // for a link somebody shares and useless to the installed build, which is
    // the one thing that always knows *which* version it is. So the markdown
    // ships beside the executables and Koru reads its own.
    //
    // The same string the page is rendered from, so the two cannot drift.
    //
    // **Optional, because `dist-local` is.** A local bundle's default version is
    // `0.0.0-local.<commit>`, which has no descriptor and never will; omitting
    // the file is the truthful outcome, and every reader treats its absence as
    // ordinary rather than as an error.
    //
    // Not added to `koru_update::archive::required()` on purpose: that list is
    // the *strict* check applied to an incoming package, so a name in it makes
    // every package built by an older `koru-release` invalid. Notes are a
    // nicety and a build without them must still install.
    if let Some(notes) = notes {
        fs::write(data.join(koru_update::RELEASE_NOTES_FILE), notes)
            .map_err(|error| error.to_string())?;
    }

    #[cfg(target_os = "macos")]
    {
        mac_bundle_chrome(root, &stage, version, build_number)?;
        // Before the walk, so `release-files.json` lists the appex and the
        // whole-bundle swap carries it with no updater change; and before the
        // seal, because a plug-in that lands after the signature breaks it.
        mac_share_extension(root, &stage, version, build_number, channel)?;
    }

    // Walked rather than listed, which is why nothing above had to remember to
    // append itself here. On macOS the list itself rides inside the bundle
    // (`in_data`), because the artifact is a disk image holding only the app —
    // so the walk happens **before the seal**, and the signature artifacts the
    // seal adds are deliberately unlisted: the mac swap moves the whole
    // bundle, never file-by-file.
    let mut files = collect_files(&stage)?
        .into_iter()
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    let list_entry = koru_update::in_data("release-files.json");
    files.push(list_entry.clone());
    files.sort();
    let release_files = ReleaseFiles {
        schema_version: 1,
        // **One generation number, and it selects nothing.** What used to be
        // encoded here — 5 for an unsigned Windows package, 7 for a signed one
        // — is now said directly, as the `WindowsIdentity` handed to
        // `validate` below. The field stays in the JSON because an older
        // installed updater parses this document, and because a package should
        // say which generation of Koru wrote it.
        inventory_version: Some(koru_update::INVENTORY_VERSION),
        build_number,
        files,
    };
    fs::write(
        stage.join(&list_entry),
        serde_json::to_vec_pretty(&release_files).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    // **What was staged, not what the list claims.** A package that meant to
    // carry Windows notification identity and lost a file must fail here, and
    // it cannot if the requirement is derived from the list being checked.
    release_files.validate(if windows_identity_packaged {
        koru_update::WindowsIdentity::Staged
    } else {
        koru_update::WindowsIdentity::NotStaged
    })?;

    // **The seal comes after the last byte lands in the bundle, and is then
    // verified.** It did not: the release notes were written after signing,
    // so every *published* mac build — the ones that have notes — shipped
    // with a broken resource seal and answered Gatekeeper's "damaged", while
    // every local rehearsal — no descriptor, no notes — passed. The verify
    // is what turns the next ordering mistake into a failed build here
    // instead of a dialog on a customer's machine.
    #[cfg(target_os = "macos")]
    {
        mac_sign(&stage)?;
        mac_notarize(&stage)?;
        mac_verify_seal(&stage)?;
    }

    let archive = archive_path(root, version, crate::host_target());
    if archive.exists() {
        return Err(format!(
            "{} already exists; immutable release archives are never overwritten",
            archive.display()
        ));
    }
    #[cfg(target_os = "macos")]
    mac_dmg(&stage, &archive)?;
    #[cfg(not(target_os = "macos"))]
    {
        if archive_extension(crate::host_target()) == "tar.gz" {
            tar_gz_dir(&stage, &archive)?;
        } else {
            zip_dir(&stage, &archive)?;
        }
    }
    let size = archive.metadata().map_err(|error| error.to_string())?.len();
    if size > koru_update::MAX_PACKAGE_SIZE {
        return Err("release archive exceeds Koru's package limit".into());
    }
    let (_, sha256) = sha256_file(&archive).map_err(|error| error.to_string())?;
    // Printed on every publish so growth is *watched* rather than discovered at
    // the `MAX_PACKAGE_SIZE` wall, which is where a package first stops being
    // publishable at all. The file count is here for the same reason: a bundle
    // that gained a directory nobody meant to stage shows up as a count that
    // jumped, long before the megabytes do.
    println!(
        "[Koru] package: {:.1} MB, {} files -> {}",
        size as f64 / (1024.0 * 1024.0),
        release_files.files.len(),
        archive.display()
    );
    Ok(Package {
        archive,
        sha256,
        size,
    })
}

/// One `cargo build --release`, over one or more packages.
///
/// **`packages` is a slice because the channel-carrying binaries are one
/// invocation and not one each**, and that is a mechanism rather than a
/// tidiness.
/// `koru_channel::build_info()` reads `KORU_RELEASE_CHANNEL` through
/// `option_env!`, which is evaluated when *`koru-channel` itself* is compiled —
/// not when the binary that calls it is. So the channel a process reports is
/// whatever was in the environment of the build that produced the `koru-channel`
/// rlib it linked, and three separate invocations are three chances for one of
/// them to compile it under different env and get a different answer.
///
/// The answer is not cosmetic. `koru_channel::segment()` decides both which
/// `%LOCALAPPDATA%\Koru\<segment>\` a process opens and which IPC endpoint it
/// binds. A binary built without `KORU_RELEASE_CHANNEL` reports
/// `internal-release`, binds a name the other two never bind, and
/// gateway discovery refuses the mismatch rather than
/// serving it — so the failure is a shipped package whose Koru cannot find its
/// own Store, discovered by a user rather than by the packager.
///
/// One invocation makes every channel consumer carry identical build
/// metadata by construction: cargo compiles `koru-channel` once, under this
/// environment, and every binary in the invocation links that one artefact.
/// The updater was built immediately before them under the same environment.
#[allow(clippy::too_many_arguments)]
fn run_cargo(
    root: &Path,
    packages: &[&str],
    version: &str,
    build_number: u64,
    channel: &str,
    commit: &str,
    released_at: &str,
    public_key: &str,
    updater_sha: Option<&str>,
    baked: Baked<'_>,
) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["build", "--release", "--locked"]);
    for package in packages {
        command.args(["-p", package]);
    }
    command
        .env("KORU_VERSION", version)
        .env("KORU_BUILD_NUMBER", build_number.to_string())
        .env("KORU_RELEASE_CHANNEL", channel)
        .env("KORU_COMMIT", commit)
        .env("KORU_RELEASED_AT", released_at)
        .env("KORU_UPDATE_PUBLIC_KEY", public_key);
    if let Some(hash) = updater_sha {
        command.env("KORU_UPDATER_SHA256", hash);
    }
    // Removed rather than left to the shell: `option_env!` would bake a
    // value a developer happened to export, and this build promises none.
    if let Baked::WithoutCloud { update_base_url } = baked {
        for name in koru_env::BAKED {
            command.env_remove(name);
        }
        command.env(koru_env::name::KORU_UPDATE_BASE_URL, update_base_url);
    }
    // **The product feature flags are baked here or nowhere.** Each is read by
    // `option_env!` inside `koru-features` alone — the same per-crate-compile
    // trap as the channel above — so this one invocation decides what every
    // binary in it shows. A set variable is normalised to `1` or `0` with
    // koru-env's rule, so `KORU_ENABLE_AGENTS=off` in `.env.production` reaches
    // the compiler as an explicit off; an unset one is removed and
    // `koru-features` decides — off for every surface except agents
    // (guide/contributing/FEATURE-FLAGS.md). Removing a set `0` instead would
    // turn agents *on*.
    for name in koru_features::env::ALL {
        match std::env::var(name) {
            Ok(value) => {
                let on = koru_env::switch_value(Some(&value));
                command.env(name, if on { "1" } else { "0" });
            }
            Err(_) => {
                command.env_remove(name);
            }
        }
    }
    let status = command.status().map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("release build for {} failed", packages.join(", ")))
    }
}

fn native_sign(path: &Path) -> Result<(), String> {
    let Ok(template) = std::env::var("KORU_SIGNTOOL_COMMAND") else {
        return Ok(());
    };
    let quoted = format!("\"{}\"", path.display());
    let command = template.replace("{file}", &quoted);
    let status = crate::shell(&command)
        .status()
        .map_err(|error| format!("could not start native signing hook: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("native signing hook failed for {}", path.display()))
    }
}

#[cfg(any(windows, test))]
fn windows_package_version(version: &str, build_number: u64) -> Result<String, String> {
    let parsed = semver::Version::parse(version.trim_start_matches('v'))
        .map_err(|error| format!("invalid release version for Windows identity: {error}"))?;
    let parts = [parsed.major, parsed.minor, parsed.patch, build_number];
    if parts.iter().any(|part| *part > u16::MAX as u64) {
        return Err(format!(
            "Windows package version fields must be at most {}; got {version}.{build_number}",
            u16::MAX
        ));
    }
    Ok(format!(
        "{}.{}.{}.{}",
        parsed.major, parsed.minor, parsed.patch, build_number
    ))
}

#[cfg(any(windows, test))]
fn windows_identity_manifest(
    identity: &koru_winres::PackageIdentity,
    version: &str,
    architecture: &str,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10"
         xmlns:uap10="http://schemas.microsoft.com/appx/manifest/uap/windows10/10"
         xmlns:desktop="http://schemas.microsoft.com/appx/manifest/desktop/windows10"
         xmlns:com="http://schemas.microsoft.com/appx/manifest/com/windows10"
         xmlns:rescap="http://schemas.microsoft.com/appx/manifest/foundation/windows10/restrictedcapabilities"
         IgnorableNamespaces="uap uap10 desktop com rescap">
  <Identity Name="{}" Publisher="{}" Version="{}" ProcessorArchitecture="{}"/>
  <Properties>
    <DisplayName>Koru</DisplayName>
    <PublisherDisplayName>Koru</PublisherDisplayName>
    <Logo>assets\StoreLogo.png</Logo>
    <uap10:AllowExternalContent>true</uap10:AllowExternalContent>
  </Properties>
  <Resources>
    <Resource Language="en-us"/>
  </Resources>
  <Dependencies>
    <TargetDeviceFamily Name="Windows.Desktop" MinVersion="10.0.19041.0" MaxVersionTested="10.0.26100.0"/>
  </Dependencies>
  <Capabilities>
    <rescap:Capability Name="runFullTrust"/>
    <rescap:Capability Name="unvirtualizedResources"/>
  </Capabilities>
  <Applications>
    <Application Id="{}" Executable="koru-agent.exe" uap10:RuntimeBehavior="win32App" uap10:TrustLevel="mediumIL">
      <uap:VisualElements
          AppListEntry="none" DisplayName="Koru" Description="Koru reminders"
          Square150x150Logo="assets\Square150x150Logo.png" Square44x44Logo="assets\Square44x44Logo.png" BackgroundColor="transparent"/>
    </Application>
    <Application Id="{}" Executable="koru-com-notification.exe" uap10:RuntimeBehavior="win32App" uap10:TrustLevel="mediumIL">
      <uap:VisualElements
          AppListEntry="none" DisplayName="Koru" Description="Koru reminders"
          Square150x150Logo="assets\Square150x150Logo.png" Square44x44Logo="assets\Square44x44Logo.png" BackgroundColor="transparent"/>
      <Extensions>
        <desktop:Extension Category="windows.toastNotificationActivation" Executable="koru-com-notification.exe" EntryPoint="Windows.FullTrustApplication">
          <desktop:ToastNotificationActivation ToastActivatorCLSID="{}"/>
        </desktop:Extension>
        <com:Extension Category="windows.comServer">
          <com:ComServer>
            <com:ExeServer Executable="koru-com-notification.exe" DisplayName="Koru reminder activation">
              <com:Class Id="{}" DisplayName="Koru reminder activation"/>
            </com:ExeServer>
          </com:ComServer>
        </com:Extension>
      </Extensions>
    </Application>
  </Applications>
</Package>"#,
        identity.name,
        xml_attribute(&identity.publisher),
        version,
        architecture,
        identity.application_id,
        koru_native_notifications::WINDOWS_NOTIFICATION_APPLICATION_ID,
        identity.toast_activator_clsid,
        identity.toast_activator_clsid,
    )
}

#[cfg(any(windows, test))]
fn xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(windows)]
fn stage_windows_identity(
    root: &Path,
    stage: &Path,
    version: &str,
    build_number: u64,
    channel: &str,
) -> Result<bool, String> {
    let publisher = std::env::var("KORU_WINDOWS_PACKAGE_PUBLISHER")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let signing = std::env::var("KORU_SIGNTOOL_COMMAND")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let publisher = match (publisher, signing) {
        (None, None) => return Ok(false),
        (Some(_), None) => {
            return Err("KORU_WINDOWS_PACKAGE_PUBLISHER requires KORU_SIGNTOOL_COMMAND; the identity MSIX is never emitted unsigned".into());
        }
        (None, Some(_)) => {
            return Err("KORU_WINDOWS_PACKAGE_PUBLISHER must match the Subject of the certificate used by KORU_SIGNTOOL_COMMAND".into());
        }
        (Some(publisher), Some(_)) => publisher,
    };
    let segment = koru_channel::segment_of(
        koru_channel::ReleaseChannel::from_build_channel(channel),
        false,
    );
    let identity = koru_winres::notification_identity(segment, publisher);
    let package_version = windows_package_version(version, build_number)?;
    // The sparse package contains no executable code. Microsoft's external-
    // location guidance requires `neutral`, so this one identity works for
    // both x64 and ARM64 ZIPs signed by the same publisher.
    let architecture = "neutral";
    let source = stage.with_extension("identity-source");
    if source.exists() {
        ensure_under(
            stage
                .parent()
                .ok_or_else(|| "stage has no parent".to_string())?,
            &source,
        )?;
        fs::remove_dir_all(&source).map_err(|error| error.to_string())?;
    }
    stage_windows_identity_assets(root, stage)?;
    stage_windows_identity_source(&source, &identity, &package_version, architecture)?;
    validate_windows_identity_layout(stage, &source)?;
    let output = stage.join(koru_update::WINDOWS_IDENTITY_PACKAGE);
    // MakeAppx cannot pack an identity-only external-location package with its
    // ordinary file-existence validation: by definition, the declared COM
    // broker and logos live at the external location rather than in the MSIX. The
    // preceding call validates that exact manifest against a temporary mirror
    // of the real external layout. `/nv` here is therefore the sparse encoding
    // step, never a substitute for validation.
    let packed = makeappx_pack(&source, &output, true);
    let removed = fs::remove_dir_all(&source).map_err(|error| error.to_string());
    packed?;
    removed?;
    native_sign(&output)?;
    Ok(true)
}

#[cfg(windows)]
fn stage_windows_identity_source(
    source: &Path,
    identity: &koru_winres::PackageIdentity,
    package_version: &str,
    architecture: &str,
) -> Result<(), String> {
    fs::create_dir_all(source).map_err(|error| error.to_string())?;
    fs::write(
        source.join("AppxManifest.xml"),
        windows_identity_manifest(&identity, &package_version, architecture),
    )
    .map_err(|error| error.to_string())
}

#[cfg(windows)]
const WINDOWS_IDENTITY_LOGOS: &[(&str, u32)] = &[
    ("StoreLogo.png", 50),
    ("Square44x44Logo.png", 44),
    ("Square150x150Logo.png", 150),
];

#[cfg(windows)]
fn stage_windows_identity_assets(root: &Path, external: &Path) -> Result<(), String> {
    let assets = external.join("assets");
    fs::create_dir_all(&assets).map_err(|error| error.to_string())?;
    for (name, size) in WINDOWS_IDENTITY_LOGOS {
        write_windows_logo(root, &assets.join(name), *size)?;
    }
    Ok(())
}

#[cfg(windows)]
fn validate_windows_identity_layout(external: &Path, source: &Path) -> Result<(), String> {
    let validation = source.with_extension("validation");
    let output = source.with_extension("validation.msix");
    fs::create_dir_all(validation.join("Assets")).map_err(|error| error.to_string())?;
    fs::copy(
        source.join("AppxManifest.xml"),
        validation.join("AppxManifest.xml"),
    )
    .map_err(|error| error.to_string())?;
    fs::copy(
        external.join(koru_update::AGENT_EXE),
        validation.join(koru_update::AGENT_EXE),
    )
    .map_err(|error| format!("could not stage Agent for manifest validation: {error}"))?;
    fs::copy(
        external.join(koru_update::COM_NOTIFICATION_EXE),
        validation.join(koru_update::COM_NOTIFICATION_EXE),
    )
    .map_err(|error| format!("could not stage COM broker for manifest validation: {error}"))?;
    for (name, _) in WINDOWS_IDENTITY_LOGOS {
        fs::copy(
            external.join("assets").join(name),
            validation.join("Assets").join(name),
        )
        .map_err(|error| format!("could not stage {name} for manifest validation: {error}"))?;
    }
    let validated = makeappx_pack(&validation, &output, false);
    let remove_layout = fs::remove_dir_all(&validation).map_err(|error| error.to_string());
    let remove_package = fs::remove_file(&output).map_err(|error| error.to_string());
    validated?;
    remove_layout?;
    remove_package
}

#[cfg(windows)]
fn makeappx_pack(source: &Path, output: &Path, skip_validation: bool) -> Result<(), String> {
    let makeappx = koru_winres::find_rc()
        .and_then(|(rc, _)| rc.parent().map(|directory| directory.join("MakeAppx.exe")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("MakeAppx.exe"));
    let mut command = Command::new(makeappx);
    command.args(["pack", "/o", "/d"]).arg(source);
    if skip_validation {
        command.arg("/nv");
    }
    let status = command
        .arg("/p")
        .arg(output)
        .status()
        .map_err(|error| format!("could not start MakeAppx.exe: {error}"))?;
    if !status.success() {
        return Err("MakeAppx.exe failed to build the Windows identity package".into());
    }
    Ok(())
}

#[cfg(windows)]
fn write_windows_logo(root: &Path, destination: &Path, size: u32) -> Result<(), String> {
    let image = image::open(root.join("assets/logo.png"))
        .map_err(|error| format!("could not read Windows package logo: {error}"))?
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3);
    image
        .save_with_format(destination, image::ImageFormat::Png)
        .map_err(|error| format!("could not write Windows package logo: {error}"))
}

/// Sign the bundle, ad hoc by default — because **an unsigned bundle under
/// quarantine is "damaged"**, not "unidentified": on Apple silicon,
/// Gatekeeper's dialog for a downloaded app with no bundle signature offers
/// only the Trash, and right-click → Open rescues nothing. An ad-hoc
/// signature is what turns that into the "unidentified developer" flow the
/// README describes. Found by downloading a published build through Safari.
///
/// `KORU_MAC_SIGN_COMMAND` (a template with `{file}`) replaces the ad-hoc
/// identity with a real one — Developer ID, hardened runtime — and is Phase
/// B's seam; see docs/features/macos/RELEASING.md. Inside-out either way: the helpers
/// helpers first, then the bundle, whose signature covers the main
/// executable and seals Info.plist and Resources — which is also why this
/// runs after every file has landed in the bundle and before the zip.
#[cfg(target_os = "macos")]
fn mac_sign(stage: &Path) -> Result<(), String> {
    let bundle = stage.join("Koru.app");
    // **Beside the staging directory, never inside it.** Anything under
    // `stage` is walked into `release-files.json` and shipped; this is signing
    // input, and a shipped entitlements file would be a list entry no
    // installation has and a strict-check failure on every incoming package.
    let entitlements = entitlements_path(stage);
    fs::write(&entitlements, HARDENED_ENTITLEMENTS).map_err(|error| error.to_string())?;
    let signed = (|| {
        let owner = notification_owner_bundle(&bundle);
        // The notification owner never opens audio. Keep its two nested
        // signatures hardened but entitlement-free; only the existing outer
        // executable set inherits the audio-input entitlement below.
        mac_sign_one(
            &owner
                .join("Contents/MacOS")
                .join(koru_native_notifications::MAC_OWNER_EXECUTABLE),
        )?;
        mac_sign_one(&owner)?;
        let helpers = CHANNEL_BINARIES
            .iter()
            .map(|(_, executable)| *executable)
            // No app here: the outer bundle signing covers it. No updater here: it
            // was signed **before** its sha256 was taken and
            // compiled into the agent (see `build`), and signing is not idempotent —
            // a second pass would rewrite the bytes and break that embedded check,
            // which is exactly the bug the old fixed list caused. The staged copy
            // carries the early signature with it.
            .filter(|executable| {
                *executable != koru_update::APP_EXE && *executable != koru_update::UPDATER_EXE
            });
        // **Every executable, not only the one that opens the device.** The
        // microphone is opened by `koru-voice-worker`, and it is a separately signed
        // Mach-O inside the bundle: an entitlement on the outer bundle alone does
        // not reach it. Giving the whole set the same one entitlement is the
        // smaller mistake than reasoning per binary about which one might one
        // day capture audio and getting it wrong once.
        for helper in helpers.collect::<Vec<_>>() {
            mac_sign_one_with_entitlements(
                &bundle.join("Contents/MacOS").join(helper),
                &entitlements,
            )?;
        }
        // **The plug-in is signed as a bundle, and before its host.** Signing the
        // appex seals its own Info.plist and its executable in one step; the app's
        // signature then covers the sealed plug-in. Doing it the other way round
        // produces a bundle whose seal `codesign --verify --deep` rejects, which
        // is what `mac_verify_seal` is there to catch.
        //
        // It gets the same entitlements as everything else, by the rule stated
        // above: the extension captures no audio and does not need the microphone,
        // but reasoning per binary is the mistake that costs more than the
        // over-grant. What the extension *does* need — a sandbox — it still does
        // not have, and that is TECHDEBT 83 rather than something to settle here.
        let appex = bundle.join("Contents/PlugIns/KoruShare.appex");
        if appex.is_dir() {
            mac_sign_one_with_entitlements(&appex, &entitlements)?;
        }
        mac_sign_one_with_entitlements(&bundle, &entitlements)
    })();
    let _ = fs::remove_file(&entitlements);
    signed
}

/// Notarize and staple the bundle, when this machine is set up to.
///
/// `KORU_MAC_NOTARIZE_COMMAND` is a `{file}` template handed a zip of the
/// bundle — typically
/// `xcrun notarytool submit {file} --keychain-profile koru-notary --wait` —
/// and, like the signing hook, it keeps this crate free of Apple tooling and
/// of credentials. Unset, the step is skipped and the package ships with the
/// could-not-verify first-open dialog (docs/features/macos/RELEASING.md).
///
/// Three facts shape the order. **The submission zip is `ditto`'s**, because
/// notarytool is particular about zip structure and ditto is the tool Apple
/// tests against; it is a throwaway, not the shipped archive. **The staple
/// lands on the .app afterward**, so the ticket is in the bytes the real
/// archive then carries — which is why this runs before `zip_dir`. And **a
/// staple failure is fatal**: a release that claims notarization and cannot
/// prove it offline is worse than one that says could-not-verify, because
/// the failure surfaces on the customer's machine instead of here.
#[cfg(target_os = "macos")]
fn mac_notarize(stage: &Path) -> Result<(), String> {
    let Ok(template) = std::env::var("KORU_MAC_NOTARIZE_COMMAND") else {
        println!(
            "[Koru] KORU_MAC_NOTARIZE_COMMAND is not set; skipping notarization \
             (first open will say could-not-verify)"
        );
        return Ok(());
    };
    let bundle = stage.join("Koru.app");
    let submission = stage.join("koru-notarize-submission.zip");
    let status = Command::new("ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(&bundle)
        .arg(&submission)
        .status()
        .map_err(|error| format!("could not run ditto: {error}"))?;
    if !status.success() {
        return Err("ditto failed to build the notarization submission".into());
    }
    let quoted = format!("\"{}\"", submission.display());
    let submitted = crate::shell(&template.replace("{file}", &quoted))
        .status()
        .map_err(|error| format!("could not start the notarization hook: {error}"))?;
    // The submission zip is spent either way; the staple below is what
    // carries the outcome into the package.
    let _ = fs::remove_file(&submission);
    if !submitted.success() {
        return Err("notarization failed; the log names the reason (notarytool log <id>)".into());
    }
    let stapled = Command::new("xcrun")
        .args(["stapler", "staple"])
        .arg(&bundle)
        .status()
        .map_err(|error| format!("could not run stapler: {error}"))?;
    if !stapled.success() {
        return Err("stapling failed after a successful notarization".into());
    }
    println!("[Koru] notarized and stapled Koru.app");
    Ok(())
}

/// The disk image: `Koru.app` and an Applications symlink, the drag-install
/// every mac user already knows — no README, no loose files, because the
/// window *is* the instruction. The app is stapled before it goes in, so a
/// copy dragged out passes Gatekeeper offline; the image itself is signed
/// with the same identity (or ad hoc) and needs no staple of its own.
#[cfg(target_os = "macos")]
fn mac_dmg(stage: &Path, archive: &Path) -> Result<(), String> {
    let dmg_root = stage.with_file_name(format!(
        "{}-dmgroot",
        stage.file_name().and_then(|n| n.to_str()).unwrap_or("koru")
    ));
    if dmg_root.exists() {
        fs::remove_dir_all(&dmg_root).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&dmg_root).map_err(|error| error.to_string())?;
    // ditto, not fs::copy: the sealed bundle's structure — symlink-free
    // today, but the seal is exact — must arrive byte-for-byte.
    let copied = Command::new("ditto")
        .arg(stage.join("Koru.app"))
        .arg(dmg_root.join("Koru.app"))
        .status()
        .map_err(|error| format!("could not run ditto: {error}"))?;
    if !copied.success() {
        return Err("ditto failed to stage the disk image".into());
    }
    std::os::unix::fs::symlink("/Applications", dmg_root.join("Applications"))
        .map_err(|error| error.to_string())?;
    // The Koru mark on the *mounted volume*: `.VolumeIcon.icns` at the root
    // plus the Finder custom-icon bit on the volume root. The downloaded
    // .dmg file itself keeps the generic glyph — a file's custom icon lives
    // in a resource-fork xattr that HTTP strips, for every vendor alike. The
    // bit lives in the volume's FinderInfo, so the image is built writable,
    // stamped while mounted, and only then converted to the compressed
    // read-only final.
    fs::copy(
        stage.join("Koru.app/Contents/Resources/koru.icns"),
        dmg_root.join(".VolumeIcon.icns"),
    )
    .map_err(|error| error.to_string())?;
    let writable = archive.with_extension("udrw.dmg");
    if writable.exists() {
        fs::remove_file(&writable).map_err(|error| error.to_string())?;
    }
    let created = Command::new("hdiutil")
        .args([
            "create",
            "-volname",
            "Koru",
            "-format",
            "UDRW",
            "-ov",
            "-srcfolder",
        ])
        .arg(&dmg_root)
        .arg(&writable)
        .status()
        .map_err(|error| format!("could not run hdiutil: {error}"))?;
    fs::remove_dir_all(&dmg_root).map_err(|error| error.to_string())?;
    if !created.success() {
        return Err("hdiutil failed to create the disk image".into());
    }
    stamp_volume_icon(&writable)?;
    let converted = Command::new("hdiutil")
        .args(["convert", "-format", "UDZO", "-ov", "-o"])
        .arg(archive)
        .arg(&writable)
        .status()
        .map_err(|error| format!("could not run hdiutil convert: {error}"))?;
    fs::remove_file(&writable).map_err(|error| error.to_string())?;
    if !converted.success() {
        return Err("hdiutil failed to compress the disk image".into());
    }
    // **No entitlements: a disk image is not code.** The signature is here so
    // Gatekeeper can attribute the download; there is no process inside it for
    // a microphone grant to reach.
    mac_sign_one(archive)
}

/// Mount the writable image and set `kHasCustomIcon` on the volume root —
/// the 0x0400 flag at offset 8 of the 32-byte `com.apple.FinderInfo`, which
/// is what makes Finder read `.VolumeIcon.icns`. `SetFile -a C` is the
/// classic spelling; it is deprecated and not guaranteed installed, and this
/// xattr is the whole of what it did.
#[cfg(target_os = "macos")]
fn stamp_volume_icon(writable: &Path) -> Result<(), String> {
    let mount = writable.with_extension("mount");
    fs::create_dir_all(&mount).map_err(|error| error.to_string())?;
    let attached = Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-mountpoint"])
        .arg(&mount)
        .arg(writable)
        .status()
        .map_err(|error| format!("could not run hdiutil attach: {error}"))?;
    if !attached.success() {
        return Err("hdiutil failed to mount the writable image".into());
    }
    let mut finder_info = [0u8; 32];
    finder_info[8] = 0x04; // kHasCustomIcon: big-endian finderFlags at offset 8
    let hex: String = finder_info.iter().map(|b| format!("{b:02X}")).collect();
    let stamped = Command::new("xattr")
        .args(["-wx", "com.apple.FinderInfo", &hex])
        .arg(&mount)
        .status()
        .map_err(|error| format!("could not run xattr: {error}"));
    let detached = Command::new("hdiutil")
        .args(["detach"])
        .arg(&mount)
        .status()
        .map_err(|error| format!("could not run hdiutil detach: {error}"))?;
    let _ = fs::remove_dir(&mount);
    match stamped {
        Ok(status) if status.success() => {}
        Ok(_) => return Err("could not stamp the volume's custom-icon bit".into()),
        Err(error) => return Err(error),
    }
    if !detached.success() {
        return Err("hdiutil failed to detach the writable image".into());
    }
    Ok(())
}

/// The packager's own Gatekeeper rehearsal: the seal must survive everything
/// this function did, or the build fails here rather than on a customer's
/// machine.
#[cfg(target_os = "macos")]
fn mac_verify_seal(stage: &Path) -> Result<(), String> {
    let status = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(stage.join("Koru.app"))
        .status()
        .map_err(|error| format!("could not run codesign --verify: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("the bundle's seal is broken — something wrote into Koru.app after signing".into())
    }
}

/// **What macOS reads out of the bundle**, as a string this build can produce
/// and check on any platform.
///
/// Pure, and **not** `#[cfg(target_os = "macos")]`, which is the point: the
/// generator that writes it only compiles on a Mac, so a Windows or Linux
/// developer could add a key, ship it, and never learn that they had spelled
/// it wrong. Splitting the text out means
/// `the_bundle_asks_for_the_microphone_in_words` runs everywhere.
///
/// **`NSMicrophoneUsageDescription` is not optional decoration.** macOS
/// terminates a process that touches an audio input device without it — not a
/// refusal a program can report, a kill — and the string is what the system
/// consent dialog puts in front of somebody. So it says what the microphone is
/// *for* and where the audio goes, because the alternative is a prompt that
/// reads as an app asking to listen for reasons it will not give.
///
/// The binary that opens the device is `koru-voice-worker`, inside
/// `Contents/MacOS/`, and this key is on the *bundle* rather than beside that
/// executable. That is the roadmap's one unproven mechanism (Q-V36) and it is
/// unproven here too: this tree has no Mac. See Q-V73 for exactly what has to
/// be checked and what changes if it does not hold.
/// `#[allow(dead_code)]` rather than a `cfg`: the only caller is macOS-only,
/// and the whole reason this is a separate item is that the check has to run
/// where there is no Mac.
#[allow(dead_code)]
pub(crate) fn info_plist(core: &str, version: &str, build_number: u64) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Koru</string>
    <key>CFBundleDisplayName</key><string>Koru</string>
    <key>CFBundleIdentifier</key><string>do.koru.desktop</string>
    <key>CFBundleExecutable</key><string>koru</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>koru</string>
    <key>CFBundleShortVersionString</key><string>{core}</string>
    <key>CFBundleVersion</key><string>{build_number}</string>
    <key>CFBundleGetInfoString</key><string>Koru {version}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSMicrophoneUsageDescription</key><string>Koru uses the microphone for voice input. Speech is transcribed on this Mac and never leaves it.</string>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>Koru link</string>
            <key>CFBundleURLSchemes</key>
            <array><string>koru</string></array>
        </dict>
    </array>
</dict>
</plist>
"#
    )
}

/// The separately identified background app which owns macOS notifications.
/// Pure so its identity and background-only shape are checked on every host.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn notification_owner_info_plist(
    core: &str,
    version: &str,
    build_number: u64,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Koru Agent</string>
    <key>CFBundleDisplayName</key><string>Koru Agent</string>
    <key>CFBundleIdentifier</key><string>{}</string>
    <key>CFBundleExecutable</key><string>{}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{core}</string>
    <key>CFBundleVersion</key><string>{build_number}</string>
    <key>CFBundleGetInfoString</key><string>Koru Agent {version}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSUIElement</key><true/>
</dict>
</plist>
"#,
        koru_native_notifications::MAC_OWNER_BUNDLE_ID,
        koru_native_notifications::MAC_OWNER_EXECUTABLE,
    )
}

#[cfg(any(target_os = "macos", test))]
fn notification_owner_bundle(outer_bundle: &Path) -> PathBuf {
    outer_bundle
        .join("Contents/Library/LoginItems")
        .join(koru_native_notifications::MAC_OWNER_APP)
}

/// **What the hardened runtime is asked to allow.**
///
/// One entitlement, and it is the microphone. Under the hardened runtime —
/// which notarization requires — a process is denied audio input unless
/// `com.apple.security.device.audio-input` is in its signature, and the denial
/// arrives as an ordinary open failure with nothing in it that names the cause.
/// `Info.plist`'s usage description and this are **both** needed and neither
/// substitutes for the other: the plist is what the consent dialog says, and
/// this is what lets the request be made at all.
///
/// A separate constant rather than a file in `assets/` because it is signing
/// input rather than shipped content: it must not appear in
/// `release-files.json`, and a file staged inside the bundle would.
#[allow(dead_code)]
pub(crate) const HARDENED_ENTITLEMENTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.device.audio-input</key><true/>
</dict>
</plist>
"#;

/// The entitlements file's name, beside the staging directory rather than in
/// it — see [`HARDENED_ENTITLEMENTS`].
#[allow(dead_code)]
pub(crate) const ENTITLEMENTS_FILE: &str = "koru-entitlements.plist";

/// Where the entitlements plist is written for a signing run.
///
/// **A sibling of the staging directory, never a child.** `collect_files` walks
/// everything under `stage` into `release-files.json`, and that list is the
/// strict check an *incoming* package is measured against: a name in it that no
/// published archive carries invalidates every one of them. Pure, so the rule
/// is asserted on a platform that cannot run `codesign`.
#[allow(dead_code)]
pub(crate) fn entitlements_path(stage: &Path) -> PathBuf {
    stage.with_file_name(ENTITLEMENTS_FILE)
}

/// The direct `codesign` argv for one hardened Mach-O or bundle.
///
/// Pure so least privilege remains testable off macOS: omitting an entitlement
/// must also omit the `--entitlements` flag rather than reusing the outer
/// bundle's microphone plist.
#[allow(dead_code)]
pub(crate) fn mac_codesign_args(path: &Path, entitlements: Option<&Path>) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--force"),
        OsString::from("--options"),
        OsString::from("runtime"),
    ];
    if let Some(entitlements) = entitlements {
        args.push(OsString::from("--entitlements"));
        args.push(entitlements.as_os_str().to_owned());
    }
    args.extend([
        OsString::from("--sign"),
        OsString::from("-"),
        path.as_os_str().to_owned(),
    ]);
    args
}

/// Sign one Mach-O or bundle with the hardened runtime and no entitlements.
///
/// This is used by the updater, disk image, and narrow notification owner.
/// `KORU_MAC_SIGN_COMMAND` remains the real-identity seam; a custom command is
/// responsible for keeping its own entitlement policy at least as narrow.
#[cfg(target_os = "macos")]
fn mac_sign_one(path: &Path) -> Result<(), String> {
    mac_sign_one_with_optional_entitlements(path, None)
}

/// Sign one Mach-O or bundle with the hardened runtime and the microphone
/// entitlement.
///
/// `--options runtime` is not optional: notarization refuses a submission
/// without it. Turning it on is what makes the entitlement necessary, because
/// the hardened runtime denies audio input to a process whose signature does
/// not ask for it — the two arrive together or the microphone stops working on
/// the first notarized build, which is a failure nobody sees locally.
///
/// **`entitlements` is an `Option` because a disk image is not code.** An
/// entitlement is a property of an executable, and the `.dmg` is a container
/// that is signed so Gatekeeper can attribute it — there is no process for a
/// microphone grant to apply to. Passing `None` is what says so; every Mach-O
/// and every bundle passes `Some`, including the ones that will never open a
/// device, by the rule [`mac_sign`] states about not reasoning per binary.
///
/// `KORU_MAC_SIGN_COMMAND` keeps this crate free of Apple tooling and of
/// credentials, and it gains a second placeholder: `{entitlements}` is the
/// plist's path, so a hook that signs with a real identity can pass it on. A
/// template that names only `{file}` still works and is what most of them are —
/// it is then that hook's business whether it hardens, which is the same trade
/// the notarization hook already makes. **A template that does name
/// `{entitlements}` is only ever handed one for code**: on the disk image the
/// placeholder expands to nothing, so such a hook has to spell the flag as
/// `{entitlements}` alone rather than `--entitlements {entitlements}`, or it
/// will hand `codesign` a bare flag on that one call. Untested against a real
/// identity — see the macOS catch-up.
#[cfg(target_os = "macos")]
fn mac_sign_one_with_entitlements(path: &Path, entitlements: &Path) -> Result<(), String> {
    mac_sign_one_with_optional_entitlements(path, Some(entitlements))
}

#[cfg(target_os = "macos")]
fn mac_sign_one_with_optional_entitlements(
    path: &Path,
    entitlements: Option<&Path>,
) -> Result<(), String> {
    let status = if let Ok(template) = std::env::var("KORU_MAC_SIGN_COMMAND") {
        let quoted = format!("\"{}\"", path.display());
        let quoted_entitlements = entitlements
            .map(|path| format!("\"{}\"", path.display()))
            .unwrap_or_else(|| "\"\"".into());
        // An empty placeholder lets a wrapper choose the no-entitlement form.
        // A direct `--entitlements {entitlements}` template instead fails
        // safely for the owner rather than granting the outer plist.
        let command = template
            .replace("{file}", &quoted)
            .replace("{entitlements}", &quoted_entitlements);
        crate::shell(&command)
            .status()
            .map_err(|error| format!("could not start the mac signing hook: {error}"))?
    } else {
        Command::new("codesign")
            .args(mac_codesign_args(path, entitlements))
            .status()
            .map_err(|error| format!("could not run codesign: {error}"))?
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("signing failed for {}", path.display()))
    }
}

/// What macOS itself reads from the bundle, as opposed to what the binaries
/// read from beside themselves: `Info.plist` and the Finder icon.
///
/// The identifier deliberately is not the iOS app's `do.koru.app` — two apps
/// with one identity confuse everything from notarization to LaunchServices.
/// `CFBundleURLTypes` is how a mac build registers `koru://`; the per-channel
/// question (five channels, one LaunchServices winner) is recorded in
/// docs/features/macos/RELEASING.md rather than guessed here.
#[cfg(target_os = "macos")]
fn mac_bundle_chrome(
    root: &Path,
    stage: &Path,
    version: &str,
    build_number: u64,
) -> Result<(), String> {
    let contents = stage.join("Koru.app/Contents");
    fs::create_dir_all(contents.join("Resources")).map_err(|error| error.to_string())?;
    let icon = root.join("assets/koru.icns");
    if !icon.is_file() {
        return Err("assets/koru.icns is missing (run scripts/icons/set-logo.py)".into());
    }
    fs::copy(icon, contents.join("Resources/koru.icns")).map_err(|error| error.to_string())?;
    // CFBundleShortVersionString wants dotted numbers; the channel decoration
    // (`-beta.0`, `-nightly.<date>`) lives in our own VERSION file, so the
    // core is the honest value here and the full string stays visible in
    // Get Info via CFBundleGetInfoString.
    let core = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .to_string();
    fs::write(
        contents.join("Info.plist"),
        info_plist(&core, version, build_number),
    )
    .map_err(|error| error.to_string())?;

    let owner = notification_owner_bundle(&stage.join("Koru.app"));
    let owner_contents = owner.join("Contents");
    fs::create_dir_all(owner_contents.join("MacOS")).map_err(|error| error.to_string())?;
    let owner_sounds = owner_contents.join("Resources/assets/reminders");
    fs::create_dir_all(&owner_sounds).map_err(|error| error.to_string())?;
    fs::copy(
        root.join("target/release")
            .join(koru_native_notifications::MAC_OWNER_EXECUTABLE),
        owner_contents
            .join("MacOS")
            .join(koru_native_notifications::MAC_OWNER_EXECUTABLE),
    )
    .map_err(|error| error.to_string())?;
    for asset in koru_native_notifications::BUNDLED_TONES {
        fs::copy(
            root.join("assets/reminders").join(asset.file),
            owner_sounds.join(asset.file),
        )
        .map_err(|error| error.to_string())?;
    }
    fs::write(
        owner_contents.join("Info.plist"),
        notification_owner_info_plist(&core, version, build_number),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Koru's row in the macOS share sheet: a real `.appex` at
/// `Koru.app/Contents/PlugIns/KoruShare.appex` — [ADR-416].
///
/// **There is no cheaper mechanism.** A row in the sheet is a Share Extension
/// and nothing else: no plist-only registration, no LaunchServices key, and
/// `NSServices` is a different surface that does not appear there.
/// LaunchServices discovers the plug-in when the containing app is registered.
///
/// **`swiftc` directly, not `xcodebuild`.** The desktop package is hand-rolled
/// precisely so no `.xcodeproj` has to be kept in step with it, and one target
/// does not justify bringing one back. The compiler is in the Command Line
/// Tools, which the packaging machine already needs for `codesign`, `ditto`,
/// `hdiutil` and `xcrun` — so this adds no new class of requirement, and it is
/// therefore a hard failure rather than a skip: a build that quietly shipped
/// without its share row would be discovered by a user, which is the packaging
/// omission [ADR-112] was written about.
///
/// Two things about the link line that are not guessable:
///
/// - **An app extension's entry point is `_NSExtensionMain`, not `main`.**
///   That is what Xcode's `LD_ENTRY_POINT` build setting sets for an extension
///   target, and without it the linker has no entry symbol at all. The symbol
///   comes from Foundation.
/// - **Nothing Swift is embedded.** The binary links `/usr/lib/swift`, the
///   OS-provided runtime, so the appex carries no `Frameworks/` directory —
///   which matters because `collect_files` refuses a symlink anywhere in the
///   staging tree and an embedded Swift runtime is a directory full of them.
///
/// The sources are **iOS's `ShareInbox.swift` compiled unmodified**, beside a
/// macOS-only view controller. One spelling of the manifest, the commit order,
/// the sweep and the provider rules across three products; see
/// `crates/koru-share-inbox`, whose tests pin the Rust reader against iOS's
/// own fixture.
///
/// [ADR-416]: ../../../docs/decisions/ADR-416-macshare.md
/// [ADR-112]: ../../../docs/decisions/ADR-101-125.md
#[cfg(target_os = "macos")]
fn mac_share_extension(
    root: &Path,
    stage: &Path,
    version: &str,
    build_number: u64,
    channel: &str,
) -> Result<(), String> {
    // The segment, not the channel name: it is what `koru_channel::segment()`
    // reports inside the binaries being packaged, and therefore which
    // `~/Library/Application Support/Koru/<segment>/` the app will drain.
    // Release builds are never the debug variant.
    let segment = channel
        .parse::<koru_update::ReleaseChannel>()
        .map(|channel| koru_channel::segment_of(channel, false))
        .map_err(|_| format!("unknown release channel: {channel}"))?;

    let sources = [
        // Deliberately reached across from the iOS tree rather than copied.
        // A copy is the drift this whole design exists to prevent, and the
        // Xcode project does exactly the same thing through a synchronized
        // group shared by two targets.
        root.join("apps/ios/KoruShared/ShareInbox.swift"),
        root.join("apps/macos/KoruShare/ShareViewController.swift"),
    ];
    for source in &sources {
        if !source.is_file() {
            return Err(format!(
                "the share extension's source is missing: {}",
                source.display()
            ));
        }
    }

    let appex = stage.join("Koru.app/Contents/PlugIns/KoruShare.appex/Contents");
    fs::create_dir_all(appex.join("MacOS")).map_err(|error| error.to_string())?;

    // One arch, matching the rest of the package. `macos12.0` is
    // `LSMinimumSystemVersion` in both plists; they are the same claim.
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    };
    let compiled = Command::new("swiftc")
        .args(["-O", "-swift-version", "5", "-module-name", "KoruShare"])
        .arg("-target")
        .arg(format!("{arch}-apple-macos12.0"))
        .arg("-o")
        .arg(appex.join("MacOS/KoruShare"))
        // The extension entry point; see this function's note.
        .args(["-Xlinker", "-e", "-Xlinker", "_NSExtensionMain"])
        .args(["-framework", "Foundation", "-framework", "AppKit"])
        .args(&sources)
        .status()
        .map_err(|error| {
            format!(
                "could not run swiftc for the share extension: {error} \
                 (the Command Line Tools carry it)"
            )
        })?;
    if !compiled.success() {
        return Err("swiftc failed to build KoruShare.appex".into());
    }

    let core = version
        .split(['-', '+'])
        .next()
        .unwrap_or(version)
        .to_string();
    let identifier = koru_share_inbox::share_extension_bundle_id(segment);
    let display = koru_share_inbox::share_extension_display_name(segment);
    // `NSExtensionActivationRule` mirrors `apps/ios/KoruShare/Info.plist`
    // — URLs, text, and up to twenty files, images or movies — so the two
    // sheets offer Koru for the same things.
    //
    // `KoruChannelSegment` is ours, and it is the whole reason five channels
    // can coexist: the extension reads it to know which channel root to stage
    // into. `CFBundlePackageType` is `XPC!` because an appex is one.
    //
    // The principal class is spelled without a module prefix because
    // `ShareViewController` carries `@objc(ShareViewController)`; Xcode's
    // `$(PRODUCT_MODULE_NAME).Class` form would tie this string to a compiler
    // flag two files away.
    fs::write(
        appex.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>KoruShare</string>
    <key>CFBundleDisplayName</key><string>{display}</string>
    <key>CFBundleIdentifier</key><string>{identifier}</string>
    <key>CFBundleExecutable</key><string>KoruShare</string>
    <key>CFBundlePackageType</key><string>XPC!</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleShortVersionString</key><string>{core}</string>
    <key>CFBundleVersion</key><string>{build_number}</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>KoruChannelSegment</key><string>{segment}</string>
    <key>NSExtension</key>
    <dict>
        <key>NSExtensionPointIdentifier</key><string>com.apple.share-services</string>
        <key>NSExtensionPrincipalClass</key><string>ShareViewController</string>
        <key>NSExtensionAttributes</key>
        <dict>
            <key>NSExtensionActivationRule</key>
            <dict>
                <key>NSExtensionActivationSupportsWebURLWithMaxCount</key><integer>1</integer>
                <key>NSExtensionActivationSupportsText</key><true/>
                <key>NSExtensionActivationSupportsFileWithMaxCount</key><integer>20</integer>
                <key>NSExtensionActivationSupportsImageWithMaxCount</key><integer>20</integer>
                <key>NSExtensionActivationSupportsMovieWithMaxCount</key><integer>20</integer>
            </dict>
        </dict>
    </dict>
</dict>
</plist>
"#
        ),
    )
    .map_err(|error| error.to_string())?;
    println!("[Koru] staged KoruShare.appex ({identifier})");
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(format!(
                "runtime asset tree contains a link: {}",
                entry.path().display()
            ));
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
fn collect_files(root: &Path) -> Result<Vec<String>, String> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_symlink() {
                return Err(format!(
                    "release staging contains a link: {}",
                    path.display()
                ));
            } else if kind.is_dir() {
                visit(root, &path, out)?;
            } else if kind.is_file() {
                out.push(
                    path.strip_prefix(root)
                        .map_err(|_| "release staging path escaped its root")?
                        .to_string_lossy()
                        .into_owned(),
                );
            } else {
                return Err(format!("unsupported release file type: {}", path.display()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

#[cfg(any(not(target_os = "macos"), test))]
fn zip_dir(root: &Path, archive: &Path) -> Result<(), String> {
    let output = File::create(archive).map_err(|error| error.to_string())?;
    let mut writer = zip::ZipWriter::new(output);
    for relative in collect_files(root)? {
        let mode = file_mode(&root.join(&relative))?;
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode);
        writer
            .start_file(relative.replace('\\', "/"), options)
            .map_err(|error| error.to_string())?;
        let mut source = File::open(root.join(&relative)).map_err(|error| error.to_string())?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            writer
                .write_all(&buffer[..read])
                .map_err(|error| error.to_string())?;
        }
    }
    writer.finish().map_err(|error| error.to_string())?;
    Ok(())
}

/// The mode an archive entry carries: the source file's own — a binary
/// extracted without its execute bit is a package that will not launch.
/// Windows has no mode to read, and 0o644 is what its entries always carried.
#[cfg(any(not(target_os = "macos"), test))]
fn file_mode(path: &Path) -> Result<u32, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(0o644)
    }
}

/// The `.desktop` file a Linux package carries beside `koru`, and its name:
/// `koru-nightly.desktop` for the nightly, `koru.desktop` for stable.
///
/// **`Exec` goes through `sh -c` for `$HOME`**, which a desktop entry does not
/// expand. The doubled backslashes are the file format's own escape over the
/// `Exec` key's, so the shell receives `exec "$HOME/koru-nightly/koru"`.
/// `Icon` cannot use a variable at all, hence the icon is copied by name.
/// `StartupWMClass=koru` is winit's X11 default, the binary's name, so the
/// dock groups Koru's windows under this entry.
fn linux_desktop_entry(channel: &str) -> (String, String) {
    let id = if channel == "stable" {
        "koru".to_owned()
    } else {
        format!("koru-{channel}")
    };
    let name = match channel {
        "stable" => "Koru".to_owned(),
        other => {
            let mut letters = other.chars();
            let first = letters.next().map(|c| c.to_ascii_uppercase());
            format!(
                "Koru {}{}",
                first.into_iter().collect::<String>(),
                letters.as_str()
            )
        }
    };
    let text = format!(
        "# Koru's launcher entry, for the app list and the dock.\n\
         # It assumes Koru is extracted to ~/{id}; if not, change {id} in Exec.\n\
         #\n\
         #   mkdir -p ~/.local/share/applications ~/.local/share/icons\n\
         #   cp ~/{id}/{id}.desktop ~/.local/share/applications/\n\
         #   cp ~/{id}/assets/logo.png ~/.local/share/icons/{id}.png\n\
         [Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec=sh -c \"exec \\\\\"\\\\$HOME/{id}/koru\\\\\"\"\n\
         Icon={id}\n\
         Terminal=false\n\
         Categories=Office;\n\
         StartupWMClass=koru\n"
    );
    (format!("{id}.desktop"), text)
}

/// The Linux package: **the zip's layout, in a gzipped tar.**
///
/// The layout is the contract with the extractor in `koru-update`, which
/// mirrors the zip extractor, so it is the zip's exactly: no top-level
/// directory, one regular-file entry per staged file, at the same
/// `/`-separated path relative to the package root and in the same order.
/// On Linux that is `koru`, `koru-agent`, `koru-mcp`, `koru-ai-chat` and
/// `koru-updater` at the root beside `release-files.json`, `VERSION`,
/// `LICENSE`, `NOTICE`, `THIRD-PARTY-NOTICES.txt` and `RELEASE_NOTES.md`, with
/// `assets/` and `docs/` beneath. No directory entries, links or devices: the
/// zip has none, and `collect_files` refuses links while staging.
///
/// Each entry keeps its file's mode, so the binaries stay executable, and its
/// modification time; owner and group are zero and unnamed, so nothing about
/// the machine that built it rides along.
#[cfg(any(not(target_os = "macos"), test))]
fn tar_gz_dir(root: &Path, archive: &Path) -> Result<(), String> {
    let output = File::create(archive).map_err(|error| error.to_string())?;
    let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for relative in collect_files(root)? {
        let path = root.join(&relative);
        let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(metadata.len());
        header.set_mode(file_mode(&path)? & 0o7777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs()),
        );
        let source = File::open(&path).map_err(|error| error.to_string())?;
        // `append_data` writes the path — as a GNU long-name entry when it is
        // over a hundred bytes — and then the checksum.
        builder
            .append_data(&mut header, relative.replace('\\', "/"), source)
            .map_err(|error| format!("{relative}: {error}"))?;
    }
    let encoder = builder.into_inner().map_err(|error| error.to_string())?;
    encoder
        .finish()
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn ensure_under(root: &Path, target: &Path) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", root.display()))?;
    let parent = target
        .parent()
        .ok_or("managed release path has no parent")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if parent == root && target.file_name().is_some() {
        Ok(())
    } else {
        Err("refusing to remove a path outside dist/cloud".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A community `dist-local` with nothing of ours says, per feature, what
    /// stays unconfigured; an official one has nothing to say.
    #[test]
    fn a_bundle_without_our_values_names_what_stays_unconfigured() {
        let nothing = unset_baked(|_| false);
        assert_eq!(nothing, koru_env::BAKED.to_vec());
        let lines = unconfigured_features(&nothing, true);
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert!(lines[0].starts_with("Koru Cloud sign-in is not configured"));
        assert!(lines[0].contains("KORU_PUBLIC_API_ISSUER"));
        assert!(lines[1].starts_with("Google Docs import is not set up in this build"));
        assert!(lines[2].starts_with("KORU_SUPPORT_URL unset"));
        assert!(lines[3].starts_with("KORU_FEEDBACK_URL unset"));
        assert_eq!(
            lines[4],
            "updates are off: the bundle carries no update key"
        );

        let everything = unset_baked(|_| true);
        assert!(everything.is_empty());
        assert!(unconfigured_features(&everything, false).is_empty());

        let partial =
            unconfigured_features(&[koru_env::name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT], false);
        assert_eq!(
            partial,
            ["Koru Cloud sign-in is partly configured: KORU_PUBLIC_API_CREDENTIAL_ROLLOUT unset"]
        );
    }

    #[test]
    fn windows_identity_manifest_binds_the_dedicated_com_broker() {
        let identity = koru_winres::notification_identity("beta", "CN=Koru & Company");
        let manifest = windows_identity_manifest(&identity, "1.2.3.42", "neutral");
        assert!(manifest.contains("Name=\"Koru.Desktop.Beta\""));
        assert!(manifest.contains("Publisher=\"CN=Koru &amp; Company\""));
        assert_eq!(
            manifest
                .matches("Executable=\"koru-com-notification.exe\"")
                .count(),
            3
        );
        assert_eq!(manifest.matches("Executable=\"koru-agent.exe\"").count(), 1);
        assert!(manifest.contains("Application Id=\"KoruAgent\""));
        assert!(manifest.contains("Application Id=\"KoruNotification\""));
        assert!(!manifest.contains("Arguments="));
        assert_eq!(
            manifest.matches(identity.toast_activator_clsid).count(),
            2,
            "toast and COM declarations must name the same class"
        );
        assert!(manifest.contains("<uap10:AllowExternalContent>true"));
        assert!(manifest.contains("ProcessorArchitecture=\"neutral\""));
        assert!(manifest.contains("uap10:RuntimeBehavior=\"win32App\""));
        assert!(manifest.contains("assets\\StoreLogo.png"));
        assert!(manifest.contains("assets\\Square44x44Logo.png"));
        assert!(manifest.contains("assets\\Square150x150Logo.png"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "explicit Windows SDK validation; writes only to a temporary directory"]
    fn windows_identity_package_passes_makeappx_validation() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let output = temporary.path().join("koru-identity.msix");
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let identity = koru_winres::notification_identity("beta", "CN=Koru");
        fs::write(
            temporary.path().join(koru_update::AGENT_EXE),
            b"manifest validation placeholder",
        )
        .unwrap();
        fs::write(
            temporary.path().join(koru_update::COM_NOTIFICATION_EXE),
            b"manifest validation placeholder",
        )
        .unwrap();
        stage_windows_identity_assets(root, temporary.path()).unwrap();
        stage_windows_identity_source(&source, &identity, "1.2.3.42", "neutral").unwrap();

        validate_windows_identity_layout(temporary.path(), &source).unwrap();
        makeappx_pack(&source, &output, true).unwrap();
        assert!(output.is_file());
    }

    #[test]
    fn windows_package_version_is_numeric_and_bounded() {
        assert_eq!(
            windows_package_version("1.2.3-beta.4", 42).as_deref(),
            Ok("1.2.3.42")
        );
        assert!(windows_package_version("1.2.3", u16::MAX as u64 + 1).is_err());
    }

    /// This half of the drift guard. The other half lives in
    /// `crates/koru-draw/src/text.rs`, where `BUNDLED_FAMILIES` is, and asserts
    /// that *it* names files on disk. The two halves cannot be one test because
    /// a release tool must not link `koru-draw` — that would put vello and
    /// parley in the build graph of a program that writes a ZIP.
    ///
    /// The count is what makes them meet: both lists name the same files under
    /// `assets/fonts/`, so if both are complete against that directory and both have
    /// thirty entries, neither can have quietly dropped one.
    #[test]
    fn fonts_are_the_whole_set() {
        assert_eq!(
            FONTS.len(),
            30,
            "koru-draw registers thirty faces; see BUNDLED_FAMILIES"
        );

        let mut seen = std::collections::HashSet::new();
        for font in FONTS {
            assert!(seen.insert(*font), "{font} is listed twice");
        }

        // The crate lives at crates/koru-release; assets/fonts/ is under the
        // workspace root, where the fetch scripts write it.
        let fonts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .join(FONTS_DIR);
        // Skipped on a tree that has not fetched them: a missing font is never
        // a build error anywhere else in this repo, and a fresh clone should
        // not fail its suite over one.
        if !fonts.join("Inter.ttf").is_file() {
            eprintln!("skipping the on-disk half: run koru.bat fonts");
            return;
        }
        for font in FONTS {
            assert!(
                fonts.join(font).is_file(),
                "{font} is packaged but not in {FONTS_DIR}/"
            );
        }
        validate_pinned_fonts(&fonts)
            .expect("the packaged terminal faces match scripts/setup/fonts.manifest");
    }

    /// **A path across two trees, checked here rather than at release time.**
    /// The macOS share extension compiles iOS's `ShareInbox.swift` directly —
    /// that is what stops the inbox format existing twice — so an iOS
    /// reorganisation that moves the file breaks mac packaging. Without this
    /// the discovery is `swiftc: no such file` during a publish; with it, it
    /// is a failing test in the run that moved it.
    #[test]
    fn the_share_extension_sources_are_where_the_packager_looks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        for source in [
            "apps/ios/KoruShared/ShareInbox.swift",
            "apps/macos/KoruShare/ShareViewController.swift",
        ] {
            assert!(
                root.join(source).is_file(),
                "{source} is compiled into KoruShare.appex"
            );
        }
    }

    /// The plug-in's identity comes from `koru-share-inbox`, which is also
    /// what the app reads to find a sandboxed extension's container. Two
    /// spellings would be an inbox nothing drains.
    #[test]
    fn the_extension_identity_is_not_spelled_twice() {
        assert_eq!(koru_share_inbox::DESKTOP_BUNDLE_ID, "do.koru.desktop");
        assert_eq!(
            koru_share_inbox::share_extension_bundle_id("beta"),
            "do.koru.desktop.share.beta"
        );
    }

    #[test]
    fn every_runtime_document_is_explicit_and_present() {
        assert_eq!(
            koru_update::RELEASE_DOCUMENTS,
            &["docs/mcp/README.md", "docs/prompts/delegated-agent.md"]
        );
        // The preamble's reader and the package agree on the path, in the
        // one place both can be named: the reader exports its constant, and
        // the publisher links it for exactly this assertion.
        assert!(
            koru_update::RELEASE_DOCUMENTS.contains(&koru_delegate_preamble::RELATIVE_PATH),
            "the preamble is staged where its reader looks"
        );
        // Every installed document has exactly one source, and nothing else
        // does.
        assert_eq!(
            RELEASE_DOCUMENT_SOURCES
                .iter()
                .map(|(installed, _)| *installed)
                .collect::<Vec<_>>(),
            koru_update::RELEASE_DOCUMENTS
        );
        assert_eq!(
            release_document_source(koru_delegate_preamble::RELATIVE_PATH).unwrap(),
            koru_delegate_preamble::SOURCE_PATH,
            "the preamble is staged from the file its crate compiles in"
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        for document in koru_update::RELEASE_DOCUMENTS {
            let source = release_document_source(document).unwrap();
            assert!(
                !source.starts_with("docs/"),
                "{source} must not depend on the docs/ checkout"
            );
            assert!(
                root.join(source).is_file(),
                "{source} is the source of the required runtime document {document}"
            );
        }
    }

    /// The license notices a package must carry are the repository's own
    /// `LICENSE` and `NOTICE` plus the generated third-party file — and the
    /// two the package copies exist, so a release cannot reach `build` without
    /// them.
    #[test]
    fn every_license_notice_is_explicit_and_present() {
        assert_eq!(
            koru_update::RELEASE_LEGAL,
            &["LICENSE", "NOTICE", crate::notices::FILE]
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        for file in ["LICENSE", "NOTICE"] {
            assert!(
                root.join(file).is_file(),
                "{file} is at the repository root"
            );
        }
        let license = fs::read_to_string(root.join("LICENSE")).unwrap();
        assert!(
            license.starts_with("Mozilla Public License Version 2.0"),
            "LICENSE is the MPL-2.0 text"
        );
    }

    /// **The packaged NOTICE names the exact source** (MPL-2.0 §3.2): the
    /// repository's own notice, unchanged, then the commit link — written
    /// into the package's data root, which is where `RELEASE_LEGAL` is read.
    #[test]
    fn the_packaged_notice_links_the_commit_it_was_built_from() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let data = tempfile::tempdir().unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";
        write_notice(root, data.path(), commit).unwrap();
        let packaged = fs::read_to_string(data.path().join("NOTICE")).unwrap();
        let repository = fs::read_to_string(root.join("NOTICE")).unwrap();
        assert!(
            packaged.starts_with(repository.trim_end()),
            "the repository's NOTICE comes first, unchanged"
        );
        assert!(
            packaged.contains(&format!("https://github.com/koruworks/mono/tree/{commit}")),
            "{packaged}"
        );
    }

    /// **The bundle asks for the microphone, in words somebody will read.**
    ///
    /// macOS *terminates* a process that touches an audio input device with no
    /// `NSMicrophoneUsageDescription` — not a refusal it can report, a kill —
    /// and the generator that writes this file only compiles on a Mac. So the
    /// text is checked here, on every platform, or the first person to find out
    /// it was missing is somebody whose Koru vanished when they held the
    /// dictate key.
    #[test]
    fn the_bundle_asks_for_the_microphone_in_words() {
        let plist = info_plist("1.2.3", "1.2.3-beta.4", 91);
        let key = "<key>NSMicrophoneUsageDescription</key>";
        assert!(plist.contains(key), "{plist}");
        let sentence = plist
            .split_once(key)
            .and_then(|(_, rest)| rest.split_once("<string>"))
            .and_then(|(_, rest)| rest.split_once("</string>"))
            .map(|(sentence, _)| sentence)
            .expect("a usage description");
        // Not a placeholder, and it says where the audio goes: this is the
        // sentence the system consent dialog puts in front of somebody.
        assert!(sentence.len() > 30, "{sentence:?} is not an explanation");
        assert!(sentence.contains("microphone"), "{sentence:?}");
        assert!(sentence.contains("never leaves"), "{sentence:?}");
    }

    /// Everything else the bundle declared before is still declared. A plist
    /// this crate assembles by hand is one string away from losing a key
    /// nothing else checks.
    #[test]
    fn the_bundle_keeps_its_identity_and_its_link_scheme() {
        let plist = info_plist("1.2.3", "1.2.3-beta.4", 91);
        for needle in [
            "<key>CFBundleIdentifier</key><string>do.koru.desktop</string>",
            "<key>CFBundleExecutable</key><string>koru</string>",
            "<key>CFBundleShortVersionString</key><string>1.2.3</string>",
            "<key>CFBundleVersion</key><string>91</string>",
            "<key>CFBundleGetInfoString</key><string>Koru 1.2.3-beta.4</string>",
            "<array><string>koru</string></array>",
        ] {
            assert!(plist.contains(needle), "{needle} is missing from {plist}");
        }
    }

    #[test]
    fn the_notification_owner_is_a_separate_background_app() {
        let plist = notification_owner_info_plist("1.2.3", "1.2.3-beta.4", 91);
        for needle in [
            "<key>CFBundleIdentifier</key><string>do.koru.desktop.agent.notifications</string>",
            "<key>CFBundleExecutable</key><string>koru-notification-owner</string>",
            "<key>CFBundleShortVersionString</key><string>1.2.3</string>",
            "<key>CFBundleVersion</key><string>91</string>",
            "<key>LSUIElement</key><true/>",
        ] {
            assert!(plist.contains(needle), "{needle} is missing from {plist}");
        }
        let bundle = notification_owner_bundle(Path::new("stage/Koru.app"));
        assert_eq!(
            bundle,
            Path::new("stage/Koru.app/Contents/Library/LoginItems/Koru Agent.app")
        );
        assert_eq!(
            bundle
                .join("Contents/Info.plist")
                .to_string_lossy()
                .replace('\\', "/"),
            format!("stage/{}", koru_update::NOTIFICATION_OWNER_INFO_PLIST)
        );
        assert_eq!(
            bundle
                .join("Contents/MacOS")
                .join(koru_native_notifications::MAC_OWNER_EXECUTABLE)
                .to_string_lossy()
                .replace('\\', "/"),
            format!("stage/{}", koru_update::NOTIFICATION_OWNER_EXECUTABLE)
        );
        assert_eq!(MAC_NOTIFICATION_OWNER_PACKAGE, "koru-notification-owner");
    }

    #[test]
    fn every_notification_owner_tone_has_one_nested_resource_path() {
        let bundle = notification_owner_bundle(Path::new("stage/Koru.app"));
        let paths = koru_native_notifications::BUNDLED_TONES
            .iter()
            .map(|asset| {
                bundle
                    .join("Contents/Resources/assets/reminders")
                    .join(asset.file)
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(paths.len(), 10);
        assert!(paths.iter().all(|path| path.starts_with(&bundle)));
        let inventory = koru_update::MAC_NOTIFICATION_OWNER_FILES
            .iter()
            .map(|path| Path::new(path))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(inventory.len(), 12);
        assert!(paths.iter().all(|path| {
            let path = path.to_string_lossy().replace('\\', "/");
            inventory.contains(Path::new(path.strip_prefix("stage/").unwrap()))
        }));
    }

    #[test]
    fn notification_owner_signing_does_not_inherit_the_microphone_entitlement() {
        let owner = mac_codesign_args(Path::new("Koru Agent.app"), None);
        let outer = mac_codesign_args(
            Path::new("Koru.app"),
            Some(Path::new("koru-entitlements.plist")),
        );
        let text = |args: Vec<OsString>| {
            args.into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let owner = text(owner);
        let outer = text(outer);
        assert!(
            owner
                .windows(2)
                .any(|pair| pair == ["--options", "runtime"])
        );
        assert!(!owner.iter().any(|arg| arg == "--entitlements"));
        assert!(outer.iter().any(|arg| arg == "--entitlements"));
        assert!(outer.iter().any(|arg| arg == "koru-entitlements.plist"));
    }

    /// **The entitlement and the usage description are two halves.** The plist
    /// is what the consent dialog says; this is what lets the request be made
    /// at all under the hardened runtime, which notarization requires.
    #[test]
    fn the_hardened_runtime_is_allowed_audio_input() {
        assert!(
            HARDENED_ENTITLEMENTS
                .contains("<key>com.apple.security.device.audio-input</key><true/>"),
            "{HARDENED_ENTITLEMENTS}"
        );
        assert!(HARDENED_ENTITLEMENTS.starts_with("<?xml"));
        assert!(HARDENED_ENTITLEMENTS.trim_end().ends_with("</plist>"));
    }

    /// **Signing input is not shipped content.** The entitlements file is
    /// written beside the staging directory, because everything inside it is
    /// walked into `release-files.json` — and a name in that list that no
    /// published package carries invalidates every one of them.
    #[test]
    fn the_entitlements_file_is_not_a_package_file() {
        let stage = Path::new("C:/build/dist/koru-1.2.3");
        let written = entitlements_path(stage);
        assert!(
            !written.starts_with(stage),
            "{} would be walked into release-files.json",
            written.display()
        );
        assert_eq!(
            written.file_name().and_then(|n| n.to_str()),
            Some(ENTITLEMENTS_FILE)
        );
    }

    /// The executable catalogue is the strict package contract. This table is
    /// the build/sign/stage mechanism, so equality keeps either side from
    /// silently gaining a binary the other side forgot.
    #[test]
    fn every_catalogued_executable_is_built_and_staged() {
        let mut packaged = CHANNEL_BINARIES
            .iter()
            .map(|(_, executable)| *executable)
            .chain(std::iter::once(koru_update::UPDATER_EXE))
            .collect::<Vec<_>>();
        packaged.sort_unstable();

        let mut catalogued = koru_update::ALL_EXE.to_vec();
        catalogued.sort_unstable();

        assert_eq!(packaged, catalogued);
        assert!(
            CHANNEL_BINARIES.contains(&("koru-ai-chat", koru_update::AI_CHAT_EXE)),
            "the standalone chat package and executable must stay paired"
        );
        // `crates/koru-voice` holds the contract, so the worker package and
        // binary both use the `-worker` suffix. Getting either half wrong
        // builds nothing or stages nothing, and both fail long after this
        // table was edited.
        if cfg!(target_os = "linux") {
            assert!(
                !CHANNEL_BINARIES
                    .iter()
                    .any(|(package, executable)| *package == "koru-voice-worker"
                        || *executable == koru_update::VOICE_EXE),
                "Linux builds no voice worker until rung 3"
            );
        } else {
            assert!(
                CHANNEL_BINARIES.contains(&("koru-voice-worker", koru_update::VOICE_EXE)),
                "the voice worker package and its shipped executable must stay paired"
            );
        }
        // **Every retired executable, by constant rather than by literal.**
        // Each of these is a name `koru_update::exe` still carries so the
        // updater can stop the process and move the file aside; none of them
        // may be built, signed or staged into a package again. The gateway and
        // the session host are the two Plan 7 retired, and they are the ones a
        // careless re-add would look most reasonable.
        for retired in [
            koru_update::GATEWAY_EXE,
            koru_update::SESSION_HOST_EXE,
            koru_update::TERMINAL_EXE,
            koru_update::LEGACY_STORE_EXE,
            koru_update::LEGACY_STORE_SERVER_EXE,
            koru_update::LEGACY_VOICE_EXE,
        ] {
            assert!(
                !packaged.contains(&retired),
                "the release must not build or stage the retired {retired}"
            );
        }
        assert!(
            !CHANNEL_BINARIES
                .iter()
                .any(|(package, _)| *package == "koru-api-gateway"
                    || *package == "koru-session-host"),
            "the gateway and the sessions host are roles of koru-agent, not packages a \
             release builds"
        );
        if cfg!(windows) {
            assert!(
                CHANNEL_BINARIES
                    .contains(&("koru-com-notification", koru_update::COM_NOTIFICATION_EXE))
            );
        }
        assert_eq!(
            MAC_NOTIFICATION_OWNER_PACKAGE,
            koru_native_notifications::MAC_OWNER_EXECUTABLE
        );
    }

    #[test]
    fn each_target_has_its_own_archive_and_manifest_format() {
        use koru_update::PackageFormat;
        for (target, extension, format) in [
            ("windows-x86_64", "zip", PackageFormat::Zip),
            ("macos-aarch64", "dmg", PackageFormat::Dmg),
            ("macos-x86_64", "dmg", PackageFormat::Dmg),
            ("linux-x86_64", "tar.gz", PackageFormat::TarGz),
        ] {
            assert_eq!(archive_extension(target), extension, "{target}");
            assert_eq!(package_format(target), format, "{target}");
        }
        assert_eq!(
            archive_path(Path::new("/r"), "0.1.4", "linux-x86_64"),
            Path::new("/r/dist/cloud/koru-0.1.4-linux-x86_64.tar.gz")
        );
    }

    /// The launcher entry names the channel's folder, and its `Exec` survives
    /// both escape layers as one `sh -c` argument with `$HOME` intact.
    #[test]
    fn linux_desktop_entry_names_the_channel_and_escapes_exec() {
        let (file, text) = linux_desktop_entry("nightly");
        assert_eq!(file, "koru-nightly.desktop");
        assert!(text.contains("\nName=Koru Nightly\n"));
        assert!(text.contains("\nIcon=koru-nightly\n"));
        assert!(text.contains(r#"Exec=sh -c "exec \\"\\$HOME/koru-nightly/koru\\"""#));
        assert!(text.contains("\nStartupWMClass=koru\n"));
        let (file, text) = linux_desktop_entry("stable");
        assert_eq!(file, "koru.desktop");
        assert!(text.contains("\nName=Koru\n"));
        assert!(text.contains("$HOME/koru/koru"));
    }

    /// The Linux arm is the Windows/mac inventory minus the voice worker and
    /// the Windows-only COM broker — no more, no less.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_packages_everything_but_the_voice_worker() {
        assert_eq!(
            CHANNEL_BINARIES,
            &[
                ("koru", koru_update::APP_EXE),
                ("koru-agent", koru_update::AGENT_EXE),
                ("koru-mcp", koru_update::MCP_EXE),
                ("koru-ai-chat", koru_update::AI_CHAT_EXE),
            ]
        );
        assert_eq!(crate::host_target(), "linux-x86_64");
    }

    /// **The tarball's layout is the zip's**, which is the contract with the
    /// extractor in `koru-update`: the same entries, at the same relative
    /// paths, in the same order, with no top-level folder — and on Unix the
    /// same modes, so a binary is still executable after extraction.
    #[test]
    fn the_tarball_has_exactly_the_zips_layout() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        let long = format!("docs/{}/deep.md", "nested".repeat(20));
        for (path, contents) in [
            ("koru", "binary"),
            ("release-files.json", "{}"),
            ("RELEASE_NOTES.md", "notes"),
            ("assets/fonts/Inter.ttf", "font"),
            ("docs/mcp/README.md", "doc"),
            (long.as_str(), "a path over a hundred bytes"),
        ] {
            let file = stage.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, contents).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(stage.join("koru"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(
                stage.join("RELEASE_NOTES.md"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        let zip_path = dir.path().join("koru.zip");
        let tar_path = dir.path().join("koru.tar.gz");
        zip_dir(&stage, &zip_path).unwrap();
        tar_gz_dir(&stage, &tar_path).unwrap();

        let mut zip = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let zipped: Vec<(String, Option<u32>)> = (0..zip.len())
            .map(|index| {
                let entry = zip.by_index(index).unwrap();
                (entry.name().unwrap().to_string(), entry.unix_mode())
            })
            .collect();
        let mut tar =
            tar::Archive::new(flate2::read::GzDecoder::new(File::open(&tar_path).unwrap()));
        let mut tarred = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            assert_eq!(entry.header().entry_type(), tar::EntryType::Regular);
            assert_eq!(entry.header().uid().unwrap(), 0);
            let mut contents = String::new();
            entry.read_to_string(&mut contents).unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            assert_eq!(
                contents,
                fs::read_to_string(stage.join(&path)).unwrap(),
                "{path}"
            );
            tarred.push((path, entry.header().mode().unwrap()));
        }

        let zip_paths: Vec<&str> = zipped.iter().map(|(path, _)| path.as_str()).collect();
        let tar_paths: Vec<&str> = tarred.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(tar_paths, zip_paths);
        assert_eq!(tar_paths.len(), 6);
        assert!(tar_paths.contains(&long.as_str()));
        assert!(
            tar_paths
                .iter()
                .all(|path| !path.starts_with('/') && !path.contains('\\'))
        );
        #[cfg(unix)]
        for ((path, zip_mode), (_, tar_mode)) in zipped.iter().zip(&tarred) {
            assert_eq!(
                zip_mode.map(|mode| mode & 0o7777),
                Some(*tar_mode),
                "{path}"
            );
            if path == "koru" {
                assert_eq!(*tar_mode, 0o755);
            }
        }
    }
}
