// SPDX-License-Identifier: MPL-2.0

use super::*;

fn args(values: &[&str]) -> Result<DistGithubOptions, String> {
    DistGithubOptions::parse(values.iter().map(|value| value.to_string()))
}

const FULL: &[&str] = &[
    "--build",
    "202610241530",
    "--notes",
    "releases/nightly/2026-10-24.toml",
    "--key",
    "/run/secrets/minisign.key",
    "--download-base",
    "https://github.com/koruworks/mono/releases/download/nightly",
    "--out",
    "dist/nightly",
];

#[test]
fn dist_github_parses_every_flag_and_refuses_the_rest() {
    let options = args(FULL).unwrap();
    assert_eq!(options.build_number, 202_610_241_530);
    assert_eq!(
        options.released.to_rfc3339_opts(SecondsFormat::Secs, true),
        "2026-10-24T15:30:00Z"
    );
    assert_eq!(
        options.download_base, "https://github.com/koruworks/mono/releases/download/nightly/",
        "the base gains its trailing slash"
    );
    assert_eq!(options.notes, Path::new("releases/nightly/2026-10-24.toml"));
    assert!(!options.key_password_stdin);
    assert_eq!(options.version, None);

    let mut with_extras = FULL.to_vec();
    with_extras.extend([
        "--key-password-stdin",
        "--version",
        "0.1.4-nightly.20261024+abc1234",
    ]);
    let options = args(&with_extras).unwrap();
    assert!(options.key_password_stdin);
    assert_eq!(
        options.version.as_deref(),
        Some("0.1.4-nightly.20261024+abc1234")
    );

    for missing in ["--build", "--notes", "--key", "--download-base", "--out"] {
        let position = FULL.iter().position(|arg| *arg == missing).unwrap();
        let mut without = FULL.to_vec();
        without.drain(position..position + 2);
        let error = args(&without).unwrap_err();
        assert!(error.contains(&format!("{missing} is required")), "{error}");
    }
    let mut twice = FULL.to_vec();
    twice.extend(["--out", "elsewhere"]);
    assert!(args(&twice).unwrap_err().contains("--out given twice"));
    let mut unknown = FULL.to_vec();
    unknown.push("--yes");
    assert!(
        args(&unknown)
            .unwrap_err()
            .contains("unexpected argument: --yes")
    );
    assert!(
        args(&["--build"])
            .unwrap_err()
            .contains("--build needs a value")
    );
}

/// N11: the build number is the build's UTC minute, and a typo is refused
/// rather than becoming a number no later nightly can be newer than.
#[test]
fn the_build_number_is_a_utc_minute() {
    assert_eq!(parse_build_time("202610241530").unwrap().0, 202_610_241_530);
    for bad in [
        "",
        "20261024153",
        "2026102415300",
        "202613241530",
        "202610241560",
        "2026-10-2415",
        "+20261024153",
    ] {
        assert!(parse_build_time(bad).is_err(), "{bad:?} was accepted");
    }
    // Above every build the update origin has handed out (101 when the
    // nightly moved to GitHub), by many orders of magnitude.
    assert!(parse_build_time("202610241530").unwrap().0 > 1_000_000);
}

#[test]
fn the_download_base_is_a_plain_https_directory() {
    assert_eq!(
        download_base_url("https://example.invalid/a/").unwrap(),
        "https://example.invalid/a/"
    );
    for bad in [
        "http://example.invalid/a",
        "file:///tmp/a",
        "https://example.invalid/a?x=1",
        "https://example.invalid/a#b",
        "not a url",
    ] {
        assert!(download_base_url(bad).is_err(), "{bad} was accepted");
    }
}

/// The version's `+` would be renamed by GitHub on upload, and the manifest
/// would then name a file that is not there.
#[test]
fn asset_names_are_what_github_keeps() {
    assert_eq!(
        github_asset_name("koru-0.1.4-nightly.20261024+abc1234-linux-x86_64.tar.gz"),
        "koru-0.1.4-nightly.20261024.abc1234-linux-x86_64.tar.gz"
    );
    assert_eq!(github_asset_name("a b_c~d"), "a.b_c.d");
    assert_eq!(
        feed_name(ReleaseChannel::Nightly, "linux-x86_64"),
        "nightly-linux-x86_64.json"
    );
}

