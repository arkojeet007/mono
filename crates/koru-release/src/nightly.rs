// SPDX-License-Identifier: MPL-2.0

//! **The GitHub nightly**: one pre-release on the moving `nightly` tag of
//! koruworks/mono, rebuilt and replaced each time the maintainer moves the tag
//! (`docs/features/linux/NIGHTLY.rev1.md`).
//!
//! Three verbs, and none of them talks to GitHub — the workflow uploads:
//!
//! - `dist-github nightly` builds, signs and writes everything one build
//!   publishes into a directory: the package, its `.minisig`, the feed
//!   manifest and the release body. Non-interactive, for CI.
//! - `nightly-notes` drafts the next `releases/nightly/<yyyy-mm-dd>.toml` with
//!   the release-notes generator, for the maintainer to edit and commit.
//! - `nightly-notes --select <before> <after>` names the one notes file a tag
//!   push added, for CI to hand to `dist-github`.
//!
//! **The notes are an archive, never edited after publication**: the release
//! page shows only the newest build, and koru.do reads the folder for the
//! history (NIGHTLY §9).

use crate::{
    Descriptor, git, git_allow_failure, host_target, newest_across_targets, notes, package,
    remove_local_artifact_set, require_clean_tree, sign_archive, source, split_target,
    validate_release_version, version,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use koru_update::{ReleaseChannel, ReleaseNotes, UpdateManifest, UpdatePackage, UpdateTarget};
use minisign::{PublicKey, SecretKeyBox, SignatureBox};
use pulldown_cmark::{Event, Parser, Tag};
use std::fs::{self, File};
use std::io::{self, BufRead, IsTerminal};
use std::path::{Path, PathBuf};

/// The tag the nightly release lives on, and the release's name.
const TAG: &str = "nightly";

/// Where the notes files live, relative to the workspace root.
const NOTES_DIR: &str = "releases/nightly";

/// How many commits a first draft describes when there is no `nightly` tag to
/// start from.
const FALLBACK_COMMITS: usize = 50;

pub(crate) const DIST_GITHUB_USAGE: &str = "usage: koru-release dist-github nightly \
     --build <yyyymmddHHMM> --notes <releases/nightly/<file>.toml> --key <minisign secret key> \
     [--key-password-stdin] --download-base <https url> --out <dir> [--version <version>]";

pub(crate) const NIGHTLY_NOTES_USAGE: &str = "usage: koru-release nightly-notes\n       \
     koru-release nightly-notes --select <before-sha> <after-sha>";

/// What a person is told before downloading a Linux nightly (NIGHTLY §1).
/// Per platform, because Windows and macOS join the same page later and their
/// limits are not Linux's.
fn preview_note(platform: &str) -> Option<&'static str> {
    (platform == "linux").then_some(
        "**A preview** — X11 only (XWayland on Wayland desktops), no dictation yet, \
         no Koru Cloud yet.",
    )
}

// ------------------------------------------------------------ dist-github

/// What `dist-github` was asked to build.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DistGithubOptions {
    pub build_number: u64,
    /// The instant the build number names. It is also the release's
    /// `published_at` and the date in its version: one reading of the clock
    /// for the whole build, as `publish` does.
    pub released: DateTime<Utc>,
    pub notes: PathBuf,
    pub key: PathBuf,
    pub key_password_stdin: bool,
    /// Always ends in `/`, so a file name is appended to it.
    pub download_base: String,
    pub out: PathBuf,
    pub version: Option<String>,
}

