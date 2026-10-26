// SPDX-License-Identifier: MPL-2.0

mod assets;
mod cloud;
#[cfg(test)]
mod docs_crates;
mod index;
mod native;
mod nightly;
mod notes;
mod notices;
mod package;
mod page;
mod source;
mod version;
mod voice;

use chrono::{SecondsFormat, Utc};
use cloud::{Cloud, CloudConfig, HOST};
use koru_update::{
    ReleaseChannel, ReleaseNotes, UpdateManifest, UpdatePackage, UpdatePaths, UpdateTarget,
};
use minisign::{KeyPair, PublicKeyBox, SecretKeyBox};
use notes::offer_generated_notes;
use page::escape_html;
use pulldown_cmark::{Options, Parser, html};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every target this pipeline has ever published, so the shared build-number
/// counter can scan them all: the monotonic-build invariant is per *product*,
/// not per platform, or a mac build could carry a number an installed Windows
/// build already holds.
const PUBLISHED_TARGETS: &[&str] = &["windows-x86_64", "macos-aarch64"];

/// The target this machine builds. Publication happens *from* each platform —
/// a signed mac bundle cannot be produced on Windows or the reverse — so the
/// host decides, and the shared counter in R2 is what keeps the two attached.
const fn host_target() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x86_64"
    } else {
        "unsupported"
    }
}

/// `"macos-aarch64"` → `("macos", "aarch64")`. The manifest carries the halves.
fn split_target(target: &str) -> (String, String) {
    let (platform, architecture) = target.split_once('-').unwrap_or((target, "unknown"));
    (platform.to_string(), architecture.to_string())
}

const VERIFY_ATTEMPTS: u32 = 20;
const VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const MANIFEST_CACHE: &str = "no-cache, max-age=30";

fn main() {
    if let Err(error) = run() {
        eprintln!("koru-release: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = workspace_root()?;
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("cloud-setup") => {
            if args.next().is_some() {
                return Err("usage: koru-release cloud-setup".into());
            }
            cloud_setup()
        }
        Some("dist-cloud") => {
            let channel: ReleaseChannel = args
                .next()
                .ok_or("usage: koru-release dist-cloud <stable|beta|nightly> [version] [--yes]")?
                .parse()?;
            let mut explicit = None;
            let mut yes = false;
            for arg in args {
                if arg == "--yes" {
                    yes = true;
                } else if explicit.replace(arg).is_some() {
                    return Err("only one explicit version may be supplied".into());
                }
            }
            publish(&root, channel, explicit.as_deref(), yes)
        }
        Some("dist-local") => {
            let channel: ReleaseChannel = args.next().ok_or(DIST_LOCAL_USAGE)?.parse()?;
            let options = DistLocalOptions::parse(args)?;
            dist_local(&root, channel, options)
        }
        // The GitHub nightly's build, for CI: writes the release's files into
        // a directory and uploads nothing (`nightly`).
        Some("dist-github") => {
            let channel: ReleaseChannel = args.next().ok_or(nightly::DIST_GITHUB_USAGE)?.parse()?;
            let options = nightly::DistGithubOptions::parse(args)?;
            nightly::dist_github(&root, channel, options)
        }
        Some("nightly-notes") => nightly::notes_command(&root, args),
        // **Not a `dist-*` verb, because it builds nothing.** It reads what is
        // already published and rewrites one page from it.
        Some("release-index") => {
            let mut dry_run = None;
            let mut keep_latest = false;
            let mut args = args;
            while let Some(arg) = args.next() {
                if arg == "--dry-run" {
                    dry_run = Some(PathBuf::from(args.next().ok_or(RELEASE_INDEX_USAGE)?));
                } else if arg == "--keep-latest" {
                    keep_latest = true;
                } else {
                    return Err(format!("unexpected argument: {arg}\n{RELEASE_INDEX_USAGE}"));
                }
            }
            if keep_latest {
                release_index_keep_latest(dry_run)
            } else {
                release_index(dry_run)
            }
        }
        // **Also not a `dist-*` verb.** It builds no Koru: it fetches somebody
        // else's weights, repackages them, and publishes them beside the
        // releases. It writes to production only behind `--upload`.
        Some("voice-models") => voice::run(&root, args),
        Some("publish-assets") => assets::run(&root, args),
        // **Builds nothing either.** Gradle runs it while it builds the APK,
        // and the iOS app's Xcode build phase while it builds the app, for the
        // notices each carries; `macos` checks a Mac package's from any host.
        Some("notices") => notices::run(&root, args),
        _ => Err("usage:\n  koru-release cloud-setup\n  \
             koru-release dist-cloud <stable|beta|nightly> [version] [--yes]\n  \
             koru-release dist-local <stable|beta|nightly|internal> [version] [--build <n>] [--sign-update]\n  \
             koru-release dist-github nightly --build <yyyymmddHHMM> --notes <file> --key <file> [--key-password-stdin] --download-base <url> --out <dir> [--version <v>]\n  \
             koru-release nightly-notes [--select <before-sha> <after-sha>]\n  \
             koru-release release-index [--keep-latest] [--dry-run <page.html>]\n  \
             koru-release voice-models plan|pin|fetch|publish|local [--only <id>] [--work <dir>] [--upload]\n  \
             koru-release notices android --out <file> [--maven <libraries.json>] [--source <url>] [--debug]\n  \
             koru-release notices ios --dir <licenses dir> --source <url> [--debug]\n  \
             koru-release notices macos --out <file> [--triple <triple>]..."
            .into()),
    }
}

/// The whole package, and then nothing.
///
/// It replaces `scripts/dist.{ps1,sh}`, which built `-p koru` alone and staged a
/// *nested* top-level folder — a shape `koru_update::extract_zip` refuses, and a
/// bundle that dies at first launch now that `koru.exe` treats a Store that will
/// not answer as fatal. Two packaging paths that produce different layouts is one
/// too many, so there is now one `package::build` and this verb stops just before
/// the parts that need secrets: no Minisign signature, no Cloudflare, no build
/// number allocated from the published counter.
///
/// **Build number 1, and it is not a detail.** `ReleaseFiles::validate` refuses a
/// zero, so the staged `release-files.json` needs *some* number, and 1 is below
/// every real one — which means the update client would treat any published
/// release as newer and offer to replace this bundle. That is the honest
/// behaviour for a local build and the printed output says so, because the
/// alternative (a number that competes with real releases) is a bundle that
/// silently declines the update it should take.
/// The release notes for a version, when a descriptor for it happens to exist.
///
/// **Every failure is `None`, deliberately.** `publish` reads the same file
/// through a path that refuses without it, because a published release with no
/// notes is not one. Here the file is usually absent by design, and a local
/// bundle is not the place to turn a missing nicety into an error the developer
/// has to work around.
fn local_notes(root: &Path, version: &str) -> Option<String> {
    let path = root.join("releases").join(format!("{version}.toml"));
    let descriptor: Descriptor = toml::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some(descriptor.body)
}

const DIST_LOCAL_USAGE: &str = "usage: koru-release dist-local <stable|beta|nightly|internal> [version] [--build <n>] [--sign-update]";

/// What `dist-local` was asked to build beyond the channel.
///
/// `--build` and `--sign-update` support the signed local updater rehearsal in
/// `docs/architecture/api/UPDATE_AND_ROLLBACK.md`, which needs two installable bundles
/// whose build numbers order them and whose updater can verify a Minisign
/// signature against this machine's own release key. That is the same path
/// `dist-cloud` publishes, so the updater under rehearsal is the shipped
/// updater rather than a fork. A plain `dist-local` keeps build 1 and no key,
/// and stays honest about what that bundle cannot do.
struct DistLocalOptions {
    version: Option<String>,
    build_number: u64,
    sign_update: bool,
}

impl DistLocalOptions {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut version = None;
        let mut build_number = 1u64;
        let mut sign_update = false;
        while let Some(arg) = args.next() {
            if arg == "--sign-update" {
                sign_update = true;
            } else if arg == "--build" {
                build_number = args
                    .next()
                    .ok_or(DIST_LOCAL_USAGE)?
                    .parse()
                    .map_err(|_| DIST_LOCAL_USAGE.to_string())?;
            } else if arg.starts_with("--") || version.replace(arg).is_some() {
                return Err(DIST_LOCAL_USAGE.into());
            }
        }
        if build_number == 0 {
            // `ReleaseFiles::validate` refuses a zero later anyway; refusing it
            // here names the flag that carried it instead of a staged file.
            return Err("--build 0 is refused everywhere; use 1 or higher".into());
        }
        Ok(Self {
            version,
            build_number,
            sign_update,
        })
    }
}

