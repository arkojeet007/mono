// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **The plan's guard, asserted rather than assumed.**
///
/// A microphone light coming on during `cargo test` is the failure this feature
/// exists to prevent, and it is a failure nobody notices in CI. `cfg(test)`
/// cannot cover it — a dependency is never compiled with it — so the feature is
/// turned on in this package's `[dev-dependencies]` and this is what says it is
/// still turned on.
#[test]
fn a_test_build_never_opens_an_audio_device() {
    assert!(
        cfg!(feature = "no-capture"),
        "`no-capture` is off in this test build — `cargo test` would open the machine's microphone. \
         It is turned on by the self dev-dependency in apps/koru-voice/Cargo.toml."
    );
    assert!(describe_default().is_none());
    let refused = open_default(std::sync::Arc::new(Ring::new(64))).unwrap_err();
    assert_eq!(refused.code, ErrorCode::CaptureFailed);
}

/// **A device's name is a person's hardware and never reaches a category.**
///
/// The description is built with the name, the manufacturer, the driver and the
/// address all set to strings a support log must never carry, and the category
/// derived from it is checked — formatted, `Debug`-ed and serialized — for
/// every one of them.
#[test]
fn a_category_never_carries_the_device_name() {
    let secrets = [
        "Steven's AirPods Pro",
        "Blue Microphones",
        "Realtek(R) Audio Driver",
        "usb#vid_046d&pid_0892",
    ];
    let description = cpal::DeviceDescriptionBuilder::new(secrets[0])
        .manufacturer(secrets[1])
        .driver(secrets[2])
        .address(secrets[3])
        .device_type(cpal::DeviceType::Headset)
        .interface_type(cpal::InterfaceType::Bluetooth)
        .build();

    let category = category_of(&description);
    assert_eq!(category.device_type, DeviceKind::Headset);
    assert_eq!(category.interface_type, InterfaceKind::Bluetooth);
    assert_eq!(category.to_string(), "headset/bluetooth");

    let info = CaptureInfo {
        category,
        device_sample_rate: 48_000,
        device_channels: 2,
        engine_sample_rate: koru_voice::SAMPLE_RATE,
    };
    let rendered = [
        format!("{category}"),
        format!("{category:?}"),
        format!("{info:?}"),
        format!("{info:#?}"),
        serde_json::to_string(&info).unwrap(),
    ];
    for line in &rendered {
        for secret in secrets {
            assert!(
                !line.contains(secret),
                "a device name reached a rendered category: {line}"
            );
        }
    }
}

/// An unfamiliar `cpal` category is `unknown` rather than a build failure: the
/// enums are `#[non_exhaustive]`, so an upgrade that adds a device kind must
/// not stop this compiling.
#[test]
fn an_unmapped_category_reads_as_unknown() {
    let description = cpal::DeviceDescriptionBuilder::new("x").build();
    let category = category_of(&description);
    assert_eq!(category, DeviceCategory::UNKNOWN);
}

/// The meter is buckets, and the bottom of the scale really is silence — an
/// indicator that never rests at zero reads as a microphone that is always
/// hearing something.
#[test]
fn the_level_meter_is_nine_buckets_and_silence_is_one_of_them() {
    assert_eq!(level_of(&[]), koru_voice::LevelBucket::SILENT);
    assert_eq!(level_of(&[0.0; 16]), koru_voice::LevelBucket::SILENT);
    // -80 dBFS: below the floor, so still silence.
    assert_eq!(level_of(&[0.0001]), koru_voice::LevelBucket::SILENT);
    assert_eq!(
        level_of(&[1.0]).step(),
        koru_voice::LevelBucket::MAX,
        "a full-scale sample is the top bucket"
    );
    // Ordinary speech sits in the middle rather than pinned at either end,
    // which is the whole reason the scale is logarithmic.
    let ordinary = level_of(&[0.05]).step();
    assert!(
        (1..koru_voice::LevelBucket::MAX).contains(&ordinary),
        "-26 dBFS landed in bucket {ordinary}"
    );
    // Monotonic, and clipping cannot escape the top.
    assert!(level_of(&[0.5]).step() >= level_of(&[0.05]).step());
    assert_eq!(level_of(&[9.0]).step(), koru_voice::LevelBucket::MAX);
}

/// **The tokens serde writes are the tokens `as_str` says**, walked rather than
/// spot-checked: a `rename_all` change would otherwise move the wire spelling
/// and leave every log line and every future translation key behind.
#[test]
fn every_category_token_is_the_one_serde_writes() {
    for kind in DeviceKind::ALL {
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{}\"", kind.as_str())
        );
        assert_eq!(kind.to_string(), kind.as_str());
    }
    for kind in InterfaceKind::ALL {
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{}\"", kind.as_str())
        );
        assert_eq!(kind.to_string(), kind.as_str());
    }
}

// -------------------------------------------------------------- permission

/// **A denied permission has its own code**, because a refusal and a missing
/// device are fixed in different places.
#[test]
fn a_backend_that_says_permission_denied_is_a_permission_refusal() {
    let denied = cpal::Error::with_message(
        cpal::ErrorKind::PermissionDenied,
        "microphone access was not authorized",
    );
    assert_eq!(refusal(&denied), koru_voice::ErrorCode::PermissionDenied);
}