impl DistGithubOptions {
    /// Strict, as every verb here is: an argument that is not understood is
    /// not run, and a flag given twice is not resolved by guessing.
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut build = None;
        let mut notes = None;
        let mut key = None;
        let mut key_password_stdin = false;
        let mut download_base = None;
        let mut out = None;
        let mut version = None;
        while let Some(arg) = args.next() {
            let slot = match arg.as_str() {
                "--key-password-stdin" => {
                    key_password_stdin = true;
                    continue;
                }
                "--build" => &mut build,
                "--notes" => &mut notes,
                "--key" => &mut key,
                "--download-base" => &mut download_base,
                "--out" => &mut out,
                "--version" => &mut version,
                _ => return Err(format!("unexpected argument: {arg}\n{DIST_GITHUB_USAGE}")),
            };
            let value = args
                .next()
                .ok_or_else(|| format!("{arg} needs a value\n{DIST_GITHUB_USAGE}"))?;
            if slot.replace(value).is_some() {
                return Err(format!("{arg} given twice\n{DIST_GITHUB_USAGE}"));
            }
        }
        let required = |value: Option<String>, flag: &str| {
            value.ok_or_else(|| format!("{flag} is required\n{DIST_GITHUB_USAGE}"))
        };
        let (build_number, released) = parse_build_time(&required(build, "--build")?)?;
        Ok(Self {
            build_number,
            released,
            notes: required(notes, "--notes")?.into(),
            key: required(key, "--key")?.into(),
            key_password_stdin,
            download_base: download_base_url(&required(download_base, "--download-base")?)?,
            out: required(out, "--out")?.into(),
            version,
        })
    }
}

/// **The build number is the UTC time of the build, `yyyymmddHHMM`** (N11):
/// always increasing, no state, and above every number the update origin has
/// handed out. Parsed as a time rather than a number, so a typo is refused
/// instead of becoming a build no later nightly can ever be newer than.
pub(crate) fn parse_build_time(value: &str) -> Result<(u64, DateTime<Utc>), String> {
    let refuse = || format!("--build must be the UTC build time as yyyymmddHHMM, not {value:?}");
    if value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(refuse());
    }
    let time = NaiveDateTime::parse_from_str(value, "%Y%m%d%H%M").map_err(|_| refuse())?;
    Ok((value.parse().map_err(|_| refuse())?, time.and_utc()))
}

/// The directory every file of the release is downloaded from, ending in `/`.
/// HTTPS only, because `UpdateManifest::validate` refuses anything else and a
/// manifest is better refused before the ten-minute build than after it.
pub(crate) fn download_base_url(value: &str) -> Result<String, String> {
    let url = url::Url::parse(value).map_err(|error| format!("--download-base: {error}"))?;
    if url.scheme() != "https" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "--download-base must be a plain https directory URL, not {value}"
        ));
    }
    let mut base = url.to_string();
    if !base.ends_with('/') {
        base.push('/');
    }
    Ok(base)
}

/// The name a file keeps once it is a release asset.
///
/// **GitHub renames an asset whose name holds anything outside letters,
/// digits, `.`, `-` and `_`**, replacing each such character with `.` — and a
/// nightly version always holds a `+` (`0.1.4-nightly.20261024+abc1234`). The
/// manifest has to name the file GitHub serves, so the name is made safe here
/// by GitHub's own rule rather than discovered as a 404 by every installed
/// nightly.
pub(crate) fn github_asset_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '.'
            }
        })
        .collect()
}

/// The feed manifest's asset name: `<channel>-<platform>-<arch>.json`
/// (NIGHTLY §6.1) — asked of `koru_update::FeedLayout`, the function the
/// installed app uses to find it, so the two cannot spell it differently.
pub(crate) fn feed_name(channel: ReleaseChannel, target: &str) -> String {
    let (platform, architecture) = split_target(target);
    koru_update::FeedLayout::GitHubRelease.manifest_path(&koru_update::UpdateTarget {
        channel,
        platform,
        architecture,
    })
}

/// The release page on GitHub, which the update banner links to.
pub(crate) fn release_page(channel: ReleaseChannel) -> String {
    format!("{}/releases/tag/{channel}", source::REPOSITORY)
}

/// Everything one GitHub build publishes about its package.
pub(crate) struct GithubBuild<'a> {
    pub channel: ReleaseChannel,
    pub target: &'a str,
    pub version: &'a str,
    pub build_number: u64,
    pub released_at: &'a str,
    /// The full commit id.
    pub commit: &'a str,
    pub notes: &'a Descriptor,
    pub download_base: &'a str,
    pub asset: &'a str,
    pub size: u64,
    pub sha256: &'a str,
    pub signature: &'a str,
}

