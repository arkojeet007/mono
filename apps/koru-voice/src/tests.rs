// SPDX-License-Identifier: MPL-2.0

//! **Crate-wide invariants — the ones that are about more than one module.**
//!
//! Chiefly the content boundary, because this binary is the only process in
//! Koru that holds audio, a transcript and a device's name at the same time.

use std::sync::{Arc, Mutex, OnceLock};

use koru_voice::{EngineEvent, Event, Requested, SessionId, State, Transcript, VoiceLanguage};

use crate::engine::Script;
use crate::fixtures;
use crate::worker::{Command, Shutdown, spawn};
use koru_voice::control::Outgoing;
use koru_voice::status::{CaptureInfo, DeviceCategory, DeviceKind, InterfaceKind};
use koru_voice::status::{ModelSummary, StageTiming, StatusReport};

/// The transcript the redaction test hunts for. Nonsense on purpose: a grep for
/// it finds only the tests that are meant to mention it.
const SENTINEL: &str = "quenderoth-vasp-1471";

/// What a sample looks like once something has printed it.
/// [`fixtures::SAMPLE`] is `0.1234567`, so any line holding one holds this.
const AUDIO_SENTINEL: &str = "0.123456";

/// A `tracing` subscriber that keeps every line, at every level.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<u8>>>);

