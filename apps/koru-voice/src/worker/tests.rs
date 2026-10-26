// SPDX-License-Identifier: MPL-2.0

//! **The worker driven end to end, against a scripted engine and a recording.**
//!
//! No device, no model bytes, no socket. What is under test is which effects
//! are performed in what order and what the client is told about it — and a
//! real recogniser would make every one of those depend on a model, a machine
//! and a person's voice.
//!
//! The one exception is the [`dictating`] submodule, which does load model
//! bytes when its explicit environment gate is set. It is where
//! audio becomes text, and everything it needs that this file refuses — half a
//! gigabyte of weights, a native runtime — is why it is behind a feature *and*
//! behind an environment variable naming the directory.

use std::sync::mpsc::{Receiver, channel};

use super::*;
use crate::engine::Script;
use crate::fixtures;
use crate::models::MemoryModelStore;
use koru_voice::control::Outgoing;
use koru_voice::{
    EndReason, EngineEvent, ErrorCode, Event, ModelId, ModelTier, Requested, SessionId, State,
    Transcript, VoiceLanguage,
};
use koru_voice_models::ModelVersion;

/// **Not a budget on the machine.** Every wait here is for something the worker
/// does immediately; the bound is generous so a loaded CI box fails a test
/// about semantics only when the semantics are wrong.
const PATIENCE: Duration = Duration::from_secs(15);

/// One client, driving one worker.
struct Driver {
    handle: WorkerHandle,
    sink: EventSink,
    events: Receiver<Outgoing>,
    stopped: Receiver<()>,
    next_generation: u64,
}

impl Driver {
    fn new(config: WorkerConfig) -> Driver {
        let (shutdown, stopped) = Shutdown::channel();
        let handle = crate::worker::spawn(config, shutdown);
        let (sink, events) = channel();
        Driver {
            handle,
            sink,
            events,
            stopped,
            next_generation: 1,
        }
    }

    fn send(&self, request: Request) {
        assert!(
            self.handle.send(Command::Request {
                client: 1,
                request,
                sink: self.sink.clone(),
            }),
            "the inference thread has gone"
        );
    }

    fn begin(&mut self) -> (SessionId, u64) {
        let session = SessionId::new();
        let generation = self.next_generation;
        self.next_generation += 1;
        self.send(Request::Begin {
            session,
            generation,
            language: Requested::Fixed(VoiceLanguage::En),
            tier: ModelTier::Compact,
            model: None,
            input: koru_voice::InputChoice::SystemDefault,
        });
        (session, generation)
    }

    /// Collect events until one matches, or fail saying what did arrive.
    ///
    /// The failure prints the events, which is safe: every one of them formats
    /// through `koru_voice::Transcript`'s redacted `Debug`.
    fn wait_for(&self, seen: &mut Vec<Event>, matches: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(Outgoing::Event(event)) => {
                    seen.push(event.clone());
                    if matches(&event) {
                        return event;
                    }
                }
                Ok(Outgoing::Status(_)) => {}
                Err(_) => panic!("no matching event arrived; the worker sent {seen:?}"),
            }
        }
    }

    fn listening(&self, seen: &mut Vec<Event>) {
        self.wait_for(seen, |event| {
            matches!(
                event,
                Event::State {
                    state: State::Listening,
                    ..
                }
            )
        });
    }

    /// Everything already queued, without waiting.
    fn drain(&self, seen: &mut Vec<Event>) {
        while let Ok(message) = self.events.try_recv() {
            if let Outgoing::Event(event) = message {
                seen.push(event);
            }
        }
    }

    fn status(&self) -> Box<StatusReport> {
        self.handle
            .status(PATIENCE)
            .expect("the worker did not answer a status request")
    }

    /// Poll status until a condition holds, so a suite never sleeps for a
    /// fixed time it hopes is long enough.
    fn status_until(&self, holds: impl Fn(&StatusReport) -> bool) -> Box<StatusReport> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let report = self.status();
            if holds(&report) {
                return report;
            }
            assert!(
                Instant::now() < deadline,
                "the worker never reached the expected state: {report:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn fixture_model() -> (ModelId, ModelVersion) {
    (
        ModelId::parse(fixtures::MODEL).unwrap(),
        ModelVersion::parse(fixtures::VERSION).unwrap(),
    )
}