/// The feed manifest for a GitHub release: today's schema, unchanged, with
/// every URL on GitHub (NIGHTLY §6.1).
pub(crate) fn github_manifest(build: &GithubBuild<'_>) -> UpdateManifest {
    let (platform, architecture) = split_target(build.target);
    UpdateManifest {
        schema_version: 1,
        channel: build.channel,
        platform,
        architecture,
        version: build.version.into(),
        build_number: build.build_number,
        published_at: build.released_at.into(),
        commit: Some(build.commit.chars().take(7).collect()),
        source: source::url(build.commit),
        minimum_updater_version: Some(1),
        minimum_supported_build: None,
        required: false,
        release_notes: ReleaseNotes {
            title: build.notes.title.clone(),
            summary: build.notes.summary.clone(),
            url: Some(release_page(build.channel)),
        },
        package: UpdatePackage {
            url: format!("{}{}", build.download_base, build.asset),
            file_name: build.asset.into(),
            size: build.size,
            sha256: build.sha256.into(),
            signature: build.signature.into(),
            format: package::package_format(build.target),
        },
    }
}

/// The GitHub release's text: the notes in full, then what this build is.
///
/// Markdown, because GitHub renders a release body as Markdown — the same
/// Markdown the notes file holds and the package carries as
/// `RELEASE_NOTES.md`, so the three cannot say different things.
pub(crate) fn release_body(build: &GithubBuild<'_>) -> String {
    let notes = build.notes;
    let mut body = format!("## {}\n\n", notes.title.trim());
    for line in &notes.summary {
        body.push_str(&format!("- {}\n", line.trim()));
    }
    body.push_str(&format!("\n{}\n\n---\n\n", notes.body.trim()));
    let short: String = build.commit.chars().take(7).collect();
    body.push_str(&format!(
        "Koru `{version}` · build {build_number} · built from \
         [`{short}`]({repository}/commit/{commit})\n",
        version = build.version,
        build_number = build.build_number,
        repository = source::REPOSITORY,
        commit = build.commit,
    ));
    let (platform, _) = split_target(build.target);
    if let Some(note) = preview_note(&platform) {
        body.push_str(&format!("\n{note}\n"));
    }
    body
}

