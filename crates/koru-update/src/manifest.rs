// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use url::Url;

use crate::MAX_PACKAGE_SIZE;
use koru_channel::ReleaseChannel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateTarget {
    pub channel: ReleaseChannel,
    pub platform: String,
    pub architecture: String,
}

impl UpdateTarget {
    /// The manifest's path on `updates.koru.do`, the layout every origin had
    /// before GitHub releases became one. See [`FeedLayout`].
    pub fn manifest_path(&self) -> String {
        FeedLayout::Channels.manifest_path(self)
    }

    /// Where this target's manifest lives beneath `base`, in the layout that
    /// origin uses.
    pub fn manifest_url(&self, base: &Url) -> Result<Url, String> {
        let layout = FeedLayout::of(base);
        match layout {
            FeedLayout::Channels => base
                .join(&layout.manifest_path(self))
                .map_err(|e| e.to_string()),
            // Pushed as a segment rather than joined, so a base written
            // without its trailing slash still names the release directory
            // instead of silently replacing the tag with the file name.
            FeedLayout::GitHubRelease => {
                let mut url = base.clone();
                url.path_segments_mut()
                    .map_err(|()| "the update origin cannot hold a path".to_string())?
                    .pop_if_empty()
                    .push(&layout.manifest_path(self));
                Ok(url)
            }
        }
    }
}

/// How an update origin lays out its manifests — a fact about the origin and
/// nothing else; the manifest is the same schema either way.
///
/// **The rule:** a base URL that is a GitHub release download directory —
/// `https://github.com/<owner>/<repo>/releases/download/<tag>/`, or the
/// `releases/latest/download/` alias GitHub resolves to the newest
/// non-pre-release — is [`GitHubRelease`](Self::GitHubRelease). Every other
/// origin, `updates.koru.do` and a loopback rehearsal feed among them, is
/// [`Channels`](Self::Channels). The host must be exactly `github.com` over
/// HTTPS on the default port; the same path on any other host stays
/// `Channels`, so a URL's shape alone never changes where a request goes.
///
/// GitHub's layout is flat because a release's assets are one directory with
/// no subfolders, so channel, platform and architecture all go into the file
/// name. **Trust does not move with the layout**: GitHub is only the host, and
/// the client checks the manifest's target, the package's hash and its
/// Minisign signature exactly as it does for R2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedLayout {
    /// `channels/<channel>/<platform>-<arch>.json` beneath the base.
    Channels,
    /// `<channel>-<platform>-<arch>.json` directly in the release directory.
    GitHubRelease,
}

impl FeedLayout {
    pub fn of(base: &Url) -> Self {
        let github = base.scheme() == "https"
            && base.host_str() == Some("github.com")
            && base.port().is_none()
            && base.query().is_none()
            && base.path_segments().is_some_and(|segments| {
                let mut segments: Vec<&str> = segments.collect();
                // The trailing slash is an empty last segment; no other
                // segment may be empty.
                if segments.last() == Some(&"") {
                    segments.pop();
                }
                match segments.as_slice() {
                    [owner, repo, "releases", "download", tag] => {
                        !owner.is_empty() && !repo.is_empty() && !tag.is_empty()
                    }
                    [owner, repo, "releases", "latest", "download"] => {
                        !owner.is_empty() && !repo.is_empty()
                    }
                    _ => false,
                }
            });
        if github {
            Self::GitHubRelease
        } else {
            Self::Channels
        }
    }