/// **The plan's end-to-end test.** A recording drives the machine from a
/// gesture to a committed transcript, and every sample reaches the engine.
#[test]
fn a_recorded_session_reaches_a_final_transcript() {
    let script = Script::new().finishing_with("create a new project plan");
    let store = fixtures::store();
    let mut drive = Driver::new(fixtures::config(&script, store.clone()));
    let mut seen = Vec::new();

    let (session, generation) = drive.begin();
    drive.listening(&mut seen);
    drive.send(Request::Finish {
        session,
        generation,
    });

    let last = drive.wait_for(&mut seen, |event| matches!(event, Event::Final { .. }));
    match last {
        Event::Final { text, .. } => assert_eq!(text.as_str(), "create a new project plan"),
        other => panic!("expected a final, got {other:?}"),
    }
    let ended = drive.wait_for(&mut seen, |event| matches!(event, Event::Ended { .. }));
    assert!(
        matches!(
            ended,
            Event::Ended {
                reason: EndReason::Completed,
                ..
            }
        ),
        "{ended:?}"
    );

    // The recording really went through the ring, the resampler's identity path
    // and into the engine — half a second at the engine's own rate.
    let accepted = script.accepted();
    assert_eq!(accepted.len(), (koru_voice::SAMPLE_RATE / 2) as usize);
    assert!(accepted.iter().all(|sample| *sample == fixtures::SAMPLE));

    // A meter was fed, and it is buckets rather than samples.
    assert!(
        seen.iter()
            .any(|event| matches!(event, Event::Level { .. })),
        "no level bucket reached the client: {seen:?}"
    );

    // And the model was leased for exactly as long as the engine lived.
    let (id, version) = fixture_model();
    assert_eq!(store.leases(&id, &version), 1);
    assert_eq!(script.opens(), 1);
}

/// **The plan's cancellation test, both halves.**
///
/// A session cancelled mid-utterance emits no `Final`; a `Final` that a decoder
/// produces afterwards is dropped on its generation and counted, rather than
/// committed into whatever comes next.
#[test]
fn a_cancelled_session_emits_no_final_and_a_late_one_is_dropped() {
    let script = Script::new();
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();

    let (session, generation) = drive.begin();
    drive.listening(&mut seen);
    script.queue(EngineEvent::Partial(Transcript::new("create a new")));
    drive.wait_for(&mut seen, |event| matches!(event, Event::Partial { .. }));

    drive.send(Request::Cancel {
        session,
        generation,
    });
    let ended = drive.wait_for(&mut seen, |event| matches!(event, Event::Ended { .. }));
    assert!(
        matches!(
            ended,
            Event::Ended {
                reason: EndReason::Cancelled,
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(script.was_cancelled(), "the engine was not told to let go");

    // The decoder was still running, and answers two seconds late.
    let before = drive.status().session.dropped_engine_events;
    script.queue(EngineEvent::Final(Transcript::new("create a new project")));
    let after = drive.status_until(|report| report.session.dropped_engine_events > before);
    assert_eq!(after.session.finals, 0);

    drive.drain(&mut seen);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::Final { .. })),
        "a cancelled session produced a final: {seen:?}"
    );
}

