// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The three values the worker's `fixtures` module supplies, spelled here so
/// the contract crate's tests take nothing from the worker: `fixtures` links
/// model-store types this crate must not.
mod fixtures {
    pub const MODEL: &str = "fixture-model";
    pub const VERSION: &str = "1.0.0";
    pub const DIGEST: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
}

fn microphone() -> DeviceCategory {
    DeviceCategory {
        device_type: DeviceKind::Microphone,
        interface_type: InterfaceKind::Usb,
    }
}

/// The value of one rendered line, by its key.
///
/// The block is `{key:<20} {value}`, and asserting on the padding would be
/// asserting the format back to itself.
fn field(rendered: &str, key: &str) -> String {
    rendered
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| panic!("no `{key}` line in: {rendered}"))
}

fn report() -> StatusReport {
    StatusReport {
        worker_version: "0.0.3".into(),
        protocol_version: crate::PROTOCOL_VERSION,
        channel: "internal-debug".into(),
        process_id: 4242,
        supervisor_launch_id: Some("a-launch".into()),
        engine: "sherpa".into(),
        engine_version: "1.12.14".into(),
        available_engines: vec![
            EngineSummary {
                engine: "sherpa".into(),
                version: "1.12.14".into(),
            },
            EngineSummary {
                engine: "whisper".into(),
                version: "whisper.cpp".into(),
            },
        ],
        model_loaded: true,
        model: Some(ModelSummary {
            id: fixtures::MODEL.into(),
            version: fixtures::VERSION.into(),
            digest: fixtures::DIGEST.into(),
            tier: "compact".into(),
            engine: "sherpa".into(),
            languages: vec!["en".into(), "zh-Hans".into()],
        }),
        device: Some(microphone()),
        capture: Some(CaptureInfo {
            category: microphone(),
            device_sample_rate: 48_000,
            device_channels: 2,
            engine_sample_rate: crate::SAMPLE_RATE,
        }),
        sample_rate: crate::SAMPLE_RATE,
        ring_overruns: 320,
        ring_capacity: 32_768,
        session: crate::SessionStats {
            sessions: 3,
            partials: 41,
            finals: 2,
            refusals: 1,
            ..Default::default()
        },
        state: crate::State::Listening,
        last_error: Some("capture_failed".into()),
        stages: vec![
            StageTiming {
                stage: "model_load".into(),
                millis: 3_400,
            },
            StageTiming {
                stage: "capture_start".into(),
                millis: 40,
            },
            StageTiming {
                stage: "finalise".into(),
                millis: 180,
            },
        ],
        uptime_millis: 61_000,
        idle_millis: 0,
        unload_after_millis: 300_000,
        exit_after_millis: 1_800_000,
    }
}

/// **Everything Plan 1.3 says `koru-voice-worker status` prints**, asked for by name.
///
/// Engine and version, the model's id, version and digest, loaded or unloaded,
/// the capture device's category, the sample rate, ring overruns, sessions this
/// run, the last error code, and the last three stage timings.
#[test]
fn the_rendered_block_answers_every_question_the_plan_asks() {
    let rendered = report().render();
    assert_eq!(field(&rendered, "engine"), "sherpa 1.12.14");
    assert_eq!(
        field(&rendered, "available_engines"),
        "sherpa 1.12.14, whisper whisper.cpp"
    );
    assert_eq!(
        field(&rendered, "model"),
        format!(
            "{}/{} {} engine=sherpa tier=compact languages=en,zh-Hans",
            fixtures::MODEL,
            fixtures::VERSION,
            fixtures::DIGEST
        )
    );
    assert_eq!(field(&rendered, "loaded"), "yes");
    assert_eq!(field(&rendered, "device"), "microphone/usb");
    assert_eq!(field(&rendered, "capture"), "open 48000 Hz x2 -> 16000 Hz");
    assert_eq!(field(&rendered, "sample_rate"), "16000");
    assert_eq!(field(&rendered, "ring"), "overruns=320 capacity=32768");
    assert_eq!(field(&rendered, "state"), "listening");
    assert_eq!(
        field(&rendered, "sessions"),
        "3 partials=41 finals=2 refusals=1"
    );
    assert_eq!(field(&rendered, "last_error"), "capture_failed");
    assert_eq!(
        field(&rendered, "stages"),
        "model_load=3400ms capture_start=40ms finalise=180ms"
    );
    assert!(field(&rendered, "supervised").starts_with("yes launch="));
}

/// A worker that has done nothing renders without panicking and says so in
/// words rather than in blanks — an empty field reads as a bug.
#[test]
fn a_worker_that_has_done_nothing_still_renders() {
    let mut fresh = report();
    fresh.model = None;
    fresh.model_loaded = false;
    fresh.device = None;
    fresh.capture = None;
    fresh.last_error = None;
    fresh.stages.clear();
    fresh.supervisor_launch_id = None;
    let rendered = fresh.render();
    assert_eq!(field(&rendered, "model"), "none");
    assert_eq!(field(&rendered, "loaded"), "no");
    assert_eq!(field(&rendered, "device"), "unknown");
    assert_eq!(field(&rendered, "capture"), "closed");
    assert_eq!(field(&rendered, "last_error"), "none");
    assert_eq!(field(&rendered, "stages"), "none yet");
    assert_eq!(field(&rendered, "supervised"), "no");
}

/// The same data crosses the socket, so the JSON and the block cannot say
/// different things.
#[test]
fn the_report_round_trips_as_json() {
    let report = report();
    let wire = serde_json::to_string(&report).unwrap();
    let read: StatusReport = serde_json::from_str(&wire).unwrap();
    assert_eq!(read, report);
    assert_eq!(read.render(), report.render());
}

/// **A path is never in a status report**, and neither is a device's name — the
/// two absences `status.rs` names out loud, held by something rather than by a
/// reader remembering.
#[test]
fn a_report_carries_no_path_and_no_device_name() {
    let rendered = report().render();
    let wire = serde_json::to_string(&report()).unwrap();
    for forbidden in ["C:\\", "/Users/", "AppData", "Yeti", "AirPods"] {
        assert!(!rendered.contains(forbidden), "{forbidden:?} in {rendered}");
        assert!(!wire.contains(forbidden), "{forbidden:?} in {wire}");
    }
}

/// **Three, because that is one dictation** — a model load, a capture start and
/// a finalise are the three waits a person can feel.
#[test]
fn three_stages_are_kept() {
    assert_eq!(STAGES_KEPT, 3);
}