fn dist_local(
    root: &Path,
    channel: ReleaseChannel,
    options: DistLocalOptions,
) -> Result<(), String> {
    // Allowed to fail: a local bundle is worth building from a tarball with no
    // `.git` in it, and an unknown commit is a fact about the bundle rather than
    // a reason to refuse. `publish` uses the strict `git` for the opposite
    // reason — a published archive that cannot name its commit is not one.
    let commit = git_allow_failure(root, &["rev-parse", "HEAD"])?;
    let short_commit: String = commit.chars().take(7).collect();
    let short_commit = if short_commit.is_empty() {
        "nogit".to_string()
    } else {
        short_commit
    };
    let released_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    // A version is needed for the folder name and the `VERSION` file, but not
    // validated against the channel's shape: this bundle is never published, and
    // refusing `0.0.0-local` would only make the developer invent a lie that
    // passes.
    let version = options
        .version
        .clone()
        .unwrap_or_else(|| format!("0.0.0-local.{short_commit}"));
    // **A local bundle is not immutable**, so last run's is replaced rather than
    // refused. `package::build` guards against overwriting an archive because a
    // *published* one has a signature and a manifest pointing at its bytes;
    // nothing points at this one, and refusing would make the second
    // `dist-local` of the day an error message about a file the developer has
    // to go and delete by hand.
    let archive = package::archive_path(root, &version, host_target());
    remove_local_artifact_set(&archive)?;
    // No public key by default: `KORU_UPDATE_PUBLIC_KEY` is what the binary
    // verifies a downloaded package against, and an empty one is the truthful
    // value for a build that was never signed — it is what a plain
    // `cargo build` compiles in. `--sign-update` swaps in this machine's
    // release key so the rehearsal bundles can verify each other.
    let signing = if options.sign_update {
        let paths = release_paths()?;
        ensure_keypair(&paths)?;
        let public_key = fs::read_to_string(&paths.public)
            .map_err(|error| format!("release public key: {error}"))?;
        // The password prompt happens here, before the build — see
        // `open_release_secret` for why the order matters.
        Some((open_release_secret(&paths)?, public_key))
    } else {
        None
    };
    let package = package::build(
        root,
        &version,
        options.build_number,
        channel.as_str(),
        &commit,
        &released_at,
        signing.as_ref().map(|(_, key)| key.as_str()).unwrap_or(""),
        // **Read if it happens to be there, and not required.** The default
        // version is `0.0.0-local.<commit>`, which has no descriptor and never
        // will — but a developer who named a real version is usually testing
        // that release, and a bundle silently missing its notes would be the
        // one difference between this path and the published one.
        local_notes(root, &version).as_deref(),
        native::Rule::Local,
        package::Baked::Production,
    )?;
    println!("[Koru] local bundle: {}", package.archive.display());
    println!("[Koru] sha256: {}", package.sha256);
    match signing {
        Some((secret, public_key)) => {
            let signature = sign_archive(&secret, &package.archive, &public_key, &version)?;
            let signature_path = artifact_sidecar(&package.archive, ".minisig");
            fs::write(&signature_path, &signature).map_err(|error| error.to_string())?;
            let (platform, architecture) = split_target(host_target());
            let manifest = UpdateManifest {
                schema_version: 1,
                channel,
                platform: platform.clone(),
                architecture: architecture.clone(),
                version: version.clone(),
                build_number: options.build_number,
                published_at: released_at.clone(),
                commit: (!commit.is_empty()).then(|| short_commit.clone()),
                source: source::url(&commit),
                minimum_updater_version: Some(1),
                minimum_supported_build: None,
                required: false,
                release_notes: ReleaseNotes {
                    title: "Local update rehearsal".into(),
                    summary: vec![
                        "A locally signed package for exercising Koru's real updater.".into(),
                    ],
                    url: None,
                },
                package: UpdatePackage {
                    url: format!(
                        "https://local.invalid/{}",
                        package
                            .archive
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or("the local package has no file name")?
                    ),
                    file_name: package
                        .archive
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("the local package has no file name")?
                        .to_string(),
                    size: package.size,
                    sha256: package.sha256.clone(),
                    signature: signature.clone(),
                    format: package::package_format(host_target()),
                },
            };
            manifest
                .validate(&UpdateTarget {
                    channel,
                    platform,
                    architecture,
                })
                .map_err(|error| error.to_string())?;
            let manifest_path = artifact_sidecar(&package.archive, ".update.json");
            fs::write(
                &manifest_path,
                serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            println!("[Koru] update signature: {}", signature_path.display());
            println!("[Koru] rehearsal manifest: {}", manifest_path.display());
            println!(
                "[Koru] build {} signed for the local updater rehearsal. Never publish \
                 these numbers: the shared counter owns the real ones.",
                options.build_number
            );
            println!(
                "[Koru] launch build A from inside an isolated root with: \
                 koru-agent --root <root> --rehearse-update \"{}\"",
                manifest_path.display()
            );
        }
        None => println!(
            "[Koru] build number {} and no signing key: this bundle cannot verify or \
             install an update, and any published release will look newer than it.",
            options.build_number
        ),
    }
    Ok(())
}

/// Remove every file a previous local build could have written for `archive`.
///
/// The signature and rehearsal manifest describe the archive's exact bytes.
/// Keeping either beside a replacement archive is therefore worse than merely
/// untidy: a plain build after a signed rehearsal would appear to carry update
/// metadata that belongs to an older package. Treat the three siblings as one
/// replaceable local artifact set, whether this run will sign the new archive
/// or not.
fn remove_local_artifact_set(archive: &Path) -> Result<(), String> {
    for path in [
        artifact_sidecar(archive, ".minisig"),
        artifact_sidecar(archive, ".update.json"),
        archive.to_path_buf(),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "remove previous local artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn artifact_sidecar(archive: &Path, suffix: &str) -> PathBuf {
    let mut path = archive.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

fn cloud_setup() -> Result<(), String> {
    let cloud = Cloud::discover(cloudflare_token()?)?;
    println!(
        "[Koru] found koru.do in Cloudflare account {}",
        cloud.config.account_id
    );
    cloud.setup()?;
    // **A survivable failure, and the only one in this function.** The token
    // that has provisioned this origin since the first release carries R2
    // Storage Write, Zone Read and Cache Rules Edit —
    // and none of those is Transform Rules Edit. Failing the whole of setup
    // over the last of eleven steps would mean re-running the ten that worked,
    // so this prints what to do instead.
    if let Err(error) = cloud.install_url_rewrite() {
        println!("[Koru] ACTION REQUIRED: the /releases/ rewrite was not installed ({error}).");
        println!("[Koru] Without it https://{HOST}/releases/ answers 404, because an R2 custom");
        println!("[Koru] domain does not resolve a directory index. Add it by hand:");
        println!("{}", Cloud::rewrite_rule_by_hand());
        println!(
            "[Koru] ...or widen CLOUDFLARE_API_TOKEN with Zone -> Transform Rules -> Edit \
             and re-run cloud-setup."
        );
    }
    let paths = release_paths()?;
    fs::create_dir_all(&paths.root).map_err(|error| error.to_string())?;
    save_new_or_matching_json(&paths.cloud, &cloud.config)?;
    ensure_keypair(&paths)?;
    println!("[Koru] R2 bucket, custom domain, TLS and cache rules are configured.");
    println!("[Koru] release configuration: {}", paths.root.display());
    println!(
        "[Koru] keep {} private and backed up; it cannot be recovered from Cloudflare.",
        paths.secret.display()
    );
    Ok(())
}

const RELEASE_INDEX_USAGE: &str =
    "usage: koru-release release-index [--keep-latest] [--dry-run <page.html>]";

/// **A dry run needs no credentials**, because everything the index is built
/// from is public. That makes rehearsing the page something anybody in the
/// tree can do, rather than only whoever holds the release token.
fn release_index_cloud(dry_run: bool) -> Result<Cloud, String> {
    if dry_run {
        return Cloud::public_reader();
    }
    let paths = release_paths()?;
    let config: CloudConfig = read_json(&paths.cloud).map_err(|_| {
        "release cloud configuration is missing; run `koru-release cloud-setup` first".to_string()
    })?;
    Cloud::from_config(cloudflare_token()?, config)
}

/// Rebuild `https://updates.koru.do/releases/` from the published history.
///
/// The backfill and the repair path are the same command, deliberately: an
/// index nobody can rebuild is one that silently stays wrong. It reads only
/// what the origin already serves, so running it twice is running it once.
/// It honours `_publisher/index-floor.json`, so it never lists what
/// `--keep-latest` hid.
fn release_index(dry_run: Option<PathBuf>) -> Result<(), String> {
    let cloud = release_index_cloud(dry_run.is_some())?;
    let floors = index::fetch_floors(&cloud)?;
    let built = rebuild_release_index(&cloud, &floors)?;
    match dry_run {
        Some(path) => {
            let (_, html) = index::rendered(built, &floors)?;
            write_dry_run(&path, html.as_bytes())?;
            println!("[Koru] uploaded nothing");
            Ok(())
        }
        None => index::store(&cloud, built, &floors),
    }
}

/// **List only the newest build of each channel and target from now on.**
///
/// Raises `_publisher/index-floor.json` to the newest build of every
/// `channel/target` the current index carries, then stores the trimmed index
/// and page. Nothing is deleted: packages, notes pages, `history/` objects and
/// channel manifests are untouched, so installed clients keep updating. Later
/// publishes are above the floor and are listed as usual, and a rebuild reads
/// the floor, so it does not bring the hidden builds back.
///
/// With `--dry-run <page.html>` the page is written there and the would-be
/// `releases.json` and `index-floor.json` beside it, and nothing is uploaded.
fn release_index_keep_latest(dry_run: Option<PathBuf>) -> Result<(), String> {
    let cloud = release_index_cloud(dry_run.is_some())?;
    let floors = index::fetch_floors(&cloud)?;
    let current = match cloud.fetch_public(index::INDEX_KEY)? {
        Some(bytes) => index::ReleaseIndex::parse(&bytes)?,
        None => {
            println!("[Koru] no release index yet; rebuilding it from the published history");
            rebuild_release_index(&cloud, &floors)?
        }
    };
    // **The channel manifests too, because the index can lag.** It is updated
    // best-effort after a publish, and the live one was found missing every
    // macOS beta. A pair the index does not carry would get no floor, and the
    // next rebuild would list its whole history. A channel manifest is by
    // definition the newest build of its pair, so folding it in costs six
    // reads and makes the floors exactly what each channel serves.
    let mut serving = Vec::new();
    for channel in ReleaseChannel::PUBLIC {
        for target in PUBLISHED_TARGETS {
            match fetch_manifest(&cloud, channel, target) {
                Ok(Some(manifest)) => serving.push(manifest),
                Ok(None) => {}
                Err(error) => println!("[Koru] warning: channels/{channel}/{target}.json: {error}"),
            }
        }
    }
    let current = index::with_published(current, &serving);
    let (trimmed, floors, hidden) = index::trim_to_latest(current, floors);

    for (pair, build) in &floors.floors {
        println!("[Koru] floor {pair}: build {build}");
    }
    let mut kept: Vec<_> = trimmed.entries.iter().collect();
    kept.sort_by_key(|entry| (entry.channel.as_str(), entry.target.clone()));
    for entry in kept {
        println!(
            "[Koru] keeps {} {} {} (build {})",
            entry.channel, entry.target, entry.version, entry.build_number
        );
    }
    println!("[Koru] {} listed, {hidden} hidden", trimmed.entries.len());

    match dry_run {
        Some(page) => {
            let beside = page.parent().unwrap_or(Path::new("."));
            write_dry_run(&beside.join("index-floor.json"), &floors.to_json()?)?;
            let (json, html) = index::rendered(trimmed, &floors)?;
            write_dry_run(&beside.join("releases.json"), &json)?;
            write_dry_run(&page, html.as_bytes())?;
            println!("[Koru] uploaded nothing");
            Ok(())
        }
        None => {
            // The floor first: if the index upload then fails, the next publish
            // or rebuild still applies it, so nothing hidden can come back.
            index::store_floors(&cloud, &floors)?;
            index::store(&cloud, trimmed, &floors)
        }
    }
}

fn write_dry_run(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    println!("[Koru] wrote {}", path.display());
    Ok(())
}

/// Probe the whole published history, up to the newest build any channel or
/// the counter has seen, skipping what is below a floor.
fn rebuild_release_index(
    cloud: &Cloud,
    floors: &index::IndexFloors,
) -> Result<index::ReleaseIndex, String> {
    // The counter is the top of the range, but a manifest that somehow ran past
    // it would be missed — the same guard `allocate_build_number` already has,
    // for the same reason.
    let highest = ReleaseChannel::PUBLIC
        .into_iter()
        .filter_map(|channel| newest_across_targets(cloud, channel).transpose())
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .map(|manifest| manifest.build_number)
        .max()
        .unwrap_or(0)
        .max(published_build_counter(cloud)?);
    if highest == 0 {
        return Err("nothing has been published to this origin yet".into());
    }
    index::rebuild(cloud, highest, floors)
}

/// The two hooks that turn a mac package from ad-hoc-signed into a
/// Developer ID build with a notarization ticket stapled to it. See
/// `package::mac_sign` / `package::mac_notarize` and docs/features/macos/RELEASING.md.
const MAC_SIGNING_HOOKS: [&str; 2] = ["KORU_MAC_SIGN_COMMAND", "KORU_MAC_NOTARIZE_COMMAND"];

/// **One predicate, asked twice.** The publish gate refuses an unattended
/// unsigned mac publish, and the release page words its install sentence for
/// the build that is about to be produced — and those were two different
/// questions in two spellings: the gate looked at both hooks with `var_os`,
/// the page at the notarization hook alone with `var`. A shell exporting only
/// the signing hook therefore got a page promising the notarized first-open
/// flow over a build that would show "Apple could not verify", and a release
/// page is immutable, so that is not repairable after the fact.
fn mac_signing_is_configured() -> bool {
    MAC_SIGNING_HOOKS
        .iter()
        .all(|name| std::env::var_os(name).is_some())
}

/// What the release page tells a Mac person to do with the disk image.
///
/// **Pure, because it used to be untestable.** The sentence was chosen inline
/// from the process environment, so `the_notes_page_links_to_the_package_beside_it`
/// asserted on whichever branch the developer's shell happened to produce —
/// it passed on a machine with no signing hooks and would have failed on a
/// publishing Mac, which is where MAC.md tells people to export them
/// permanently. A page is immutable once published; the words on it deserve a
/// test that names which words it is testing.
///
/// **The `xattr -dr com.apple.quarantine` instruction is gone, from both
/// branches.** Notarization shipped (MAC.md Phase B), so on the ordinary path
/// it was simply false — and it was never the right advice on the other one
/// either: it teaches a reader to strip a security attribute by hand, which is
/// a habit worth much more to an attacker than to us. Open Anyway is the
/// supported flow and it is what the un-notarized branch now says.
///
/// That branch survives rather than being deleted because a publish *can*
/// still be ad hoc — `publish` asks "Publish UNSIGNED anyway?" and refuses it
/// unattended, but a person may answer yes. A page that claimed notarization
/// over that build would be a lie nobody could correct afterwards.
fn mac_install_sentence(notarized: bool) -> &'static str {
    if notarized {
        "Open it and drag Koru into Applications; macOS asks once whether to \
         open an app downloaded from the internet."
    } else {
        "Open it and drag Koru into Applications. This build is not notarized, \
         so the first attempt is refused and macOS offers Open Anyway under \
         System Settings &gt; Privacy &amp; Security."
    }
}

fn publish(
    root: &Path,
    channel: ReleaseChannel,
    explicit: Option<&str>,
    yes: bool,
) -> Result<(), String> {
    let commit = require_clean_tree(root, None)?;
    let paths = release_paths()?;
    let config: CloudConfig = read_json(&paths.cloud).map_err(|_| {
        "release cloud configuration is missing; run `koru-release cloud-setup` first".to_string()
    })?;
    let public_key = fs::read_to_string(&paths.public)
        .map_err(|_| "release public key is missing; run cloud-setup first")?;
    // The password prompt happens here, before the build — see
    // `open_release_secret` for why the order matters.
    let release_secret = open_release_secret(&paths)?;
    let cloud = Cloud::from_config(cloudflare_token()?, config)?;
    let target = host_target();
    if target == "unsupported" {
        return Err("dist-cloud does not know this host's target triple".into());
    }
    // **Linux's feed is a GitHub release asset**, not an origin object
    // (NIGHTLY N7), so a Linux build published here would be a manifest no
    // installed Koru reads.
    if target.starts_with("linux") {
        return Err(
            "Linux is published to GitHub, not to the update origin; use dist-github".into(),
        );
    }
    // The version suggestion follows the channel's newest release on *any*
    // target, so the second platform of a version proposes that same version
    // rather than inventing the next one.
    let stable = newest_across_targets(&cloud, ReleaseChannel::Stable)?;
    let beta = newest_across_targets(&cloud, ReleaseChannel::Beta)?;
    let nightly = newest_across_targets(&cloud, ReleaseChannel::Nightly)?;
    let short_commit = commit.chars().take(7).collect::<String>();
    // One instant for the whole publication: the date compiled into the binary,
    // the date in the version string and the manifest's `published_at` are then
    // the same day by construction, rather than three readings of the clock
    // that disagree when a release spans midnight UTC.
    let released = Utc::now();
    let date = released.format("%Y%m%d").to_string();
    // Seconds, not chrono's default nanoseconds: this string is compiled into
    // the binary, written into VERSION and published in the manifest, and a
    // nine-digit fraction is noise in all three.
    let released_at = released.to_rfc3339_opts(SecondsFormat::Secs, true);
    let inferred = version::suggest_version(
        channel,
        stable.as_ref(),
        beta.as_ref(),
        nightly.as_ref(),
        &date,
        &short_commit,
    )?;
    let version = explicit.unwrap_or(&inferred);
    validate_release_version(channel, version)?;
    println!("[Koru] proposed {channel} version: {version}");
    // **An unsigned mac publish is a decision, never a default.** With the two
    // hooks unset, `package::build` falls back to an ad-hoc signature and skips
    // notarization — behind one printed note — and every downloader then meets
    // Gatekeeper's "Apple could not verify" malware dialog. That shipped twice
    // from a shell that simply lacked the exports, and a published release is
    // immutable, so the mistake cannot be repaired in place. Ask by name,
    // before the version confirm, and refuse under `--yes`: an unattended
    // publish has nobody to answer for the degradation.
    if cfg!(target_os = "macos") {
        let unset: Vec<&str> = MAC_SIGNING_HOOKS
            .iter()
            .copied()
            .filter(|name| std::env::var_os(name).is_none())
            .collect();
        if !unset.is_empty() {
            println!(
                "[Koru] {} not set: this publish would be ad-hoc signed and NOT notarized, \
                 and every download will show the \"Apple could not verify\" malware dialog.",
                unset.join(" and ")
            );
            if yes || !confirm("Publish UNSIGNED anyway? [y/N] ")? {
                return Err(
                    "publication canceled: export the signing hooks (see docs/features/macos/RELEASING.md) \
                     or answer y to publish unsigned"
                        .into(),
                );
            }
        }
    }
    if !yes && !confirm("Publish this version? [y/N] ")? {
        return Err("publication canceled".into());
    }

    let descriptor_path = root.join("releases").join(format!("{version}.toml"));
    if !descriptor_path.exists() {
        offer_generated_notes(root, channel, &descriptor_path, yes)?;
    }
    let descriptor: Descriptor = toml::from_str(
        &fs::read_to_string(&descriptor_path).map_err(|_| {
            format!(
                "create {} before publishing; each release, including each nightly, has its own descriptor",
                descriptor_path.display()
            )
        })?,
    )
    .map_err(|error| format!("{} is invalid: {error}", descriptor_path.display()))?;
    descriptor.validate()?;

    // Generated notes must be committed before they can be part of a release.
    require_clean_tree(root, Some(&commit))?;

    // A nightly that exposes Voice input must also make both product models
    // reachable from the catalog compiled into that build.
    if matches!(channel, ReleaseChannel::Nightly) {
        voice::publish_for_nightly(root)?;
    }

    if !matches!(channel, ReleaseChannel::Nightly) {
        ensure_tag(root, version)?;
    }
    let build_number =
        allocate_build_number(&cloud, [stable.as_ref(), beta.as_ref(), nightly.as_ref()])?;
    println!("[Koru] allocated build {build_number}");
    let package = package::build(
        root,
        version,
        build_number,
        channel.as_str(),
        &commit,
        &released_at,
        &public_key,
        // The same string `Descriptor::html` renders the page from, so the page
        // and the tab Koru opens after installing cannot say different things.
        Some(descriptor.body.as_str()),
        // A published release links the canonical sherpa-onnx archive or is
        // refused before it is signed.
        native::Rule::Official,
        package::Baked::Production,
    )?;
    require_clean_tree(root, Some(&commit))?;
    let signature = sign_archive(&release_secret, &package.archive, &public_key, version)?;
    // **Named before the page is rendered**, because the page links to it. Both
    // keys live in one `releases/<version>/` prefix, which is what lets that
    // link be the bare file name.
    let package_name = format!("koru-{target}.{}", package::archive_extension(target));
    let package_key = format!("releases/{version}/{package_name}");
    let notes_key = format!("releases/{version}/index.html");
    let notes_html = descriptor.html(
        version,
        &released_at,
        &Download {
            file_name: &package_name,
            size: package.size,
            sha256: &package.sha256,
        },
    );
    if cloud.fetch_public(&package_key)?.is_some() {
        return Err(format!(
            "release {version} already exists in R2 for {target}; immutable releases are never replaced"
        ));
    }
    // **The notes page belongs to the version's first target.** It is
    // immutable once uploaded, so the second platform of a version reuses it
    // rather than replacing it — which means the page offers the first
    // target's download only. A joint page is the recorded follow-up in
    // docs/features/macos/RELEASING.md.
    let notes_already_published = cloud.fetch_public(&notes_key)?.is_some();
    println!("[Koru] uploading immutable package...");
    // **The archive first, then the page that links to it.** It was already this
    // order and is now load-bearing rather than incidental: uploading the notes
    // first would publish a download button pointing at a key that does not
    // exist yet, and a run that failed in between would leave that page up
    // permanently — immutable, so not replaceable. The same reasoning the
    // channel manifest follows by being written last.
    cloud.put_file(
        &package_key,
        &package.archive,
        if package_name.ends_with(".dmg") {
            "application/x-apple-diskimage"
        } else {
            "application/zip"
        },
        IMMUTABLE_CACHE,
    )?;
    if notes_already_published {
        println!("[Koru] release page for {version} already exists; reusing it");
    } else {
        cloud.put_bytes(
            &notes_key,
            notes_html.into_bytes(),
            "text/html; charset=utf-8",
            IMMUTABLE_CACHE,
        )?;
    }
    verify_uploaded_package(&cloud, &package_key, &package.sha256)?;

    let (platform, architecture) = split_target(target);
    let manifest = UpdateManifest {
        schema_version: 1,
        channel,
        platform,
        architecture,
        version: version.into(),
        build_number,
        published_at: released_at.clone(),
        commit: Some(short_commit),
        source: source::url(&commit),
        minimum_updater_version: Some(1),
        minimum_supported_build: None,
        required: false,
        release_notes: ReleaseNotes {
            title: descriptor.title,
            summary: descriptor.summary,
            url: Some(format!("https://{HOST}/{notes_key}")),
        },
        package: UpdatePackage {
            url: format!("https://{HOST}/{package_key}"),
            file_name: package_name,
            size: package.size,
            sha256: package.sha256,
            signature,
            format: package::package_format(target),
        },
    };
    publish_manifest(&cloud, &manifest)?;
    let beta_here = fetch_manifest(&cloud, ReleaseChannel::Beta, target)?;
    // Every manifest this run actually wrote — two when a stable release also
    // advances the beta pointer. The index needs both, or the Beta tab loses
    // the release beta users were served.
    let mut published = vec![manifest.clone()];
    if channel == ReleaseChannel::Stable
        && beta_here
            .as_ref()
            .is_none_or(|current| current.build_number < build_number)
    {
        let mut beta_pointer = manifest.clone();
        beta_pointer.channel = ReleaseChannel::Beta;
        publish_manifest(&cloud, &beta_pointer)?;
        published.push(beta_pointer);
    }
    println!("[Koru] published {version} (build {build_number}) to the {channel} channel");
    println!("[Koru] package: {}", manifest.package.url);
    if let Some(notes) = &manifest.release_notes.url {
        println!("[Koru] notes:   {notes}");
    }

    // **The descriptor is the only copy of the notes.** The manifest carries a
    // title, summary lines and a URL; the prose lives in `releases/*.toml` and
    // is what `RELEASE_NOTES.md` ships from. Leaving it uncommitted is how a
    // published release ends up with notes nobody else can read — and it is
    // also why `warn_if_dirty` fires on the *next* publish about a file this
    // one wrote.
    //
    // After the upload, so what gets recorded is what actually shipped.
    // Pathspec-limited on both halves — `git add -- releases` picks up a
    // descriptor that is still untracked, and committing with the same
    // pathspec means whatever else the operator had staged stays staged. A
    // release tool must not sweep up somebody's work in progress.
    commit_release_notes(root);

    // **Last, and never fatal.** The release is live the moment the channel
    // manifest is written; the index is derived from it. A failure here is a
    // page that is one release stale, which one command fixes — reporting it as
    // a failed publication would send somebody looking for a release that is
    // already being served. The success block is printed first for the same
    // reason: at this point the release *is* published, and a rebuild should
    // not sit between the operator and that fact.
    if let Err(error) = index::update(&cloud, &published) {
        println!(
            "[Koru] WARNING: the release index was not updated ({error}); \
             run `koru-release release-index`. The release itself is live."
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct Descriptor {
    title: String,
    summary: Vec<String>,
    body: String,
}

impl Descriptor {
    fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty()
            || self.summary.is_empty()
            || self.summary.iter().any(|item| item.trim().is_empty())
            || self.body.trim().is_empty()
        {
            Err("release descriptor requires a title, non-empty summary items, and body".into())
        } else {
            Ok(())
        }
    }

    fn html(&self, version: &str, published: &str, download: &Download<'_>) -> String {
        let mut body = String::new();
        html::push_html(&mut body, Parser::new_ext(&self.body, Options::all()));
        // **Escaped even though a version cannot contain markup.**
        // `validate_release_version` parses with semver, so `<` never reaches
        // here today — but the page is immutable once uploaded, which makes it
        // the wrong place to rely on a check made somewhere else. The body is
        // deliberately *not* escaped: it is Markdown the author wrote, already
        // rendered to HTML by pulldown-cmark.
        let title = escape_html(&self.title);
        let published = escape_html(published);
        // `Download::html` escapes the version itself, so it is handed the raw
        // one; everything interpolated here is escaped first.
        let download = download.html(version);
        let version = escape_html(version);
        page::shell(
            &format!("Koru {version} - {title}"),
            // The notes page needs nothing beyond the shared stylesheet; the
            // index is what adds to it.
            "",
            "",
            &format!(
                "<a href=\"https://koru.do\">Koru</a><h1>{title}</h1>\
                 <p>Koru {version} · <time>{published}</time></p>{download}{body}"
            ),
        )
    }
}

/// The package this page is the notes for.
///
/// **A release page that does not offer the release is a dead end**, and this
/// one was: it named a version, dated it and described what changed, with no way
/// to get it. The in-app banner has the package URL from the manifest, so
/// anybody who arrived through Koru already had the build — but the page is also
/// what a link in a message or a changelog points at, and that reader had
/// nowhere to go.
struct Download<'a> {
    /// The archive's file name. **A sibling key, so the link is relative.**
    /// `koru-release` uploads `releases/<version>/index.html` and
    /// `releases/<version>/koru-windows-x86_64.zip` into one prefix, so naming
    /// the file alone is both shorter and durable: it keeps working if the
    /// origin ever moves off `updates.koru.do`, which an absolute URL baked into
    /// an immutable page would not.
    file_name: &'a str,
    size: u64,
    /// Published beside the link because the archive is Minisign-signed and a
    /// person who cares can check it by hand. Koru verifies both the hash and
    /// the signature before installing; this is for everyone else.
    sha256: &'a str,
}

impl Download<'_> {
    fn html(&self, version: &str) -> String {
        let megabytes = self.size as f64 / (1024.0 * 1024.0);
        // The host builds the package, so the host names the platform. The
        // page belongs to the version's *first* published target (see the
        // reuse in `publish`), which is what these words describe.
        let (platform, spec, run) = if cfg!(target_os = "macos") {
            (
                "Mac",
                "Apple silicon Mac",
                mac_install_sentence(mac_signing_is_configured()),
            )
        } else {
            (
                "Windows",
                "64-bit Windows",
                "Extract it anywhere and run <code>koru.bat</code>.",
            )
        };
        format!(
            "<div class=\"get\">\
             <a class=\"zip\" href=\"{file}\" download>Download Koru {version} for {platform}</a>\
             <p>{file} · {megabytes:.1} MB · {spec}<br>\
             {run} Your content is kept outside \
             the folder, so replacing it later changes nothing you have written.<br>\
             SHA-256 <code>{sha}</code></p></div>",
            file = escape_html(self.file_name),
            version = escape_html(version),
            sha = escape_html(self.sha256),
        )
    }
}

#[derive(Default, Deserialize, Serialize)]
struct BuildCounter {
    last_build_number: u64,
}

/// The last build number this origin handed out.
///
/// Factored out because the reindexer needs the same number for the top of its
/// probe range, and two readers of one object should not be two parsers of it.
fn published_build_counter(cloud: &Cloud) -> Result<u64, String> {
    Ok(cloud
        .fetch_public("_publisher/build-number.json")?
        .map(|bytes| serde_json::from_slice::<BuildCounter>(&bytes))
        .transpose()
        .map_err(|error| format!("published build counter is invalid: {error}"))?
        .unwrap_or_default()
        .last_build_number)
}

fn allocate_build_number(
    cloud: &Cloud,
    manifests: [Option<&UpdateManifest>; 3],
) -> Result<u64, String> {
    let counter = published_build_counter(cloud)?;
    let manifest_max = manifests
        .into_iter()
        .flatten()
        .map(|manifest| manifest.build_number)
        .max()
        .unwrap_or(0);
    let next = counter
        .max(manifest_max)
        .checked_add(1)
        .ok_or("build number overflow")?;
    cloud.put_bytes(
        "_publisher/build-number.json",
        serde_json::to_vec_pretty(&BuildCounter {
            last_build_number: next,
        })
        .map_err(|error| error.to_string())?,
        "application/json",
        "no-store",
    )?;
    Ok(next)
}

fn publish_manifest(cloud: &Cloud, manifest: &UpdateManifest) -> Result<(), String> {
    let target = UpdateTarget {
        channel: manifest.channel,
        platform: manifest.platform.clone(),
        architecture: manifest.architecture.clone(),
    };
    manifest
        .validate(&target)
        .map_err(|error| error.to_string())?;
    let name = format!("{}-{}", manifest.platform, manifest.architecture);
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| error.to_string())?;
    cloud.put_bytes(
        &format!(
            "history/{}/{name}/{}.json",
            manifest.channel, manifest.build_number
        ),
        bytes.clone(),
        "application/json",
        IMMUTABLE_CACHE,
    )?;
    cloud.put_bytes(
        &format!("channels/{}/{name}.json", manifest.channel),
        bytes,
        "application/json",
        MANIFEST_CACHE,
    )
}

fn fetch_manifest(
    cloud: &Cloud,
    channel: ReleaseChannel,
    target: &str,
) -> Result<Option<UpdateManifest>, String> {
    let (platform, architecture) = split_target(target);
    cloud
        .fetch_public(&format!("channels/{channel}/{target}.json"))?
        .map(|bytes| {
            let target = UpdateTarget {
                channel,
                platform,
                architecture,
            };
            UpdateManifest::parse_and_validate(&bytes, &target).map_err(|error| error.to_string())
        })
        .transpose()
}

/// The channel's newest published release, whichever platform carries it.
fn newest_across_targets(
    cloud: &Cloud,
    channel: ReleaseChannel,
) -> Result<Option<UpdateManifest>, String> {
    let mut newest: Option<UpdateManifest> = None;
    for target in PUBLISHED_TARGETS {
        if let Some(manifest) = fetch_manifest(cloud, channel, target)?
            && newest
                .as_ref()
                .is_none_or(|current| current.build_number < manifest.build_number)
        {
            newest = Some(manifest);
        }
    }
    Ok(newest)
}

/// Decrypt the release secret key, prompting for its password on the terminal.
///
/// Called **before** the ten-minute release build, deliberately: the prompt
/// used to sit inside `sign_archive`, after the build — so a run with no tty
/// (a background shell, CI) built everything, notarized it, and then died with
/// "Device not configured", and an interactive run held its question until the
/// operator had walked away.
fn open_release_secret(paths: &ReleasePaths) -> Result<minisign::SecretKey, String> {
    SecretKeyBox::from_string(
        &fs::read_to_string(&paths.secret).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?
    .into_secret_key(None)
    .map_err(|error| error.to_string())
}

fn sign_archive(
    secret: &minisign::SecretKey,
    archive: &Path,
    public_key: &str,
    version: &str,
) -> Result<String, String> {
    let public = PublicKeyBox::from_string(public_key)
        .map_err(|error| error.to_string())?
        .into_public_key()
        .map_err(|error| error.to_string())?;
    let file = File::open(archive).map_err(|error| error.to_string())?;
    minisign::sign(
        Some(&public),
        secret,
        file,
        Some(&format!("Koru {version}")),
        Some("Koru update package"),
    )
    .map(|signature| signature.to_string())
    .map_err(|error| error.to_string())
}

/// An object is not served from the edge the instant the API accepts it, so a
/// single 404 here means "not yet", not "lost". Only exhausting the window is a
/// failure — and a hash that does not match never is retried, since that is a
/// real mismatch rather than a race.
fn verify_uploaded_package(cloud: &Cloud, key: &str, expected: &str) -> Result<(), String> {
    let mut bytes = None;
    for attempt in 0..VERIFY_ATTEMPTS {
        if let Some(found) = cloud.fetch_public(key)? {
            bytes = Some(found);
            break;
        }
        if attempt + 1 < VERIFY_ATTEMPTS {
            println!("[Koru] waiting for {key} to be served...");
            std::thread::sleep(VERIFY_INTERVAL);
        }
    }
    let bytes = bytes.ok_or_else(|| {
        format!(
            "uploaded package is not available at https://{HOST}/{key} after {}s",
            VERIFY_ATTEMPTS * VERIFY_INTERVAL.as_secs() as u32
        )
    })?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "uploaded package hash differs: expected {expected}, got {actual}"
        ))
    }
}