/// **The plan's engine-error test.** A stable code, and a worker that is still
/// answering afterwards — including a second dictation that completes.
#[test]
fn an_engine_error_ends_the_session_and_the_worker_keeps_answering() {
    let script = Script::new().finishing_with("the second one worked");
    script.fail_audio(ErrorCode::EngineFailed);
    let store = fixtures::store();
    let mut drive = Driver::new(fixtures::config(&script, store.clone()));
    let mut seen = Vec::new();

    drive.begin();
    let ended = drive.wait_for(&mut seen, |event| matches!(event, Event::Ended { .. }));
    match ended {
        Event::Ended {
            reason: EndReason::Failed { code, .. },
            ..
        } => assert_eq!(code, ErrorCode::EngineFailed),
        other => panic!("expected a failure, got {other:?}"),
    }

    let report = drive.status();
    assert_eq!(report.last_error.as_deref(), Some("engine_failed"));
    assert_eq!(report.state, State::Idle);

    // Still answering, and the model it already had is reused rather than
    // reloaded — a failure is not a reason to pay for the weights twice.
    script.allow_audio();
    let (session, generation) = drive.begin();
    drive.listening(&mut seen);
    drive.send(Request::Finish {
        session,
        generation,
    });
    let last = drive.wait_for(&mut seen, |event| matches!(event, Event::Final { .. }));
    match last {
        Event::Final { text, .. } => assert_eq!(text.as_str(), "the second one worked"),
        other => panic!("expected a final, got {other:?}"),
    }
    assert_eq!(script.opens(), 1, "the model was loaded more than once");
    let (id, version) = fixture_model();
    assert_eq!(store.leases(&id, &version), 1);
}

/// **One dictation at a time, and the refusal goes to whoever was refused.**
///
/// Sending it down the live session's socket would tell the person who *is*
/// dictating that their dictation was refused.
#[test]
fn a_second_client_is_told_busy_and_the_first_hears_nothing_about_it() {
    let script = Script::new().finishing_with("x");
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();
    drive.begin();
    drive.listening(&mut seen);

    let (other_sink, other_events) = channel();
    assert!(drive.handle.send(Command::Request {
        client: 2,
        request: Request::Begin {
            session: SessionId::new(),
            generation: 99,
            language: Requested::Fixed(VoiceLanguage::En),
            tier: ModelTier::Compact,
            model: None,
            input: koru_voice::InputChoice::SystemDefault,
        },
        sink: other_sink,
    }));

    match other_events.recv_timeout(PATIENCE) {
        Ok(Outgoing::Event(Event::Refused { code, .. })) => assert_eq!(code, ErrorCode::Busy),
        other => panic!("expected a busy refusal, got {other:?}"),
    }
    drive.drain(&mut seen);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::Refused { .. })),
        "the refusal reached the client that was dictating: {seen:?}"
    );
}

/// **An interruption discards.** A client whose socket closed had its dictation
/// thrown away; nothing is written and nothing is owed.
#[test]
fn a_client_that_goes_away_has_its_dictation_discarded() {
    let script = Script::new().finishing_with("x");
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();
    drive.begin();
    drive.listening(&mut seen);

    assert!(drive.handle.send(Command::Disconnected(1)));
    let report = drive.status_until(|report| report.state == State::Idle);
    assert_eq!(report.session.finals, 0);
    assert!(script.was_cancelled());
}