/// Build, sign and write one GitHub release's files into `--out`.
///
/// Nothing here is uploaded and nothing asks a question: the workflow uploads
/// what this writes, in the order NIGHTLY §4 gives (package and signature
/// first, the feed manifest after them).
pub(crate) fn dist_github(
    root: &Path,
    channel: ReleaseChannel,
    options: DistGithubOptions,
) -> Result<(), String> {
    if channel != ReleaseChannel::Nightly {
        return Err(format!(
            "only the nightly is published to GitHub so far, not {channel}"
        ));
    }
    let target = host_target();
    if target == "unsupported" {
        return Err("dist-github does not know this host's target triple".into());
    }
    // Everything that can be refused is refused before the build: the notes,
    // the key, the tree, the version.
    let descriptor = read_notes(&options.notes)?;
    let (secret, public_key) = open_key_file(&options.key, options.key_password_stdin)?;
    let commit = require_clean_tree(root, None)?;
    if source::url(&commit).is_none() {
        return Err(format!(
            "a published package must name its source commit, and {commit:?} is not a full commit id"
        ));
    }
    let short_commit: String = commit.chars().take(7).collect();
    let version = match options.version {
        Some(version) => version,
        None => infer_version(channel, &options.released, &short_commit)?,
    };
    validate_release_version(channel, &version)?;
    let released_at = options.released.to_rfc3339_opts(SecondsFormat::Secs, true);
    println!(
        "[Koru] {channel} {version}, build {}, for {target}",
        options.build_number
    );
    let asset = github_asset_name(&format!(
        "koru-{version}-{target}.{}",
        package::archive_extension(target)
    ));
    let mut build = GithubBuild {
        channel,
        target,
        version: &version,
        build_number: options.build_number,
        released_at: &released_at,
        commit: &commit,
        notes: &descriptor,
        download_base: &options.download_base,
        asset: &asset,
        // Placeholders until the package exists, so everything else the
        // client checks — the URLs, the format for this platform, the notes —
        // is checked before the build rather than after it.
        size: 1,
        sha256: PLACEHOLDER_SHA256,
        signature: "placeholder",
    };
    client_accepts(&github_manifest(&build))?;

    // `dist/cloud` is only where `package::build` stages; what this verb
    // produces is what it copies into `--out`. A second run of the same commit
    // on the same day names the same archive, so the last one is replaced.
    remove_local_artifact_set(&package::archive_path(root, &version, target))?;
    let package = package::build(
        root,
        &version,
        options.build_number,
        channel.as_str(),
        &commit,
        &released_at,
        &public_key,
        Some(descriptor.body.as_str()),
        // `Official` requires .env.production and the canonical sherpa-onnx
        // archive; this build has neither by design (no Cloud, no voice).
        crate::native::Rule::Local,
        package::Baked::WithoutCloud {
            update_base_url: &options.download_base,
        },
    )?;
    require_clean_tree(root, Some(&commit))?;
    let signature = sign_archive(&secret, &package.archive, &public_key, &version)?;
    verify_signature(&package.archive, &signature, &public_key)?;

    build.size = package.size;
    build.sha256 = &package.sha256;
    build.signature = &signature;
    let manifest = github_manifest(&build);
    client_accepts(&manifest)?;

    fs::create_dir_all(&options.out)
        .map_err(|error| format!("{}: {error}", options.out.display()))?;
    let signature_name = format!("{asset}.minisig");
    let feed = feed_name(channel, target);
    let body_name = "release-body.md";
    fs::copy(&package.archive, options.out.join(&asset))
        .map_err(|error| format!("{}: {error}", options.out.join(&asset).display()))?;
    write(&options.out.join(&signature_name), signature.as_bytes())?;
    write(
        &options.out.join(&feed),
        &serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
    )?;
    write(
        &options.out.join(body_name),
        release_body(&build).as_bytes(),
    )?;

    println!("[Koru] sha256: {}", package.sha256);
    println!("[Koru] wrote into {}:", options.out.display());
    for name in [asset.as_str(), &signature_name, &feed, body_name] {
        println!("{name}");
    }
    Ok(())
}

const PLACEHOLDER_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The installed app's own check of a manifest: one it would refuse must not
/// reach the release.
fn client_accepts(manifest: &UpdateManifest) -> Result<(), String> {
    manifest
        .validate(&UpdateTarget {
            channel: manifest.channel,
            platform: manifest.platform.clone(),
            architecture: manifest.architecture.clone(),
        })
        .map_err(|error| format!("the feed manifest would be refused: {error}"))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The version the release design's nightly rule gives this commit:
/// `<next>-nightly.<yyyymmdd>+<sha7>`, where `<next>` follows the stable and
/// beta lines the update origin serves. Read from the public manifests, which
/// need no credentials; a failure is an error, never a guessed version.
fn infer_version(
    channel: ReleaseChannel,
    released: &DateTime<Utc>,
    short_commit: &str,
) -> Result<String, String> {
    let cloud = crate::cloud::Cloud::public_reader()?;
    let read = |channel| {
        newest_across_targets(&cloud, channel).map_err(|error| {
            format!(
                "could not read the published {channel} manifests to choose the version \
                 ({error}); pass --version to name it"
            )
        })
    };
    version::suggest_version(
        channel,
        read(ReleaseChannel::Stable)?.as_ref(),
        read(ReleaseChannel::Beta)?.as_ref(),
        read(ReleaseChannel::Nightly)?.as_ref(),
        &released.format("%Y%m%d").to_string(),
        short_commit,
    )
}

/// The release key from a file CI provides, and its public half.
///
/// **The public half is derived, not supplied**, so the key a package is
/// signed with and the key it verifies its next update against cannot be two
/// different keys. An encrypted key — which is how `ensure_keypair` writes
/// one — needs its password, and this verb never prompts; it reads one line
/// from standard input under `--key-password-stdin`.
fn open_key_file(
    path: &Path,
    password_on_stdin: bool,
) -> Result<(minisign::SecretKey, String), String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("--key {}: {error}", path.display()))?;
    let boxed = || {
        SecretKeyBox::from_string(&text).map_err(|error| {
            format!(
                "--key {} is not a Minisign secret key: {error}",
                path.display()
            )
        })
    };
    let secret = match boxed()?.into_unencrypted_secret_key() {
        Ok(secret) => secret,
        Err(_) if password_on_stdin => {
            if io::stdin().is_terminal() {
                return Err(
                    "--key-password-stdin reads the password from a pipe, not a terminal".into(),
                );
            }
            let mut password = String::new();
            io::stdin()
                .lock()
                .read_line(&mut password)
                .map_err(|error| format!("reading the key password: {error}"))?;
            boxed()?
                .into_secret_key(Some(password.trim_end_matches(['\r', '\n']).to_string()))
                .map_err(|error| format!("--key {}: {error}", path.display()))?
        }
        Err(_) => {
            return Err(format!(
                "--key {} is encrypted; pass --key-password-stdin and pipe its password in",
                path.display()
            ));
        }
    };
    let public = PublicKey::from_secret_key(&secret)
        .and_then(|public| public.to_box())
        .map_err(|error| error.to_string())?
        .to_string();
    Ok((secret, public))
}