struct ReleasePaths {
    root: PathBuf,
    cloud: PathBuf,
    public: PathBuf,
    secret: PathBuf,
}

fn release_paths() -> Result<ReleasePaths, String> {
    let root = UpdatePaths::discover()
        .ok_or("could not locate the per-user application data directory")?
        .data_dir
        .join("release");
    Ok(ReleasePaths {
        cloud: root.join("cloud.json"),
        public: root.join("minisign.pub"),
        secret: root.join("minisign.key"),
        root,
    })
}

fn ensure_keypair(paths: &ReleasePaths) -> Result<(), String> {
    match (paths.public.exists(), paths.secret.exists()) {
        (true, true) => return Ok(()),
        (true, false) | (false, true) => {
            return Err(
                "only one release-key file exists; restore the missing file from backup".into(),
            );
        }
        (false, false) => {}
    }
    println!("[Koru] generating an encrypted Minisign release key.");
    let pair = KeyPair::generate_encrypted_keypair(None).map_err(|error| error.to_string())?;
    write_secret_new(
        &paths.secret,
        &pair
            .sk
            .to_box(Some("Koru offline release key"))
            .map_err(|error| error.to_string())?
            .to_string(),
    )?;
    write_new(
        &paths.public,
        &pair
            .pk
            .to_box()
            .map_err(|error| error.to_string())?
            .to_string(),
    )
}