    /// The manifest's path relative to the base, in this layout. Public so a
    /// publisher names the asset with the same function the client fetches it
    /// with.
    pub fn manifest_path(self, target: &UpdateTarget) -> String {
        match self {
            Self::Channels => format!(
                "channels/{}/{}-{}.json",
                target.channel, target.platform, target.architecture
            ),
            Self::GitHubRelease => format!(
                "{}-{}-{}.json",
                target.channel, target.platform, target.architecture
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub channel: ReleaseChannel,
    pub platform: String,
    pub architecture: String,
    pub version: String,
    pub build_number: u64,
    pub published_at: String,
    #[serde(default)]
    pub commit: Option<String>,
    /// Where this release's source code is: the public repository at the
    /// full commit it was built from (MPL-2.0 §3.2). Optional and omitted
    /// when absent, so manifests written before it existed still parse and
    /// an older reader ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub minimum_updater_version: Option<u32>,
    #[serde(default)]
    pub minimum_supported_build: Option<u64>,
    #[serde(default)]
    pub required: bool,
    pub release_notes: ReleaseNotes,
    pub package: UpdatePackage,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseNotes {
    pub title: String,
    pub summary: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdatePackage {
    pub url: String,
    #[serde(default)]
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub signature: String,
    pub format: PackageFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageFormat {
    Zip,
    #[serde(rename = "tar.gz")]
    TarGz,
    AppImage,
    /// An Apple disk image: `Koru.app` plus an Applications symlink, the
    /// drag-install every mac user already knows.
    Dmg,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl UpdateManifest {
    pub fn parse_and_validate(bytes: &[u8], target: &UpdateTarget) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|e| ManifestError(format!("manifest JSON is invalid: {e}")))?;
        manifest.validate(target)?;
        Ok(manifest)
    }

    pub fn validate(&self, target: &UpdateTarget) -> Result<(), ManifestError> {
        reject(self.schema_version != 1, "unsupported manifest schema")?;
        reject(
            self.channel != target.channel,
            "manifest channel does not match request",
        )?;
        reject(
            self.platform != target.platform,
            "manifest platform does not match this build",
        )?;
        reject(
            self.architecture != target.architecture,
            "manifest architecture does not match this build",
        )?;
        reject(self.version.trim().is_empty(), "version is empty")?;
        reject(
            self.build_number == 0,
            "build number must be greater than zero",
        )?;
        reject(self.published_at.trim().is_empty(), "published_at is empty")?;
        reject(
            self.release_notes.title.trim().is_empty(),
            "release-note title is empty",
        )?;
        reject(
            self.release_notes.summary.is_empty(),
            "release-note summary is empty",
        )?;
        reject(
            self.release_notes
                .summary
                .iter()
                .any(|line| line.trim().is_empty()),
            "release-note summary contains an empty item",
        )?;
        if let Some(url) = &self.release_notes.url {
            require_https(url, "release-note URL")?;
        }
        require_https(&self.package.url, "package URL")?;
        reject(
            self.package.file_name.trim().is_empty(),
            "package file name is empty",
        )?;
        reject(
            Path::new(&self.package.file_name)
                .file_name()
                .and_then(|v| v.to_str())
                != Some(self.package.file_name.as_str()),
            "package file name must not contain a path",
        )?;
        reject(self.package.size == 0, "package size is zero")?;
        reject(
            self.package.size > MAX_PACKAGE_SIZE,
            "package exceeds Koru's size limit",
        )?;
        reject(
            !valid_sha256(&self.package.sha256),
            "package SHA-256 is malformed",
        )?;
        reject(
            self.package.signature.trim().is_empty(),
            "package signature is missing",
        )?;
        // The format follows the platform: Windows installs from the zip the
        // updater already extracts; macOS ships a disk image (zip is still
        // legal there — it is what pre-dmg releases published); Linux ships
        // a gzipped tarball, which carries Unix modes natively. The updater
        // chooses its extractor by platform, so a format outside the
        // platform's set is a package nothing on that machine can open.
        let format_fits = match self.platform.as_str() {
            "macos" => matches!(self.package.format, PackageFormat::Dmg | PackageFormat::Zip),
            "linux" => matches!(self.package.format, PackageFormat::TarGz),
            _ => matches!(self.package.format, PackageFormat::Zip),
        };
        reject(
            !format_fits,
            "package format is unsupported on this platform",
        )?;
        Ok(())
    }
}

fn require_https(value: &str, field: &str) -> Result<(), ManifestError> {
    let url = Url::parse(value).map_err(|e| ManifestError(format!("{field} is invalid: {e}")))?;
    reject(url.scheme() != "https", &format!("{field} must use HTTPS"))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn reject(condition: bool, message: &str) -> Result<(), ManifestError> {
    if condition {
        Err(ManifestError(message.to_string()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> UpdateTarget {
        UpdateTarget {
            channel: ReleaseChannel::Stable,
            platform: "windows".into(),
            architecture: "x86_64".into(),
        }
    }

    fn manifest() -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            channel: ReleaseChannel::Stable,
            platform: "windows".into(),
            architecture: "x86_64".into(),
            version: "1.2.3".into(),
            build_number: 42,
            published_at: "2026-08-04T20:00:00Z".into(),
            commit: Some("abc1234".into()),
            source: None,
            minimum_updater_version: Some(1),
            minimum_supported_build: None,
            required: false,
            release_notes: ReleaseNotes {
                title: "Better".into(),
                summary: vec!["Safer updates".into()],
                url: Some("https://updates.koru.do/releases/1.2.3/release-notes.html".into()),
            },
            package: UpdatePackage {
                url: "https://updates.koru.do/releases/1.2.3/koru-windows-x86_64.zip".into(),
                file_name: "koru-windows-x86_64.zip".into(),
                size: 100,
                sha256: "a".repeat(64),
                signature: "signature".into(),
                format: PackageFormat::Zip,
            },
        }
    }

    /// `source` is additive: absent from an old manifest, omitted when unset,
    /// and carried through when a release names its commit.
    #[test]
    fn the_source_link_is_optional_in_both_directions() {
        let old = serde_json::to_value(manifest()).unwrap();
        assert!(old.get("source").is_none(), "unset is omitted, not null");
        let parsed: UpdateManifest = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.source, None);

        let mut named = manifest();
        named.source = Some(
            "https://github.com/koruworks/mono/tree/0123456789abcdef0123456789abcdef01234567"
                .into(),
        );
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(
            serde_json::from_str::<UpdateManifest>(&json).unwrap(),
            named
        );
    }

    #[test]
    fn of_this_build_maps_flavor_strings() {
        assert_eq!(
            ReleaseChannel::from_build_channel("nightly"),
            ReleaseChannel::Nightly
        );
        assert_eq!(
            ReleaseChannel::from_build_channel("beta"),
            ReleaseChannel::Beta
        );
        assert_eq!(
            ReleaseChannel::from_build_channel("stable"),
            ReleaseChannel::Stable
        );
        assert_eq!(
            ReleaseChannel::from_build_channel("dev"),
            ReleaseChannel::Internal
        );
        assert_eq!(
            ReleaseChannel::from_build_channel("internal"),
            ReleaseChannel::Internal
        );
        assert_eq!(
            ReleaseChannel::from_build_channel("Nightly"),
            ReleaseChannel::Nightly
        );
    }

    #[test]
    fn internal_is_only_choosable_on_internal_binaries() {
        // This test binary is internal (no KORU_RELEASE_CHANNEL at compile).
        assert!(ReleaseChannel::this_build_is_internal());
        assert!(ReleaseChannel::choosable().contains(&ReleaseChannel::Internal));
        assert_eq!(ReleaseChannel::default(), ReleaseChannel::Internal);
    }

    #[test]
    fn a_complete_matching_manifest_is_valid() {
        manifest().validate(&target()).unwrap();
    }

    #[test]
    fn wrong_target_and_insecure_urls_are_rejected() {
        let mut m = manifest();
        m.architecture = "aarch64".into();
        assert!(m.validate(&target()).is_err());
        m.architecture = "x86_64".into();
        m.package.url = "http://updates.koru.do/file.zip".into();
        assert!(m.validate(&target()).is_err());
    }

    #[test]
    fn malformed_hash_size_signature_and_format_are_rejected() {
        let mut m = manifest();
        m.package.sha256 = "ABC".into();
        assert!(m.validate(&target()).is_err());
        m.package.sha256 = "a".repeat(64);
        m.package.size = 0;
        assert!(m.validate(&target()).is_err());
        m.package.size = 1;
        m.package.signature.clear();
        assert!(m.validate(&target()).is_err());
        m.package.signature = "sig".into();
        m.package.format = PackageFormat::TarGz;
        assert!(m.validate(&target()).is_err());
    }

    /// The format follows the platform: a mac manifest may carry a disk
    /// image (or the zip pre-dmg releases published); Windows takes only the
    /// zip its updater extracts.
    #[test]
    fn the_package_format_follows_the_platform() {
        let mut m = manifest();
        m.platform = "macos".into();
        m.package.format = PackageFormat::Dmg;
        let mac = UpdateTarget {
            channel: ReleaseChannel::Stable,
            platform: "macos".into(),
            architecture: "x86_64".into(),
        };
        m.validate(&mac).unwrap();
        m.package.format = PackageFormat::Zip;
        m.validate(&mac).unwrap();
        m.package.format = PackageFormat::Dmg;
        m.platform = "windows".into();
        assert!(m.validate(&target()).is_err());
    }

    fn linux() -> UpdateTarget {
        UpdateTarget {
            channel: ReleaseChannel::Nightly,
            platform: "linux".into(),
            architecture: "x86_64".into(),
        }
    }

    /// Linux takes the tarball and only the tarball; nowhere else takes it.
    #[test]
    fn a_tarball_is_a_linux_package_and_nobody_elses() {
        let mut m = manifest();
        m.channel = ReleaseChannel::Nightly;
        m.platform = "linux".into();
        m.package.format = PackageFormat::TarGz;
        m.validate(&linux()).unwrap();
        for other in [
            PackageFormat::Zip,
            PackageFormat::Dmg,
            PackageFormat::AppImage,
        ] {
            m.package.format = other;
            assert!(m.validate(&linux()).is_err(), "{other:?} on linux");
        }

        let mut windows = manifest();
        windows.package.format = PackageFormat::TarGz;
        assert!(windows.validate(&target()).is_err());
        let mac = UpdateTarget {
            channel: ReleaseChannel::Stable,
            platform: "macos".into(),
            architecture: "aarch64".into(),
        };
        let mut on_mac = manifest();
        on_mac.platform = "macos".into();
        on_mac.architecture = "aarch64".into();
        on_mac.package.format = PackageFormat::TarGz;
        assert!(on_mac.validate(&mac).is_err());
    }

    /// The wire spelling is the one `koru-release` writes: `"tar.gz"`.
    #[test]
    fn the_tarball_format_is_spelled_tar_gz_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&PackageFormat::TarGz).unwrap(),
            "\"tar.gz\""
        );
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    #[test]
    fn the_r2_origin_keeps_its_channel_folders() {
        let base = url("https://updates.koru.do/");
        assert_eq!(FeedLayout::of(&base), FeedLayout::Channels);
        assert_eq!(
            linux().manifest_url(&base).unwrap().as_str(),
            "https://updates.koru.do/channels/nightly/linux-x86_64.json"
        );
        assert_eq!(
            target().manifest_path(),
            "channels/stable/windows-x86_64.json",
            "the existing path is unchanged"
        );
    }

    #[test]
    fn a_github_release_directory_is_flat() {
        let expected =
            "https://github.com/koruworks/mono/releases/download/nightly/nightly-linux-x86_64.json";
        for base in [
            "https://github.com/koruworks/mono/releases/download/nightly/",
            // A base baked without its trailing slash still names the tag.
            "https://github.com/koruworks/mono/releases/download/nightly",
        ] {
            let base = url(base);
            assert_eq!(FeedLayout::of(&base), FeedLayout::GitHubRelease);
            assert_eq!(linux().manifest_url(&base).unwrap().as_str(), expected);
        }
        let latest = url("https://github.com/koruworks/mono/releases/latest/download/");
        assert_eq!(FeedLayout::of(&latest), FeedLayout::GitHubRelease);
        assert_eq!(
            target().manifest_url(&latest).unwrap().as_str(),
            "https://github.com/koruworks/mono/releases/latest/download/stable-windows-x86_64.json"
        );
    }

    /// Only GitHub itself, and only a release directory, is flat. Everything
    /// else keeps today's layout — including look-alikes, which is what keeps
    /// the URL's shape from deciding anything on its own.
    #[test]
    fn nothing_but_a_github_release_directory_is_flat() {
        for base in [
            "http://github.com/koruworks/mono/releases/download/nightly/",
            "https://github.com:8443/koruworks/mono/releases/download/nightly/",
            "https://www.github.com/koruworks/mono/releases/download/nightly/",
            "https://github.com.example/koruworks/mono/releases/download/nightly/",
            "https://example.com/koruworks/mono/releases/download/nightly/",
            "https://github.com/koruworks/mono/releases/tag/nightly/",
            "https://github.com/koruworks/mono/releases/download/",
            "https://github.com/koruworks/mono/releases/download/nightly/extra/",
            "https://github.com//mono/releases/download/nightly/",
            "https://github.com/koruworks/mono/releases/download/nightly/?x=1",
            "https://github.com/koruworks/mono/",
            "http://127.0.0.1:8080/",
        ] {
            assert_eq!(FeedLayout::of(&url(base)), FeedLayout::Channels, "{base}");
        }
    }
}