fn notes() -> Descriptor {
    Descriptor {
        title: "Linux, previewed".into(),
        summary: vec!["Koru runs on Linux.".into(), "The tray works.".into()],
        body: "## Linux\n\nKoru builds and runs on X11.\n".into(),
    }
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn build<'a>(notes: &'a Descriptor, target: &'a str) -> GithubBuild<'a> {
    GithubBuild {
        channel: ReleaseChannel::Nightly,
        target,
        version: "0.1.4-nightly.20261024+0123456",
        build_number: 202_610_241_530,
        released_at: "2026-10-24T15:30:00Z",
        commit: COMMIT,
        notes,
        download_base: "https://github.com/koruworks/mono/releases/download/nightly/",
        asset: "koru-0.1.4-nightly.20261024.0123456-linux-x86_64.tar.gz",
        size: 52_000_000,
        sha256: "ab12",
        signature: "untrusted comment: signature\nRWQ...\n",
    }
}

/// The pre-build check uses placeholders for what only the package knows;
/// they must pass every check except the ones the real values settle. A
/// Windows zip is used because `validate` refuses a Linux `tar.gz` until the
/// koru-update change that accepts it lands.
#[test]
fn the_pre_build_placeholders_pass_the_clients_check() {
    let notes = notes();
    let mut placeholder = build(&notes, "windows-x86_64");
    placeholder.asset = "koru-0.1.4-nightly.20261024.0123456-windows-x86_64.zip";
    placeholder.size = 1;
    placeholder.sha256 = PLACEHOLDER_SHA256;
    placeholder.signature = "placeholder";
    client_accepts(&github_manifest(&placeholder)).unwrap();
}

/// What the installed app reads from GitHub (NIGHTLY §6.1). Asserted field by
/// field rather than through `UpdateManifest::validate`, which refuses
/// `tar.gz` on Linux until the koru-update change that accepts it lands;
/// `dist-github` itself does validate, so it refuses until then.
#[test]
fn the_manifest_points_every_url_at_the_github_release() {
    let notes = notes();
    let manifest = github_manifest(&build(&notes, "linux-x86_64"));
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.channel, ReleaseChannel::Nightly);
    assert_eq!(
        (manifest.platform.as_str(), manifest.architecture.as_str()),
        ("linux", "x86_64")
    );
    assert_eq!(manifest.version, "0.1.4-nightly.20261024+0123456");
    assert_eq!(manifest.build_number, 202_610_241_530);
    assert_eq!(manifest.published_at, "2026-10-24T15:30:00Z");
    assert_eq!(manifest.commit.as_deref(), Some("0123456"));
    assert_eq!(
        manifest.source.as_deref(),
        Some("https://github.com/koruworks/mono/tree/0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(manifest.release_notes.title, "Linux, previewed");
    assert_eq!(manifest.release_notes.summary, notes.summary);
    assert_eq!(
        manifest.release_notes.url.as_deref(),
        Some("https://github.com/koruworks/mono/releases/tag/nightly")
    );
    assert_eq!(
        manifest.package.url,
        "https://github.com/koruworks/mono/releases/download/nightly/\
         koru-0.1.4-nightly.20261024.0123456-linux-x86_64.tar.gz"
    );
    assert_eq!(
        manifest.package.file_name,
        "koru-0.1.4-nightly.20261024.0123456-linux-x86_64.tar.gz"
    );
    assert_eq!(manifest.package.format, koru_update::PackageFormat::TarGz);
    assert_eq!(manifest.package.size, 52_000_000);
    assert_eq!(manifest.minimum_updater_version, Some(1));
    assert!(!manifest.required);

    // And it is JSON the client parses back unchanged.
    let json = serde_json::to_vec(&manifest).unwrap();
    let parsed: UpdateManifest = serde_json::from_slice(&json).unwrap();
    assert_eq!(parsed, manifest);
    assert!(
        String::from_utf8(json)
            .unwrap()
            .contains("\"format\":\"tar.gz\"")
    );
}

#[test]
fn the_release_body_is_the_notes_then_the_build_then_the_preview_note() {
    let notes = notes();
    let body = release_body(&build(&notes, "linux-x86_64"));
    let title = body.find("## Linux, previewed").unwrap();
    let summary = body
        .find("- Koru runs on Linux.\n- The tray works.\n")
        .unwrap();
    let full = body.find("Koru builds and runs on X11.").unwrap();
    let footer = body.find("\n---\n").unwrap();
    let preview = body.find("**A preview** — X11 only").unwrap();
    assert!(
        title < summary && summary < full && full < footer && footer < preview,
        "{body}"
    );
    assert!(body.contains("`0.1.4-nightly.20261024+0123456`"));
    assert!(body.contains("build 202610241530"));
    assert!(body.contains(
        "[`0123456`](https://github.com/koruworks/mono/commit/0123456789abcdef0123456789abcdef01234567)"
    ));
    assert!(body.contains("no dictation yet, no Koru Cloud yet"));

    // The Linux limits are Linux's; another platform's page does not claim them.
    let other = release_body(&build(&notes, "windows-x86_64"));
    assert!(!other.contains("A preview"), "{other}");
}

#[test]
fn notes_files_are_the_descriptor_format_and_text_only() {
    let good = "title = \"T\"\nsummary = [\"One.\"]\nbody = \"\"\"\n## A\n\nText and a [link](https://koru.do).\n\"\"\"\n";
    assert_eq!(parse_notes(good).unwrap().title, "T");

    for (bad, why) in [
        ("title = \"T\"\nsummary = [\"One.\"]\n", "missing body"),
        (
            "title = \"\"\nsummary = [\"One.\"]\nbody = \"B\"\n",
            "blank title",
        ),
        (
            "title = \"T\"\nsummary = []\nbody = \"B\"\n",
            "empty summary",
        ),
        (
            "title = \"T\"\nsummary = [\"One.\"]\nbody = \"![demo](demo.gif)\"\n",
            "image",
        ),
        (
            "title = \"T\"\nsummary = [\"One.\"]\nbody = \"\"\"\n<video src=\"a.webm\"></video>\n\"\"\"\n",
            "video",
        ),
        (
            "title = \"T\"\nsummary = [\"One.\"]\nbody = \"See <img src=x>.\"\n",
            "inline html",
        ),
        ("not toml", "not toml"),
    ] {
        assert!(parse_notes(bad).is_err(), "{why} was accepted");
    }
}

#[test]
fn notes_files_are_named_by_date_then_number() {
    let day = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
    assert_eq!(
        notes_key("releases/nightly/2026-10-24.toml"),
        Some((day, 1))
    );
    assert_eq!(
        notes_key("releases/nightly/2026-10-24-3.toml"),
        Some((day, 3))
    );
    for not_notes in [
        "releases/nightly/README.md",
        "releases/nightly/2026-10-24-1.toml",
        "releases/nightly/2026-10-24-02.toml",
        "releases/nightly/2026-10-24-x.toml",
        "releases/nightly/2026-13-01.toml",
        "releases/nightly/sub/2026-10-24.toml",
        "releases/2026-10-24.toml",
        "releases/0.1.3.toml",
    ] {
        assert_eq!(notes_key(not_notes), None, "{not_notes}");
    }
}

#[test]
fn a_second_and_third_build_that_day_take_the_next_number() {
    let day = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
    let mut existing = vec![
        "releases/nightly/README.md".to_string(),
        "releases/nightly/2026-10-23.toml".to_string(),
    ];
    assert_eq!(
        next_notes_path(day, &existing),
        "releases/nightly/2026-10-24.toml"
    );
    existing.push("releases/nightly/2026-10-24.toml".into());
    assert_eq!(
        next_notes_path(day, &existing),
        "releases/nightly/2026-10-24-2.toml"
    );
    existing.push("releases/nightly/2026-10-24-2.toml".into());
    assert_eq!(
        next_notes_path(day, &existing),
        "releases/nightly/2026-10-24-3.toml"
    );
}

#[test]
fn ci_takes_exactly_the_one_notes_file_the_push_added() {
    let added = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| path.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        select_notes(
            false,
            &added(&[
                "releases/nightly/2026-10-24.toml",
                "releases/nightly/README.md"
            ]),
            &[]
        )
        .unwrap(),
        "releases/nightly/2026-10-24.toml"
    );
    let none = select_notes(false, &added(&["releases/nightly/README.md"]), &[]).unwrap_err();
    assert!(none.contains("no notes file was added"), "{none}");
    let several = select_notes(
        false,
        &added(&[
            "releases/nightly/2026-10-24.toml",
            "releases/nightly/2026-10-24-2.toml",
        ]),
        &[],
    )
    .unwrap_err();
    assert!(several.contains("2 notes files were added"), "{several}");
    assert!(several.contains("2026-10-24-2.toml"), "{several}");
}

/// The tag's first push has no `before` to diff against: the newest file is
/// the one, by date and then by number — never by plain string order, which
/// puts `-10` before `-2`.
#[test]
fn the_first_push_takes_the_newest_file() {
    assert!(is_zero_commit("0000000000000000000000000000000000000000"));
    assert!(!is_zero_commit("0000000000000000000000000000000000000001"));
    assert!(!is_zero_commit(""));
    let present: Vec<String> = [
        "releases/nightly/README.md",
        "releases/nightly/2026-10-24-10.toml",
        "releases/nightly/2026-10-24-2.toml",
        "releases/nightly/2026-10-23-4.toml",
    ]
    .iter()
    .map(|path| path.to_string())
    .collect();
    assert_eq!(
        select_notes(true, &[], &present).unwrap(),
        "releases/nightly/2026-10-24-10.toml"
    );
    assert!(select_notes(true, &[], &present[..1]).is_err());
}

#[test]
fn the_published_tag_is_the_baseline_when_this_clone_has_it() {
    assert_eq!(
        baseline(Some("local"), Some("remote"), true).unwrap(),
        Baseline::Since {
            sha: "remote".into(),
            from: "origin's nightly tag"
        }
    );
    let missing = baseline(Some("local"), Some("remote"), false).unwrap_err();
    assert!(
        missing.contains("git fetch --force origin tag nightly"),
        "{missing}"
    );
    assert_eq!(
        baseline(Some("local"), None, false).unwrap(),
        Baseline::Since {
            sha: "local".into(),
            from: "the local nightly tag"
        }
    );
    assert_eq!(baseline(None, None, false).unwrap(), Baseline::Fallback);
    assert_eq!(Baseline::Fallback.log_args(), ["-50", "HEAD"]);
}

#[test]
fn an_annotated_remote_tag_names_its_commit() {
    let lightweight = "1111111111111111111111111111111111111111\trefs/tags/nightly\n";
    assert_eq!(
        remote_tag_commit(lightweight).as_deref(),
        Some("1111111111111111111111111111111111111111")
    );
    let annotated = "2222222222222222222222222222222222222222\trefs/tags/nightly\n\
                     3333333333333333333333333333333333333333\trefs/tags/nightly^{}\n";
    assert_eq!(
        remote_tag_commit(annotated).as_deref(),
        Some("3333333333333333333333333333333333333333")
    );
    assert_eq!(remote_tag_commit(""), None);
}

/// The key a package is signed with and the public key it bakes in are one
/// key: the public half is derived, and the signature verifies against it.
#[test]
fn an_unencrypted_key_file_signs_what_its_derived_public_key_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let key = dir.path().join("minisign.key");
    fs::write(&key, pair.sk.to_box(None).unwrap().to_string()).unwrap();
    let (secret, public) = open_key_file(&key, false).unwrap();
    assert_eq!(public, pair.pk.to_box().unwrap().to_string());

    let archive = dir.path().join("package.tar.gz");
    fs::write(&archive, b"package bytes").unwrap();
    let signature = sign_archive(&secret, &archive, &public, "0.1.4-nightly.1+abc").unwrap();
    verify_signature(&archive, &signature, &public).unwrap();
    fs::write(&archive, b"tampered bytes").unwrap();
    assert!(verify_signature(&archive, &signature, &public).is_err());
}

#[test]
fn an_encrypted_key_without_a_password_is_refused_rather_than_prompted() {
    let dir = tempfile::tempdir().unwrap();
    let pair = minisign::KeyPair::generate_encrypted_keypair(Some("pw".into())).unwrap();
    let key = dir.path().join("minisign.key");
    fs::write(&key, pair.sk.to_box(None).unwrap().to_string()).unwrap();
    let error = open_key_file(&key, false).unwrap_err();
    assert!(error.contains("--key-password-stdin"), "{error}");
}