/// The signature checked against the public key the build baked in, before
/// it is written anywhere: the same check every installed Koru will make.
fn verify_signature(archive: &Path, signature: &str, public_key: &str) -> Result<(), String> {
    let public = minisign::PublicKeyBox::from_string(public_key)
        .and_then(|boxed| boxed.into_public_key())
        .map_err(|error| error.to_string())?;
    let signature = SignatureBox::from_string(signature).map_err(|error| error.to_string())?;
    let file = File::open(archive).map_err(|error| error.to_string())?;
    minisign::verify(&public, &signature, file, true, false, false)
        .map_err(|error| format!("the package signature does not verify: {error}"))
}

// ------------------------------------------------------------------ notes

/// A nightly notes file, parsed and checked: the stable descriptor's three
/// fields, none blank, and text only.
pub(crate) fn read_notes(path: &Path) -> Result<Descriptor, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("nightly notes {}: {error}", path.display()))?;
    parse_notes(&text).map_err(|error| format!("{} is invalid: {error}", path.display()))
}

pub(crate) fn parse_notes(text: &str) -> Result<Descriptor, String> {
    let descriptor: Descriptor = toml::from_str(text).map_err(|error| error.to_string())?;
    descriptor.validate()?;
    text_only(&descriptor.body)?;
    Ok(descriptor)
}

