// SPDX-License-Identifier: MPL-2.0

use super::*;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
}

/// **The macOS notices describe what a Mac package builds.** `package::build`
/// builds the channel binaries, the notification owner on a Mac, and the
/// updater; the list here is what `notices macos` walks from any host. On
/// Windows the channel list also holds the COM notification server, which a
/// Mac never builds.
#[test]
fn the_macos_packages_are_what_a_mac_package_builds() {
    let mut expected: Vec<&str> = crate::package::CHANNEL_BINARIES
        .iter()
        .map(|(package, _)| *package)
        .filter(|package| *package != "koru-com-notification")
        .chain(["koru-notification-owner", "koru-updater"])
        // A Linux host's channel list has no voice worker until Linux rung
        // 3; a Mac package carries one.
        .chain(cfg!(target_os = "linux").then_some("koru-voice-worker"))
        .collect();
    expected.sort_unstable();
    let mut packages = MACOS_PACKAGES.to_vec();
    packages.sort_unstable();
    assert_eq!(packages, expected);
}

#[test]
fn a_package_resolved_lists_its_pins() {
    let resolved = r#"{
      "originHash" : "x",
      "pins" : [
        {
          "identity" : "swift-protobuf",
          "kind" : "remoteSourceControl",
          "location" : "https://github.com/apple/swift-protobuf.git",
          "state" : { "revision" : "55d7a1cc", "version" : "1.38.1" }
        },
        {
          "identity" : "on-a-branch",
          "kind" : "remoteSourceControl",
          "location" : "https://example.com/b.git",
          "state" : { "branch" : "main", "revision" : "abc" }
        }
      ],
      "version" : 3
    }"#;
    assert_eq!(
        pins(resolved).unwrap(),
        [
            Pin {
                identity: "swift-protobuf".into(),
                location: "https://github.com/apple/swift-protobuf.git".into(),
                revision: "55d7a1cc".into(),
                version: Some("1.38.1".into()),
            },
            Pin {
                identity: "on-a-branch".into(),
                location: "https://example.com/b.git".into(),
                revision: "abc".into(),
                version: None,
            },
        ]
    );
    assert!(pins("{}").is_err());
    assert!(pins(r#"{"pins": [{"identity": "x"}]}"#).is_err());
}

/// **Every Swift package the app links is attributed, at the revision it is
/// pinned to, and only for iOS.** Xcode resolves them, so `cargo metadata`
/// cannot see them; each is a `licences/bundled/` file. A package added to
/// the project (or moved to another revision) without its notice here fails
/// this, and a notice marked for another platform would put an iOS library
/// in the desktop's or Android's notices.
#[test]
fn every_swift_package_the_app_links_is_attributed_for_ios_only() {
    let resolved = include_str!(
        "../../../../../apps/ios/KoruMobile.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved"
    );
    let pins = pins(resolved).expect("Package.resolved parses");
    assert!(!pins.is_empty(), "{PACKAGE_RESOLVED} pins nothing");
    let bundled = read_bundled(&root().join(BUNDLED_DIR)).expect("bundled files parse");
    assert_eq!(unattributed(&pins, &bundled), Vec::<String>::new());
    for pin in pins {
        let component = bundled
            .iter()
            .find(|component| {
                component
                    .field("Source")
                    .is_some_and(|source| source.contains(&pin.revision))
            })
            .expect("attributed");
        for other in ["windows", "macos", "linux", "android"] {
            assert!(
                !component.applies_to(other),
                "{}'s notice applies to {other}",
                pin.identity
            );
        }
    }
}

/// A pin moved to another revision or version, or a notice marked for
/// another platform, leaves the package unattributed, and the build refuses.
#[test]
fn a_swift_package_without_its_notice_is_unattributed() {
    let notice = |platforms: &str, version: &str| {
        crate::notices::parse_bundled(&format!(
            "Component: P\nVersion: {version}\nSource: https://example.com/p/tree/abc123\n\
             Files: KoruMobile\nLicense: MIT\nPlatforms: {platforms}\n---\ntext\n"
        ))
        .unwrap()
    };
    let pin = |revision: &str, version: &str| Pin {
        identity: "p".into(),
        location: "https://example.com/p.git".into(),
        revision: revision.into(),
        version: Some(version.into()),
    };
    let ios = [notice("ios", "1.0.0")];
    assert!(unattributed(&[pin("abc123", "1.0.0")], &ios).is_empty());
    assert_eq!(unattributed(&[pin("def456", "1.1.0")], &ios), ["p def456"]);
    assert_eq!(unattributed(&[pin("abc123", "1.1.0")], &ios), ["p abc123"]);
    assert_eq!(
        unattributed(&[pin("abc123", "1.0.0")], &[notice("android", "1.0.0")]),
        ["p abc123"]
    );
}

/// **Nothing the iOS app does not contain is attributed in it.** The app
/// links no sherpa-onnx, whisper.cpp or koru-blocks and bundles no font, so
/// the desktop's and Android's components must all be marked for their own
/// platforms.
#[test]
fn only_what_the_app_carries_is_attributed_on_ios() {
    let bundled = read_bundled(&root().join(BUNDLED_DIR)).expect("bundled files parse");
    let on_ios: Vec<&str> = bundled
        .iter()
        .filter(|component| component.applies_to("ios"))
        .filter_map(|component| component.field("Component"))
        .collect();
    assert_eq!(on_ios, ["SwiftProtobuf (apple/swift-protobuf)"]);
}

/// **The app reads the files this writes.** `OpenSourceLicensesScreen.swift`
/// names each file of the bundle's `licenses/`; a name changed on one side
/// would show "This build carries no license files." on a build that does.
#[test]
fn the_ios_screen_reads_the_files_the_generator_writes() {
    let swift = include_str!("../../../../../apps/ios/KoruMobile/OpenSourceLicensesScreen.swift");
    assert!(swift.contains(r#""licenses""#), "the directory");
    for name in IOS_FILES {
        assert!(swift.contains(&format!("\"{name}\"")), "{name}");
    }
}

/// The four files, in order, with Koru's own texts and the source link.
#[test]
fn the_ios_licenses_are_koru_s_own_texts_the_source_and_the_notices() {
    let source = "https://github.com/koruworks/mono/tree/0000000";
    let files = ios_files(root(), source, "the notices".to_owned()).unwrap();
    let names: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, IOS_FILES);
    assert_eq!(files[0].1, fs::read(root().join("LICENSE")).unwrap());
    assert_eq!(files[1].1, fs::read(root().join("NOTICE")).unwrap());
    assert_eq!(files[2].1, format!("{source}\n").into_bytes());
    assert_eq!(files[3].1, b"the notices");
}

/// A build names where its source is, as a link, or it is not built.
#[test]
fn the_ios_notices_need_a_source_link() {
    assert!(check_source("https://github.com/koruworks/mono/tree/abc").is_ok());
    assert!(check_source("").is_err());
    assert!(check_source("abc123").is_err());
    let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    // Refused before any `cargo metadata` runs.
    assert!(run_ios(root(), args(&["--dir", "x"]).into_iter()).is_err());
    assert!(run_ios(root(), args(&["--dir", "x", "--source", "abc"]).into_iter()).is_err());
    assert!(run_ios(root(), args(&["--nonsense"]).into_iter()).is_err());
    assert!(run_macos(root(), args(&[]).into_iter()).is_err());
}

/// **The macOS crates, for this tree**: each Mac triple's graph, refused the
/// same way a release is if a crate has no record. Runs `cargo metadata`, so
/// it is ignored by default; run it by hand on any host:
///
/// `cargo test -p koru-release generates_the_macos_notices_for_this_tree -- --ignored`
#[test]
#[ignore = "runs cargo metadata and reads the registry; run by hand"]
fn generates_the_macos_notices_for_this_tree() {
    for triple in MACOS_TRIPLES {
        let text = generate(root(), &macos_build(vec![(*triple).to_owned()]), &[])
            .unwrap_or_else(|error| panic!("{triple}: {error}"));
        for expected in ["Part 1", "Part 2", "OFL", "objc2", "loro"] {
            assert!(text.contains(expected), "{triple}: {expected}");
        }
        for absent in ["INFERRED", "SwiftProtobuf", "Inter-Regular.ttf"] {
            assert!(!text.contains(absent), "{triple}: {absent}");
        }
    }
}

/// **The iOS crates, for this tree**: the staticlib's graph for the device
/// triple, refused as a Release build refuses. Ignored for the same reason;
/// the app's Xcode build phase runs the real thing.
///
/// `cargo test -p koru-release generates_the_ios_notices_for_this_tree -- --ignored`
#[test]
#[ignore = "runs cargo metadata and reads the registry; run by hand"]
fn generates_the_ios_notices_for_this_tree() {
    let source = "https://github.com/koruworks/mono/tree/0000000";
    let text = generate(root(), &ios_build(Unrecorded::Refuse, Some(source)), &[])
        .expect("the iOS notices generate");
    for expected in [
        "SwiftProtobuf",
        "objc2",
        "loro",
        "The source code of this build: https://github.com/koruworks/mono/tree/0000000",
    ] {
        assert!(text.contains(expected), "{expected}");
    }
    for absent in [
        "sherpa-onnx",
        "whisper.cpp",
        "JetBrains Mono",
        "Inter-Regular.ttf",
        "INFERRED",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
}