fn write_secret_new(path: &Path, contents: &str) -> Result<(), String> {
    write_new(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_new(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(contents.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())
}

fn save_new_or_matching_json<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    if path.exists() {
        let existing: T = read_json(path)?;
        let existing = serde_json::to_value(existing).map_err(|error| error.to_string())?;
        let new = serde_json::to_value(value).map_err(|error| error.to_string())?;
        if existing != new {
            return Err(format!(
                "{} names another Cloudflare account; remove it deliberately before reconfiguring",
                path.display()
            ));
        }
        return Ok(());
    }
    write_new(
        path,
        &serde_json::to_string_pretty(value).map_err(|error| error.to_string())?,
    )
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn cloudflare_token() -> Result<String, String> {
    std::env::var("CLOUDFLARE_API_TOKEN")
        .map_err(|_| "set CLOUDFLARE_API_TOKEN to one scoped Cloudflare API token".into())
}

fn validate_release_version(channel: ReleaseChannel, value: &str) -> Result<(), String> {
    let parsed =
        Version::parse(value).map_err(|error| format!("invalid semantic version: {error}"))?;
    let matches = match channel {
        ReleaseChannel::Stable => parsed.pre.is_empty() && parsed.build.is_empty(),
        ReleaseChannel::Beta => parsed.pre.as_str().starts_with("beta."),
        ReleaseChannel::Nightly => parsed.pre.as_str().starts_with("nightly."),
        ReleaseChannel::Internal => false,
    };
    if matches {
        Ok(())
    } else {
        Err(format!(
            "{value} does not match the {channel} channel's version format"
        ))
    }
}

/// Record the release notes this publication used.
///
/// Never fatal, and for the same reason the index update is not: the release is
/// already live. A tree that will not commit — a hook that refuses, a rebase in
/// progress, an operator with no `user.email` — is a thing to fix afterwards,
/// not a reason to report a published release as a failure.
///
/// Nothing to commit is the ordinary case on a second publication of the same
/// version (the other platform's), and it is silent rather than an error.
fn commit_release_notes(root: &Path) {
    let pending = match git(root, &["status", "--porcelain", "--", "releases"]) {
        Ok(pending) => pending,
        Err(error) => {
            println!("[Koru] WARNING: could not check releases/ for changes ({error})");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    if let Err(error) = git(root, &["add", "--", "releases"]) {
        println!("[Koru] WARNING: the release notes were not committed ({error})");
        return;
    }
    // The pathspec is repeated here on purpose: without it this would commit
    // the whole index, including anything staged before the publish began.
    match git(
        root,
        &["commit", "-m", "Updated releases", "--", "releases"],
    ) {
        Ok(_) => println!(
            "[Koru] committed {} release descriptor path(s)",
            pending.lines().count()
        ),
        Err(error) => println!(
            "[Koru] WARNING: the release notes were staged but not committed ({error}); \
             commit releases/ by hand"
        ),
    }
}

fn require_clean_tree(root: &Path, expected: Option<&str>) -> Result<String, String> {
    let status = git(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    source::check_tree(&status, &head, expected)?;
    Ok(head)
}

fn ensure_tag(root: &Path, version: &str) -> Result<(), String> {
    let tag = format!("v{version}");
    let head = git(root, &["rev-parse", "HEAD"])?;
    let existing = git_allow_failure(root, &["rev-list", "-n", "1", &tag])?;
    if existing.is_empty() {
        let status = Command::new("git")
            .current_dir(root)
            .args(["tag", "-a", &tag, "-m", &format!("Koru {version}")])
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!("could not create tag {tag}"));
        }
    } else if existing != head {
        return Err(format!(
            "{tag} points at {existing}, not current commit {head}"
        ));
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn git_allow_failure(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The platform's own command interpreter, for the overridable hooks.
fn shell(command: &str) -> Command {
    let mut invocation = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C");
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c");
        c
    };
    append_shell_command(&mut invocation, command);
    invocation
}

#[cfg(windows)]
fn append_shell_command(invocation: &mut Command, command: &str) {
    use std::os::windows::process::CommandExt as _;

    // Normal argument encoding changes embedded quotes to backslash-quote.
    // cmd does not use that convention and treats the backslash literally.
    // Supply its outer command quotes unchanged through CreateProcess.
    let mut raw = String::new();
    raw.push(char::from(34));
    raw.push_str(command);
    raw.push(char::from(34));
    invocation.raw_arg(raw);
}

#[cfg(not(windows))]
fn append_shell_command(invocation: &mut Command, command: &str) {
    invocation.arg(command);
}

fn confirm(prompt: &str) -> Result<bool, String> {
    Ok(matches!(
        read_answer(prompt)?.as_str(),
        "y" | "Y" | "yes" | "YES"
    ))
}

fn read_answer(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(answer.trim().to_string())
}

fn workspace_root() -> Result<PathBuf, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("release tool is not inside the Koru workspace")?
        .to_path_buf();
    if root.join("Cargo.toml").is_file() {
        Ok(root)
    } else {
        Err("could not locate the Koru workspace root".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn shell_runs_a_quoted_windows_executable() {
        let command = r#""cmd" /C exit 0"#;

        assert!(shell(command).status().unwrap().success());
    }

    fn dist_local_args(args: &[&str]) -> Result<DistLocalOptions, String> {
        DistLocalOptions::parse(args.iter().map(|arg| arg.to_string()))
    }

    /// The rehearsal flags parse, and everything else is refused with the
    /// usage line — the same strictness `koru_workspace_host::cli::parse` set the
    /// precedent for: an argument that is not understood is not run.
    #[test]
    fn dist_local_parses_the_rehearsal_flags_and_refuses_the_rest() {
        let plain = dist_local_args(&[]).unwrap();
        assert_eq!(plain.version, None);
        assert_eq!((plain.build_number, plain.sign_update), (1, false));

        let signed = dist_local_args(&["0.0.3-beta.a", "--build", "100", "--sign-update"]).unwrap();
        assert_eq!(signed.version.as_deref(), Some("0.0.3-beta.a"));
        assert_eq!((signed.build_number, signed.sign_update), (100, true));

        assert!(dist_local_args(&["--build"]).is_err());
        assert!(dist_local_args(&["--build", "zero"]).is_err());
        assert!(dist_local_args(&["--build", "0"]).is_err());
        assert!(dist_local_args(&["--nope"]).is_err());
        assert!(dist_local_args(&["one-version", "two-versions"]).is_err());
    }

    #[test]
    fn internal_is_documented_for_local_packages_but_refused_by_cloud_validation() {
        assert!(DIST_LOCAL_USAGE.contains("internal"));
        assert!("internal".parse::<ReleaseChannel>().is_ok());
        // dist_local intentionally bypasses channel-shaped version validation;
        // publish calls this guard, so Internal can package but cannot upload.
        assert!(validate_release_version(ReleaseChannel::Internal, "0.0.0-m1-fault").is_err());
    }

    #[test]
    fn replacing_a_local_bundle_removes_its_whole_old_artifact_set() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("koru-local.zip");
        let signature = artifact_sidecar(&archive, ".minisig");
        let manifest = artifact_sidecar(&archive, ".update.json");
        let unrelated = dir.path().join("keep.txt");
        for path in [&archive, &signature, &manifest, &unrelated] {
            fs::write(path, "old").unwrap();
        }

        remove_local_artifact_set(&archive).unwrap();

        assert!(!archive.exists());
        assert!(!signature.exists());
        assert!(!manifest.exists());
        assert!(
            unrelated.exists(),
            "only this bundle's siblings are replaced"
        );
        remove_local_artifact_set(&archive).unwrap();
    }

    fn a_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for args in [
            &["init", "-q", "."][..],
            &["config", "user.email", "release@example.invalid"],
            &["config", "user.name", "Release"],
            // A commit hook or a signing key on the developer's machine must
            // not reach into this.
            &["config", "commit.gpgsign", "false"],
        ] {
            git(root, args).unwrap();
        }
        std::fs::create_dir_all(root.join("releases")).unwrap();
        std::fs::write(
            root.join("releases").join("old.toml"),
            "old
",
        )
        .unwrap();
        std::fs::write(
            root.join("elsewhere.txt"),
            "base
",
        )
        .unwrap();
        git(root, &["add", "-A"]).unwrap();
        git(root, &["commit", "-qm", "base"]).unwrap();
        dir
    }

    /// **A release tool must not sweep up somebody's work in progress.**
    /// An unqualified `git commit` after `git add releases` would take
    /// whatever else was staged while publication was finishing.
    #[test]
    fn committing_the_notes_takes_the_descriptors_and_nothing_else() {
        let dir = a_repo();
        let root = dir.path();
        // A descriptor this publication wrote: new, and untracked.
        std::fs::write(
            root.join("releases").join("new.toml"),
            "new
",
        )
        .unwrap();
        // And something else the operator had staged before publishing.
        std::fs::write(
            root.join("elsewhere.txt"),
            "mine
",
        )
        .unwrap();
        git(root, &["add", "elsewhere.txt"]).unwrap();

        commit_release_notes(root);

        let committed = git(root, &["show", "--stat", "--format=", "HEAD"]).unwrap();
        assert!(committed.contains("releases/new.toml"), "{committed}");
        assert!(
            !committed.contains("elsewhere.txt"),
            "the operator's staged work was committed too: {committed}"
        );
        assert!(
            git(root, &["status", "--porcelain"])
                .unwrap()
                .contains("elsewhere.txt"),
            "and it is still staged, where they left it"
        );
    }

    /// The second platform of the same version writes no new descriptor. That
    /// is the ordinary case, and it is silent rather than a failed publication.
    #[test]
    fn committing_the_notes_is_quiet_when_there_is_nothing_to_commit() {
        let dir = a_repo();
        let root = dir.path();
        let before = git(root, &["rev-parse", "HEAD"]).unwrap();
        commit_release_notes(root);
        assert_eq!(
            git(root, &["rev-parse", "HEAD"]).unwrap(),
            before,
            "no empty commit"
        );
    }

    #[test]
    fn channel_version_shapes_are_strict() {
        assert!(validate_release_version(ReleaseChannel::Stable, "1.2.3").is_ok());
        assert!(validate_release_version(ReleaseChannel::Stable, "1.2.3-beta.0").is_err());
        assert!(validate_release_version(ReleaseChannel::Beta, "1.2.4-beta.0").is_ok());
        assert!(
            validate_release_version(ReleaseChannel::Nightly, "1.2.5-nightly.20260804+abc1234")
                .is_ok()
        );
    }

    #[test]
    fn descriptor_rejects_incomplete_notes_and_renders_markdown() {
        let notes = Descriptor {
            title: "Safer & faster".into(),
            summary: vec!["One".into()],
            body: "**Done.**".into(),
        };
        notes.validate().unwrap();
        let rendered = notes.html("1.0.0", "today", &sample_download());
        assert!(rendered.contains("Safer &amp; faster"));
        assert!(rendered.contains("<strong>Done.</strong>"));
    }

    fn sample_download() -> Download<'static> {
        Download {
            file_name: "koru-windows-x86_64.zip",
            size: 45_088_768,
            sha256: "a".repeat(64).leak(),
        }
    }

    /// **The page has to offer the thing it is about.** It named a version,
    /// dated it and described what changed, with no way to get the build —
    /// which is fine for somebody who arrived through the in-app banner and a
    /// dead end for everybody who followed a link.
    #[test]
    fn the_notes_page_links_to_the_package_beside_it() {
        let rendered = Descriptor {
            title: "A release".into(),
            summary: vec!["One".into()],
            body: "Body.".into(),
        }
        .html("1.0.0", "today", &sample_download());

        // Relative, so it keeps working if the origin ever moves. An absolute
        // URL baked into an immutable page could not be corrected.
        assert!(
            rendered.contains("href=\"koru-windows-x86_64.zip\""),
            "the link is the sibling key, not an absolute URL"
        );
        assert!(!rendered.contains("href=\"https://updates.koru.do"));
        // What a person needs before clicking 43 MB, and after.
        assert!(rendered.contains("43.0 MB"));
        assert!(rendered.contains(&"a".repeat(64)));
        if cfg!(target_os = "macos") {
            // Both mac branches say this; which one is chosen depends on the
            // publishing shell's environment and is pinned separately below.
            assert!(rendered.contains("drag Koru into Applications"));
        } else {
            assert!(rendered.contains("koru.bat"));
        }
    }

    /// **Notarization shipped, so the quarantine incantation is gone.** The
    /// page used to tell a Mac reader to run
    /// `xattr -dr com.apple.quarantine /Applications/Koru.app`, which is wrong
    /// on the ordinary path and bad advice on the other one — it teaches a
    /// person to strip a security attribute by hand. Neither sentence carries
    /// it now, and neither should acquire it again.
    #[test]
    fn no_release_page_tells_a_reader_to_strip_the_quarantine() {
        for notarized in [true, false] {
            let sentence = mac_install_sentence(notarized);
            assert!(!sentence.contains("xattr"), "{sentence}");
            assert!(!sentence.contains("quarantine"), "{sentence}");
            assert!(
                sentence.contains("drag Koru into Applications"),
                "{sentence}"
            );
        }
        // A notarized build meets one ordinary confirmation and no security
        // pane; an ad-hoc one has to be allowed by hand, and says so.
        assert!(!mac_install_sentence(true).contains("Open Anyway"));
        assert!(mac_install_sentence(false).contains("Open Anyway"));
    }

    /// One question, asked once. The publish gate and the page's wording read
    /// the same two variables — they did not, and a shell exporting only the
    /// signing hook produced a page promising a notarized first open over a
    /// build that would show "Apple could not verify".
    #[test]
    fn the_page_and_the_publish_gate_ask_about_the_same_hooks() {
        assert_eq!(
            MAC_SIGNING_HOOKS,
            ["KORU_MAC_SIGN_COMMAND", "KORU_MAC_NOTARIZE_COMMAND"]
        );
    }

    /// Write a real page out so it can be opened in a browser, because the
    /// assertions above prove the link is *present* and prove nothing about
    /// whether it reads well. `KORU_NOTES_PREVIEW=<path> cargo test -p
    /// koru-release preview` — off by default, so an ordinary run writes
    /// nothing.
    #[test]
    fn preview_the_notes_page() {
        let Ok(destination) = std::env::var("KORU_NOTES_PREVIEW") else {
            return;
        };
        let rendered = Descriptor {
            title: "Everyday editing, tidied up".into(),
            summary: vec!["Documents open faster.".into()],
            body: "## What changed\n\nThe **agent** keeps Koru up to date now.\n\n\
                   - Two consent steps\n- All thirty fonts\n"
                .into(),
        }
        .html(
            "0.0.4-nightly.20260815+abc1234",
            "2026-08-15T18:32:29Z",
            &sample_download(),
        );
        fs::write(&destination, rendered).expect("write the preview");
        println!("wrote {destination}");
    }

    /// A title is escaped everywhere it appears, and it appears twice.
    #[test]
    fn the_download_button_escapes_the_version_it_names() {
        let rendered = Descriptor {
            title: "T".into(),
            summary: vec!["One".into()],
            body: "Body.".into(),
        }
        .html("1.0.0-beta.0+a<b", "today", &sample_download());
        assert!(!rendered.contains("a<b"), "the version reaches the markup");
        assert!(rendered.contains("a&lt;b"));
    }
}