/// **The Windows case, which cpal does not classify for us.** WASAPI maps a
/// dozen `AUDCLNT_*` codes and lets everything else fall to `BackendError`, and
/// a per-app privacy denial is `E_ACCESSDENIED`, which is not one of them.
///
/// Every form here is what `std::io::Error`'s or `windows::core::Error`'s own
/// rendering produces, and the **localized** sentences are the point: a
/// classifier that matched on *Access is denied* would work on an English
/// installation and silently stop working on a German one.
#[test]
fn a_windows_privacy_denial_is_a_permission_refusal() {
    for message in [
        "Access is denied. (os error -2147024891)",
        "Zugriff verweigert (os error -2147024891)",
        "\u{62d}\u{62f}\u{62b} \u{62e}\u{637}\u{623} (os error -2147024891)",
        "The stream could not be initialized (0x80070005)",
        "E_ACCESSDENIED",
        "Access is denied. (os error 5)",
    ] {
        let error = cpal::Error::with_message(cpal::ErrorKind::BackendError, message);
        assert_eq!(
            refusal(&error),
            koru_voice::ErrorCode::PermissionDenied,
            "{message:?} names a refusal"
        );
    }
}

/// Everything else stays `capture_failed`. A device somebody unplugged is not a
/// privacy setting, and telling them to look in Settings would be worse than
/// saying nothing.
#[test]
fn an_ordinary_capture_failure_is_not_a_permission_refusal() {
    for (kind, message) in [
        (cpal::ErrorKind::DeviceNotAvailable, "the device is gone"),
        (cpal::ErrorKind::DeviceBusy, "another process holds it"),
        (
            cpal::ErrorKind::BackendError,
            "The device is in use. (os error 55)",
        ),
        (
            cpal::ErrorKind::UnsupportedConfig,
            "the device offers no 16 kHz mono",
        ),
    ] {
        let error = cpal::Error::with_message(kind, message);
        assert_eq!(
            refusal(&error),
            koru_voice::ErrorCode::CaptureFailed,
            "{message:?} is not a refusal"
        );
    }
}

/// An error with no message at all is not a refusal — the absence of evidence
/// is not evidence, and `capture_failed` is the honest answer.
#[test]
fn an_error_with_no_message_is_not_a_permission_refusal() {
    let bare = cpal::Error::new(cpal::ErrorKind::BackendError);
    assert_eq!(refusal(&bare), koru_voice::ErrorCode::CaptureFailed);
}

/// **The needle is built rather than believed.** `windows_result` converts an
/// HRESULT with `io::Error::from_raw_os_error(code().0)` — the whole HRESULT,
/// not its Win32 half — so the rendered tail is a nine-digit negative number,
/// and an intuition about *os error 5* is wrong. This asks the standard library
/// what it actually prints and hands that to the classifier, so a change in
/// either fails here rather than on somebody's machine.
#[cfg(windows)]
#[test]
fn the_denial_needle_is_what_windows_actually_renders() {
    const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
    let rendered = std::io::Error::from_raw_os_error(E_ACCESSDENIED).to_string();
    assert!(
        reads_as_denied(&rendered),
        "Windows renders E_ACCESSDENIED as {rendered:?}, which the classifier missed"
    );
}

/// A device already in use renders `os error 55`, which shares a prefix with
/// the Win32 spelling of a denial. The parentheses in the needle are what keep
/// the two apart, and this is the case that put them there.
#[test]
fn a_busy_device_is_not_mistaken_for_a_denial() {
    assert!(!reads_as_denied("The device is in use. (os error 55)"));
    assert!(!reads_as_denied("os error 53"));
    assert!(reads_as_denied("Access is denied. (os error 5)"));
}

// ------------------------------------------------------------- the microphone

/// **Enumerating is behind the same guard as opening**, and this is what says
/// so. `cpal`'s `input_devices` initialises the audio host — COM on WASAPI, the
/// audio server on CoreAudio — so a suite that merely listed microphones would
/// still be a suite that woke the audio stack on somebody's machine. What a
/// test build sees is a fixture.
#[test]
fn a_test_build_enumerates_a_fixture_and_no_device() {
    assert!(cfg!(feature = "no-capture"));
    let devices = input_devices();
    assert_eq!(
        devices,
        FIXTURE_DEVICES
            .iter()
            .map(|(name, default)| InputDevice::new(*name, *default))
            .collect::<Vec<_>>(),
        "a test build enumerated something other than the fixture"
    );
    assert_eq!(
        devices.iter().filter(|d| d.default).count(),
        1,
        "exactly one fixture device is the host's own choice"
    );
}

/// Choosing a device does not make a test build open one either — the guard is
/// on the *function*, not on the argument.
#[test]
fn choosing_a_device_still_opens_nothing_under_test() {
    for choice in [
        InputChoice::SystemDefault,
        InputChoice::named(FIXTURE_DEVICES[1].0),
        InputChoice::named("a microphone nobody has"),
    ] {
        let refused = open(&choice, std::sync::Arc::new(Ring::new(64))).unwrap_err();
        assert_eq!(refused.code, ErrorCode::CaptureFailed, "{choice:?}");
    }
}

/// **A remembered device that is gone falls back to the host's own**, and the
/// resolution still knows what was asked for — which is what lets a surface say
/// so rather than leaving somebody with a worse microphone and no explanation.
///
/// The rule itself is `koru_voice::input`'s; this asserts that *this* module
/// reads a choice against the list it enumerates rather than against something
/// of its own.
#[test]
fn a_choice_is_read_against_what_this_module_can_see() {
    let devices = input_devices();
    let present = koru_voice::input::resolve(&InputChoice::named(FIXTURE_DEVICES[1].0), &devices);
    assert_eq!(present.device(), Some(FIXTURE_DEVICES[1].0));

    let gone = koru_voice::input::resolve(&InputChoice::named("Steven's AirPods"), &devices);
    assert_eq!(gone.device(), None, "it falls back to the host's default");
    assert_eq!(
        gone.missing(),
        Some("Steven's AirPods"),
        "and it says which device it could not find"
    );
}