/// A model nobody has installed is a refusal, not a half-started session — and
/// the code is the one that is fixed by downloading something.
#[test]
fn a_model_that_is_not_installed_is_refused_before_anything_starts() {
    let script = Script::new();
    let empty = Arc::new(MemoryModelStore::new(Vec::new()));
    let mut config = fixtures::config(&script, fixtures::store());
    config.store = empty;
    let mut drive = Driver::new(config);
    let mut seen = Vec::new();

    drive.begin();
    let refused = drive.wait_for(&mut seen, |event| matches!(event, Event::Refused { .. }));
    match refused {
        Event::Refused { code, .. } => assert_eq!(code, ErrorCode::ModelMissing),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let report = drive.status();
    assert_eq!(
        report.session.sessions, 0,
        "a refused request began a session"
    );
    assert!(!report.model_loaded);
    assert_eq!(script.opens(), 0);
}

/// A model whose descriptor names another engine is refused rather than handed
/// to the wrong native parser — the check the signed `engine` field exists for.
#[test]
fn a_model_for_another_engine_is_not_loaded() {
    let script = Script::new();
    let mut installed = fixtures::installed("other-engine-model", "1.0.0");
    installed.descriptor.engine = fixtures::other_engine();
    let store = Arc::new(MemoryModelStore::new(vec![installed]));
    let mut config = fixtures::config(&script, fixtures::store());
    config.store = store;
    let mut drive = Driver::new(config);
    let mut seen = Vec::new();

    drive.begin();
    let refused = drive.wait_for(&mut seen, |event| matches!(event, Event::Refused { .. }));
    match refused {
        Event::Refused { code, .. } => assert_eq!(code, ErrorCode::ModelMissing),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(script.opens(), 0);
}

/// **The reason a sixth binary exists.** After the unload deadline the engine
/// and its lease are gone, and the worker is still answering.
#[test]
fn the_model_is_unloaded_after_the_idle_deadline() {
    let script = Script::new().finishing_with("x");
    let store = fixtures::store();
    let mut config = fixtures::config(&script, store.clone());
    config.idle = crate::idle::IdlePolicy {
        unload_after: Duration::from_millis(20),
        exit_after: Duration::ZERO,
    };
    let mut drive = Driver::new(config);
    let mut seen = Vec::new();

    let (session, generation) = drive.begin();
    drive.listening(&mut seen);
    drive.send(Request::Finish {
        session,
        generation,
    });
    drive.wait_for(&mut seen, |event| matches!(event, Event::Ended { .. }));

    let report = drive.status_until(|report| !report.model_loaded);
    // The model it *had* is still named, because the question after "it went
    // quiet" is which model it had been using.
    assert!(report.model.is_some());
    let (id, version) = fixture_model();
    assert_eq!(
        store.leases(&id, &version),
        0,
        "the lease outlived the unload, so another channel could not prune"
    );
}

/// And then the process itself, which is how the memory is genuinely released
/// rather than nominally.
#[test]
fn the_worker_ends_itself_after_the_exit_deadline() {
    let script = Script::new();
    let mut config = fixtures::config(&script, fixtures::store());
    config.idle = crate::idle::IdlePolicy {
        unload_after: Duration::ZERO,
        exit_after: Duration::from_millis(20),
    };
    let drive = Driver::new(config);
    drive
        .stopped
        .recv_timeout(PATIENCE)
        .expect("the worker did not end itself at the exit deadline");
}

#[test]
fn stop_ends_the_worker() {
    let script = Script::new();
    let drive = Driver::new(fixtures::config(&script, fixtures::store()));
    assert!(drive.handle.send(Command::Stop));
    drive
        .stopped
        .recv_timeout(PATIENCE)
        .expect("the worker did not stop when it was asked to");
}

/// A status request is answered **while a dictation is live**, which is when
/// somebody actually runs one.
#[test]
fn status_is_answered_mid_dictation_and_says_what_is_happening() {
    let script = Script::new().finishing_with("x");
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();
    drive.begin();
    drive.listening(&mut seen);

    let report = drive.status();
    assert_eq!(report.state, State::Listening);
    assert!(report.model_loaded);
    assert_eq!(report.sample_rate, koru_voice::SAMPLE_RATE);
    assert_eq!(report.session.sessions, 1);
    assert_eq!(report.engine, "scripted");
    let model = report.model.as_ref().expect("a loaded model is named");
    assert_eq!(model.id, fixtures::MODEL);
    assert_eq!(model.digest, fixtures::DIGEST);
    // The stages a person can feel, in the order they happened.
    let stages: Vec<&str> = report.stages.iter().map(|s| s.stage.as_str()).collect();
    assert_eq!(stages, vec!["model_load", "capture_start"]);
    // No device was opened, because the recording is the source.
    assert!(report.device.is_none());
    assert!(report.capture.is_none());
}

/// A request whose generation has already been used is refused rather than
/// restarting a dictation the person has finished — the reconnecting client's
/// replay.
#[test]
fn a_replayed_generation_is_refused_as_stale() {
    let script = Script::new().finishing_with("x");
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();

    let (session, generation) = drive.begin();
    drive.listening(&mut seen);
    drive.send(Request::Finish {
        session,
        generation,
    });
    drive.wait_for(&mut seen, |event| matches!(event, Event::Ended { .. }));

    drive.send(Request::Begin {
        session,
        generation,
        language: Requested::Fixed(VoiceLanguage::En),
        tier: ModelTier::Compact,
        model: None,
        input: koru_voice::InputChoice::SystemDefault,
    });
    let refused = drive.wait_for(&mut seen, |event| matches!(event, Event::Refused { .. }));
    match refused {
        Event::Refused { code, .. } => assert_eq!(code, ErrorCode::StaleGeneration),
        other => panic!("expected a stale refusal, got {other:?}"),
    }
}

/// A `finish` that names a session nobody is running is refused, and the worker
/// does not end the one that *is*.
#[test]
fn a_finish_for_another_session_is_refused_and_changes_nothing() {
    let script = Script::new().finishing_with("x");
    let mut drive = Driver::new(fixtures::config(&script, fixtures::store()));
    let mut seen = Vec::new();
    let (_, generation) = drive.begin();
    drive.listening(&mut seen);

    drive.send(Request::Finish {
        session: SessionId::new(),
        generation,
    });
    let refused = drive.wait_for(&mut seen, |event| matches!(event, Event::Refused { .. }));
    match refused {
        Event::Refused { code, .. } => assert_eq!(code, ErrorCode::StaleGeneration),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(drive.status().state, State::Listening);
}

/// **The `Debug` on the capture source is a count, never the samples.** A
/// derived one would print a recording into whatever formatted the enclosing
/// struct, which is the mistake the redacted `Transcript` exists to prevent one
/// type over.
#[test]
fn a_capture_source_never_prints_its_audio() {
    let source = CaptureSource::Recorded(fixtures::recording(0.01));
    let printed = format!("{source:?} {source:#?}");
    assert!(!printed.contains("0.123"), "{printed}");
    assert!(printed.contains("160 samples"), "{printed}");
    assert_eq!(format!("{:?}", CaptureSource::Device), "Device");
}

/// A command's `Debug` names the verb and never the fields — one edit away from
/// naming something that is not a verb.
#[test]
fn a_command_prints_its_verb_and_nothing_else() {
    let (sink, _events) = channel();
    let command = Command::Request {
        client: 7,
        request: Request::Begin {
            session: SessionId::new(),
            generation: 3,
            language: Requested::Fixed(VoiceLanguage::Ja),
            tier: ModelTier::Compact,
            model: Some(ModelId::parse("secret-model").unwrap()),
            // A device name is a person's hardware, and this is the test that
            // says a command's `Debug` prints a verb and never a field.
            input: koru_voice::InputChoice::named("Steven's AirPods"),
        },
        sink,
    };
    let printed = format!("{command:?}");
    assert_eq!(printed, "Request(begin from 7)");
    assert!(!printed.contains("secret-model"));
}

/// The three model-store codes that mean something different to a person, and
/// one catch-all — a wider mapping would put a store's internal vocabulary on a
/// wire a window branches on.
#[test]
fn a_model_store_failure_becomes_one_of_four_session_codes() {
    use koru_voice_models::ModelErrorCode;
    let code = |kind: ModelErrorCode| model_error_code(&kind.with("why"));
    assert_eq!(code(ModelErrorCode::NotInstalled), ErrorCode::ModelMissing);
    assert_eq!(
        code(ModelErrorCode::EngineMismatch),
        ErrorCode::EngineFailed
    );
    assert_eq!(
        code(ModelErrorCode::LanguageUnsupported),
        ErrorCode::LanguageUnsupported
    );
    assert_eq!(code(ModelErrorCode::Io), ErrorCode::ModelLoadFailed);
    assert_eq!(
        code(ModelErrorCode::DigestMismatch),
        ErrorCode::ModelLoadFailed
    );
}

/// **The enabled build's proof that a person can dictate.** See its own note:
/// it is the only place in this tree where a waveform becomes a transcript.
mod dictating;