impl Recorder {
    fn lines(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Recorder {
    type Writer = RecorderWriter;

    fn make_writer(&'a self) -> RecorderWriter {
        RecorderWriter(self.0.clone())
    }
}

struct RecorderWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for RecorderWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// **The recorder has to be the process-wide default, not a thread-local one.**
///
/// `tracing::subscriber::with_default` sets a *thread-local* dispatcher, and a
/// thread spawned inside that scope does **not** inherit it — so the inference
/// thread, which is the thread that holds the transcript and the audio, would
/// have logged to nothing and this test would have proved that an empty string
/// contains no sentinel. `set_global_default` is what actually sees it.
///
/// Global state is behind a `OnceLock` for the reason the theme's `THEME_LOCK`
/// exists: cargo runs this binary's tests in parallel, and a second
/// `set_global_default` fails. Every other test's lines land in the same buffer,
/// which makes the assertion *stronger* rather than weaker — no line this whole
/// suite produced may carry either sentinel.
fn recorder() -> Recorder {
    static RECORDER: OnceLock<Recorder> = OnceLock::new();
    RECORDER
        .get_or_init(|| {
            let recorder = Recorder::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(recorder.clone())
                .with_ansi(false)
                .with_env_filter(tracing_subscriber::EnvFilter::new("trace"))
                .finish();
            // Ignored rather than unwrapped: if something else in this process
            // has already installed one, the assertion below that the worker's
            // own line was captured is what fails, and it says so plainly.
            let _ = tracing::subscriber::set_global_default(subscriber);
            recorder
        })
        .clone()
}

/// **The redaction test `docs/contributing/operations/LOGGING.md` mandates.**
///
/// A whole dictation runs with a sentinel transcript and a sentinel waveform,
/// with the filter wide open at `trace` — every target, every level, every
/// field — and then every line the worker produced is read back. Neither the
/// words somebody said nor a sample of what they sounded like may be in any of
/// them.
#[test]
fn no_log_line_carries_audio_or_a_transcript() {
    let recorder = recorder();

    let script = Script::new().finishing_with(SENTINEL);
    let (shutdown, _stopped) = Shutdown::channel();
    let handle = spawn(fixtures::config(&script, fixtures::store()), shutdown);
    let (sink, events) = std::sync::mpsc::channel();

    let session = SessionId::new();
    assert!(handle.send(Command::Request {
        client: 1,
        request: koru_voice::Request::Begin {
            session,
            generation: 1,
            language: Requested::Fixed(VoiceLanguage::En),
            tier: koru_voice::ModelTier::Compact,
            model: None,
            input: koru_voice::InputChoice::SystemDefault,
        },
        sink: sink.clone(),
    }));

    // Partials carry the sentinel too, because a partial is the message that
    // arrives several times a second and is therefore the one somebody is
    // tempted to log.
    script.queue(EngineEvent::Partial(Transcript::new(SENTINEL)));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut finished = false;
    let mut asked_to_finish = false;
    while std::time::Instant::now() < deadline && !finished {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match events.recv_timeout(left) {
            Ok(Outgoing::Event(Event::State {
                state: State::Listening,
                ..
            })) => {
                if !asked_to_finish {
                    asked_to_finish = true;
                    assert!(handle.send(Command::Request {
                        client: 1,
                        request: koru_voice::Request::Finish {
                            session,
                            generation: 1,
                        },
                        sink: sink.clone(),
                    }));
                }
            }
            Ok(Outgoing::Event(Event::Ended { .. })) => finished = true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(finished, "the sentinel dictation did not complete");
    // The worker really did hold both sentinels, or this proves nothing.
    assert_eq!(script.accepted().first(), Some(&fixtures::SAMPLE));

    let lines = recorder.lines();
    assert!(
        lines.contains("inference thread is running"),
        "the worker's own lines were not captured, so this test asserts nothing: {lines}"
    );
    assert!(
        !lines.contains(SENTINEL),
        "a transcript reached a log line: {lines}"
    );
    assert!(
        !lines.contains(AUDIO_SENTINEL),
        "an audio sample reached a log line: {lines}"
    );
}

/// The same boundary on the way *out*: every type this binary can format — a
/// status report, a wire event, a capture description, the capture source
/// itself — is printed plain, alternate and as JSON, and none of them may
/// reveal a transcript, a sample or a device's name.
#[test]
fn nothing_this_worker_can_print_reveals_content() {
    let name = "Steven's AirPods Pro";
    let secrets = [SENTINEL, AUDIO_SENTINEL, name];
    let description = cpal::DeviceDescriptionBuilder::new(name)
        .manufacturer(name)
        .device_type(cpal::DeviceType::Headset)
        .interface_type(cpal::InterfaceType::Bluetooth)
        .build();
    let category = DeviceCategory::of(&description);
    assert_eq!(category.device_type, DeviceKind::Headset);
    assert_eq!(category.interface_type, InterfaceKind::Bluetooth);

    let report = StatusReport {
        worker_version: "0.0.3".into(),
        protocol_version: koru_voice::PROTOCOL_VERSION,
        channel: "internal-debug".into(),
        process_id: 1234,
        supervisor_launch_id: None,
        engine: "scripted".into(),
        engine_version: "scripted".into(),
        available_engines: vec![koru_voice::status::EngineSummary {
            engine: "scripted".into(),
            version: "scripted".into(),
        }],
        model_loaded: true,
        model: Some(ModelSummary {
            id: fixtures::MODEL.into(),
            version: fixtures::VERSION.into(),
            digest: fixtures::DIGEST.into(),
            tier: "compact".into(),
            engine: "scripted".into(),
            languages: vec!["en".into()],
        }),
        device: Some(category),
        capture: Some(CaptureInfo {
            category,
            device_sample_rate: 48_000,
            device_channels: 2,
            engine_sample_rate: koru_voice::SAMPLE_RATE,
        }),
        sample_rate: koru_voice::SAMPLE_RATE,
        ring_overruns: 12,
        ring_capacity: 32_768,
        session: koru_voice::SessionStats::default(),
        state: State::Listening,
        last_error: Some("engine_failed".into()),
        stages: vec![StageTiming {
            stage: "model_load".into(),
            millis: 3_400,
        }],
        uptime_millis: 60_000,
        idle_millis: 0,
        unload_after_millis: 300_000,
        exit_after_millis: 1_800_000,
    };

    let event = Event::Partial {
        session: SessionId::new(),
        generation: 1,
        text: Transcript::new(SENTINEL),
    };
    let outgoing = Outgoing::Event(event.clone());
    let source = crate::worker::CaptureSource::Recorded(fixtures::recording(0.01));

    let printed = [
        report.render(),
        format!("{report:?}"),
        format!("{report:#?}"),
        serde_json::to_string(&report).unwrap(),
        format!("{event:?}"),
        format!("{outgoing:?}"),
        format!("{source:?}"),
        format!("{source:#?}"),
    ];
    for line in &printed {
        for secret in secrets {
            assert!(
                !line.contains(secret),
                "a rendered value revealed {secret:?}: {line}"
            );
        }
    }

    // The one exception, and it is deliberate: `Serialize` is not redacted,
    // because carrying the text is the entire point of the socket.
    let wire = serde_json::to_string(&outgoing).unwrap();
    assert!(wire.contains(SENTINEL));
}

/// **`KORU_VOICE_DISABLED=1` starts no worker**, and this is the half of that
/// claim this binary owns: `serve` stands down before the bootstrap pipe, the
/// log file, the endpoint bind and the model cache. The other half — the agent
/// not supervising one, and no chord being bound — belongs to the supervisor
/// and to the shell's key map, which do not exist to be asserted here yet.
///
/// The predicate is asserted rather than the exit, because calling `serve`
/// would need a root and would open the very things the guard exists to
/// prevent. `koru_voice::availability` pins the truthiness rule itself.
#[test]
fn a_disabled_installation_stands_down_before_it_opens_anything() {
    let name = koru_voice::DISABLE_VAR;
    let restore = std::env::var(name).ok();
    // Single-threaded within this test; no other test in this binary reads or
    // writes this name.
    unsafe { std::env::remove_var(name) };
    assert!(
        super::stand_down_notice().is_none(),
        "an unset switch must leave the worker startable"
    );
    unsafe { std::env::set_var(name, "1") };
    let notice = super::stand_down_notice().expect("a disabled installation stands down");
    assert!(notice.contains(name), "the notice must name the switch");
    match restore {
        Some(value) => unsafe { std::env::set_var(name, value) },
        None => unsafe { std::env::remove_var(name) },
    }
}

/// This package's own manifest, read at compile time.
///
/// **The property is a manifest one and no `cfg!` can see it.** Both adapters
/// must be non-optional runtime dependencies of every product worker.
const MANIFEST: &str = include_str!("../Cargo.toml");

/// The package was already named `koru-voice-worker`; Plan 1.1 changes only
/// the installed image and intentionally leaves no pre-cutover alias.
#[test]
fn the_product_manifest_names_only_the_worker_binary() {
    let declared_names = MANIFEST
        .lines()
        .filter(|line| line.starts_with("name = "))
        .collect::<Vec<_>>();
    assert_eq!(
        declared_names.len(),
        3,
        "package, binary and library targets are the only named targets"
    );
    assert_eq!(
        declared_names[0], declared_names[1],
        "the package and its one binary target must share a name"
    );
    assert!(declared_names[0].contains("koru-voice-worker"));
}

#[test]
fn every_product_build_contains_both_speech_runtimes() {
    assert!(MANIFEST.contains(
        "koru-voice-sherpa = { path = \"../../crates/koru-voice-sherpa\", features = [\"runtime\"] }"
    ));
    assert!(MANIFEST.contains(
        "koru-voice-whisper = { path = \"../../crates/koru-voice-whisper\", features = [\"runtime\"] }"
    ));
    assert!(!MANIFEST.contains("optional = true"));
    assert!(!MANIFEST.contains("sherpa = ["));
}

/// Both engines are native static libraries in the same process. The pinned
/// sherpa Windows archive is `/MT`; whisper.cpp defaults to `/MD`, and MSVC
/// records that disagreement in every object for the linker to refuse.
#[cfg(target_env = "msvc")]
#[test]
fn both_windows_speech_runtimes_use_the_static_release_crt() {
    assert!(
        cfg!(target_feature = "crt-static"),
        "the sherpa static-MT archive, whisper.cpp and Rust must use one MSVC runtime"
    );
}