/// **Nightly notes are text** (NIGHTLY §9): no image, GIF, WebP or video. The
/// body is shown on GitHub, in the app's *What's new* tab and on koru.do, and
/// nothing is published beside it to point at. Raw HTML is refused with them,
/// because `<img>` and `<video>` are how media would get in otherwise.
pub(crate) fn text_only(body: &str) -> Result<(), String> {
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Image { .. }) => {
                return Err("nightly notes are text only: remove the image".into());
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                return Err(format!(
                    "nightly notes are text only: remove the HTML {:?}",
                    html.trim()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// `releases/nightly/2026-10-24-2.toml` → (2026-10-24, 2). The first build of
/// a day has no suffix and is number 1; anything else in the folder — its
/// README — is not a notes file.
pub(crate) fn notes_key(path: &str) -> Option<(NaiveDate, u32)> {
    let name = path.strip_prefix(NOTES_DIR)?.strip_prefix('/')?;
    let stem = name.strip_suffix(".toml")?;
    if stem.len() < 10 || !stem.is_char_boundary(10) {
        return None;
    }
    let (date, rest) = stem.split_at(10);
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let number = match rest {
        "" => 1,
        suffix => {
            let digits = suffix.strip_prefix('-')?;
            if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let number: u32 = digits.parse().ok()?;
            if number < 2 {
                return None;
            }
            number
        }
    };
    Some((date, number))
}

/// The next notes file for `date`: `<date>.toml`, then `-2`, `-3`… for a
/// second and third build that day. `existing` is the folder's paths.
pub(crate) fn next_notes_path(date: NaiveDate, existing: &[String]) -> String {
    let taken = existing
        .iter()
        .filter_map(|path| notes_key(path))
        .filter(|(day, _)| *day == date)
        .map(|(_, number)| number)
        .max();
    let day = date.format("%Y-%m-%d");
    match taken {
        None => format!("{NOTES_DIR}/{day}.toml"),
        Some(number) => format!("{NOTES_DIR}/{day}-{}.toml", number + 1),
    }
}

/// **The one notes file a tag push added** (NIGHTLY §9 step 3).
///
/// `added` is what `git diff --diff-filter=A <before> <after>` lists; on the
/// tag's first push (`before` all zeros) there is nothing to diff against,
/// and the newest file of `present` — the folder at `<after>` — is taken.
/// None, or more than one, is a refusal: CI fails before it builds rather
/// than guess which notes a build is.
pub(crate) fn select_notes(
    first_push: bool,
    added: &[String],
    present: &[String],
) -> Result<String, String> {
    if first_push {
        return present
            .iter()
            .filter_map(|path| notes_key(path).map(|key| (key, path)))
            .max()
            .map(|(_, path)| path.clone())
            .ok_or_else(|| {
                format!("the first nightly needs a notes file in {NOTES_DIR}/, and there is none")
            });
    }
    let notes: Vec<&String> = added
        .iter()
        .filter(|path| notes_key(path).is_some())
        .collect();
    match notes.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!(
            "no notes file was added to {NOTES_DIR}/ since the last nightly; \
             run ./koru.sh nightly-notes, commit the file, and move the tag again"
        )),
        several => Err(format!(
            "{} notes files were added since the last nightly ({}); a build has exactly one",
            several.len(),
            several
                .iter()
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A tag push's `before` on the tag's first push.
pub(crate) fn is_zero_commit(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|byte| byte == b'0')
}

/// `nightly-notes` and `nightly-notes --select`.
pub(crate) fn notes_command(
    root: &Path,
    mut args: impl Iterator<Item = String>,
) -> Result<(), String> {
    match args.next().as_deref() {
        None => draft_notes(root),
        Some("--select") => {
            let (Some(before), Some(after), None) = (args.next(), args.next(), args.next()) else {
                return Err(NIGHTLY_NOTES_USAGE.into());
            };
            let path = select_between(root, &before, &after)?;
            // Read now, so bad notes fail the run here rather than after the
            // tarball's build has started.
            read_notes(&root.join(&path))?;
            println!("{path}");
            Ok(())
        }
        Some(other) => Err(format!(
            "unexpected argument: {other}\n{NIGHTLY_NOTES_USAGE}"
        )),
    }
}

fn select_between(root: &Path, before: &str, after: &str) -> Result<String, String> {
    let lines = |text: String| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect()
    };
    if is_zero_commit(before) {
        let present = git(
            root,
            &["ls-tree", "--name-only", after, &format!("{NOTES_DIR}/")],
        )
        .map_err(|error| format!("listing {NOTES_DIR}/ at {after}: {error}"))?;
        return select_notes(true, &[], &lines(present));
    }
    // `--no-renames`, so a file moved into the folder counts as added rather
    // than as a rename `--diff-filter=A` would not show.
    let added = git(
        root,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--diff-filter=A",
            before,
            after,
            "--",
            &format!("{NOTES_DIR}/"),
        ],
    )
    .map_err(|error| {
        format!(
            "comparing {before}..{after}: {error} (a shallow checkout lacks the tag's previous \
             commit; check out with full history)"
        )
    })?;
    select_notes(false, &lines(added), &[])
}

/// Where the commits a draft describes start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Baseline {
    /// `git log <sha>..HEAD`, and where the sha came from.
    Since { sha: String, from: &'static str },
    /// No `nightly` tag anywhere: the last [`FALLBACK_COMMITS`] commits.
    Fallback,
}

/// **The published nightly is origin's tag**, so it wins when this clone has
/// its commit; the local tag stands in when origin cannot be asked or has
/// none, which covers a tag moved here and not pushed yet.
pub(crate) fn baseline(
    local: Option<&str>,
    remote: Option<&str>,
    remote_is_here: bool,
) -> Result<Baseline, String> {
    match (remote, local) {
        (Some(sha), _) if remote_is_here => Ok(Baseline::Since {
            sha: sha.into(),
            from: "origin's nightly tag",
        }),
        (Some(sha), _) => Err(format!(
            "origin's nightly tag points at {sha}, which this clone does not have; \
             run `git fetch --force origin tag nightly` first"
        )),
        (None, Some(sha)) => Ok(Baseline::Since {
            sha: sha.into(),
            from: "the local nightly tag",
        }),
        (None, None) => Ok(Baseline::Fallback),
    }
}

impl Baseline {
    /// The arguments `git log` takes to list the commits.
    fn log_args(&self) -> Vec<String> {
        match self {
            Baseline::Since { sha, .. } => vec![format!("{sha}..HEAD")],
            Baseline::Fallback => vec![format!("-{FALLBACK_COMMITS}"), "HEAD".into()],
        }
    }
}

fn find_baseline(root: &Path) -> Result<Baseline, String> {
    let tag = format!("refs/tags/{TAG}");
    let peeled = format!("{tag}^{{commit}}");
    let local = git_allow_failure(root, &["rev-parse", "--verify", "--quiet", &peeled])?;
    let local = (!local.is_empty()).then_some(local);
    // Best effort: offline, or a clone with no `origin`, falls back to the
    // local tag and says so.
    let remote = match git(
        root,
        &[
            "ls-remote",
            "--tags",
            "origin",
            &tag,
            &format!("{tag}^{{}}"),
        ],
    ) {
        Ok(listing) => remote_tag_commit(&listing),
        Err(error) => {
            println!(
                "[Koru] could not ask origin for its nightly tag ({error}); using this clone's"
            );
            None
        }
    };
    let remote_is_here = remote
        .as_deref()
        .is_some_and(|sha| git(root, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok());
    baseline(local.as_deref(), remote.as_deref(), remote_is_here)
}

/// The commit an `ls-remote` listing's tag names: the peeled `^{}` line for
/// an annotated tag, else the tag's own line.
pub(crate) fn remote_tag_commit(listing: &str) -> Option<String> {
    let mut direct = None;
    for line in listing.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        if name.ends_with("^{}") {
            return Some(sha.trim().into());
        }
        direct = Some(sha.trim().to_string());
    }
    direct
}

/// Draft the next nightly's notes, print them, and stop: the maintainer edits
/// and commits them (NIGHTLY §9 step 2). **Never commits.**
fn draft_notes(root: &Path) -> Result<(), String> {
    let baseline = find_baseline(root)?;
    let log_args = baseline.log_args();
    let mut oneline = vec!["log", "--no-merges", "--oneline"];
    oneline.extend(log_args.iter().map(String::as_str));
    let commits = git(root, &oneline)?;
    match &baseline {
        Baseline::Since { sha, from } => {
            let short: String = sha.chars().take(7).collect();
            println!("[Koru] commits since {from} ({short}):");
        }
        Baseline::Fallback => println!(
            "[Koru] there is no nightly tag here or on origin, so this draft describes the \
             last {FALLBACK_COMMITS} commits:"
        ),
    }
    if commits.is_empty() {
        return Err("nothing has been committed since the last nightly".into());
    }
    println!("{commits}");

    let existing = fs::read_dir(root.join(NOTES_DIR))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .map(|name| format!("{NOTES_DIR}/{name}"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let relative = next_notes_path(Utc::now().date_naive(), &existing);
    let path = root.join(&relative);
    let log = format!("git log --no-merges {}", log_args.join(" "));
    let draft = notes::generate(root, notes::Request::Nightly { log: &log }, &path)?;
    println!("\n--- {relative} ---");
    println!("{draft}");
    println!("--- end ---\n");
    if let Err(error) = parse_notes(&draft) {
        return Err(format!(
            "the draft is not publishable yet ({error}); edit {relative} before committing it"
        ));
    }
    println!(
        "[Koru] review and edit {relative}, commit it, then move the tag:\n  \
         git tag -f {TAG} && git push -f origin {TAG}"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
