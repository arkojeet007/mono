// SPDX-License-Identifier: MPL-2.0

//! **The whole stack over a real endpoint**, because the handshake and the two
//! control verbs are the parts nothing else exercises.
//!
//! No process is launched: the worker, the listener and the client are three
//! sets of threads in this one. What is real is the `koru-ipc` endpoint — a
//! named pipe on Windows, a socket elsewhere — so the framing, the greeting and
//! the untagged message split are proved rather than assumed.
//!
//! **Every root here is a fresh uuid path that no worker has ever bound**, and
//! it is never created on disk: `Endpoint::for_service_root` only hashes the
//! path. That is what makes `a_client_finds_nothing_on_a_root_no_worker_bound`
//! legitimate — AGENTS.md's *never assert that a connection fails* is about
//! asserting the absence of somebody else's service, and this arms the state it
//! tests instead.

use std::path::PathBuf;

use super::*;
use crate::engine::Script;
use crate::fixtures;
use crate::worker::spawn;
use koru_voice_client::ask::{self, Asked};

/// A path nothing has ever bound and nothing will create.
fn unbound_root() -> PathBuf {
    std::env::temp_dir().join(format!("koru-voice-test-{}", uuid::Uuid::new_v4()))
}

/// A worker listening on its own root, and the shutdown receiver it will signal.
fn worker_on(root: &std::path::Path) -> std::sync::mpsc::Receiver<()> {
    let listener = bind(root).expect("the voice endpoint could not be claimed");
    let (shutdown, stopped) = Shutdown::channel();
    let script = Script::new().finishing_with("over the socket");
    let handle = spawn(
        fixtures::config(&script, fixtures::store()),
        shutdown.clone(),
    );
    std::thread::spawn(move || serve(listener, handle, shutdown));
    stopped
}

/// **The handshake, and a control verb over it.** A client says hello, is
/// welcomed, asks what the worker is doing, and is told — through the same
/// untagged `Incoming`/`Outgoing` split a dictation uses.
#[test]
fn a_client_completes_the_handshake_and_can_ask_for_status() {
    let root = unbound_root();
    let _stopped = worker_on(&root);

    match ask::status(&root) {
        Asked::Answered(report) => {
            assert_eq!(report.protocol_version, PROTOCOL_VERSION);
            assert_eq!(report.channel, koru_channel::segment());
            assert_eq!(report.engine, "scripted");
            assert_eq!(report.sample_rate, koru_voice::SAMPLE_RATE);
            assert_eq!(report.state, koru_voice::State::Idle);
            assert_eq!(report.process_id, std::process::id());
        }
        other => panic!("the worker did not answer status: {other:?}"),
    }
}

/// **Both numbers, always.** "Incompatible" alone sends somebody to read two
/// changelogs to find out which end is behind.
#[test]
fn a_client_on_another_protocol_version_is_refused_with_both_numbers() {
    let root = unbound_root();
    let _stopped = worker_on(&root);

    let endpoint = koru_ipc::Endpoint::for_service_root(
        crate::control::SERVICE,
        koru_channel::segment(),
        &root,
    )
    .unwrap();
    let mut stream =
        koru_ipc::connect_timed(&endpoint, std::time::Duration::from_secs(10)).unwrap();
    let hello = ClientHello {
        product: "a-test".into(),
        version: "0".into(),
        protocol_version: PROTOCOL_VERSION + 1,
    };
    koru_ipc::frame::write_frame(&mut stream, FRAME_VERSION, &hello).unwrap();
    let (version, greeting) =
        koru_ipc::frame::read_frame::<_, WorkerGreeting>(&mut stream).unwrap();
    assert_eq!(version, FRAME_VERSION);
    match greeting {
        WorkerGreeting::Refused {
            what, ours, theirs, ..
        } => {
            assert_eq!(what, "protocol");
            assert_eq!(ours, PROTOCOL_VERSION);
            assert_eq!(theirs, PROTOCOL_VERSION + 1);
        }
        other => panic!("a mismatched client was welcomed: {other:?}"),
    }
}

/// **Connecting is the liveness check**, so *not running* is an answer rather
/// than an error — and there is no file that could claim otherwise.
#[test]
fn a_client_finds_nothing_on_a_root_no_worker_bound() {
    let root = unbound_root();
    assert!(
        matches!(ask::status(&root), Asked::NotRunning),
        "something was listening on a root nothing bound"
    );
    assert!(matches!(ask::stop(&root), Asked::NotRunning));
}

/// `stop` answers first and then the worker ends, so the verb can say what it
/// stopped rather than reporting a closed socket.
#[test]
fn stop_answers_and_then_the_worker_ends() {
    let root = unbound_root();
    let stopped = worker_on(&root);

    match ask::stop(&root) {
        Asked::Answered(report) => assert_eq!(report.channel, koru_channel::segment()),
        other => panic!("stop did not report what it stopped: {other:?}"),
    }
    stopped
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("the worker did not end after being asked to stop");
}

/// **Binding is the claim.** A second worker on the same channel and root is
/// refused by the operating system, before it has opened a model cache or
/// touched a device — which is what makes "one dictation at a time per
/// installation" a fact rather than a lock this code has to keep in step.
#[test]
fn a_second_worker_cannot_claim_the_same_endpoint() {
    let root = unbound_root();
    let _stopped = worker_on(&root);
    match bind(&root) {
        Err(koru_ipc::IpcError::AlreadyBound(_)) => {}
        Ok(_) => panic!("two workers bound one endpoint"),
        Err(other) => panic!("the second bind failed for the wrong reason: {other}"),
    }
}

// ------------------------------------------------------------- the microphone

/// **The device query, over the real endpoint.**
///
/// It is a `Request` rather than a `Control` verb — it travels the contract
/// crate's own wire so that `koru-workspace-host`, which cannot name this binary's
/// types, asks the same question the tray does. That makes it the one thing
/// the `Incoming` split has to carry that is neither a control verb nor a
/// dictation, and this is what proves the split carries it.
///
/// The list is the `no-capture` fixture: nothing in a test build enumerates a
/// real host.
#[test]
fn a_client_can_ask_which_input_devices_there_are() {
    let root = unbound_root();
    let _stopped = worker_on(&root);

    let devices = ask_devices(&root);
    assert_eq!(
        devices,
        crate::capture::FIXTURE_DEVICES
            .iter()
            .map(|(name, default)| koru_voice::InputDevice::new(*name, *default))
            .collect::<Vec<_>>(),
        "the worker answered something other than what `capture` can see"
    );
}

/// **A device query starts no session.** It takes no generation, so a
/// dictation asked for afterwards is not refused as stale — which is the
/// failure that would follow from routing it through the session machine.
#[test]
fn asking_for_devices_leaves_the_session_machine_alone() {
    let root = unbound_root();
    let _stopped = worker_on(&root);

    let before = match ask::status(&root) {
        Asked::Answered(report) => report.session.sessions,
        other => panic!("no status: {other:?}"),
    };
    ask_devices(&root);
    match ask::status(&root) {
        Asked::Answered(report) => {
            assert_eq!(
                report.session.sessions, before,
                "a device query was counted as a dictation"
            );
            assert_eq!(report.state, koru_voice::State::Idle);
        }
        other => panic!("no status: {other:?}"),
    }
}

/// One connection, one question, one answer — the same shape `ask.rs` uses for
/// a control verb, written here because a *request* is not a control verb and
/// the CLI has no reason to send one.
fn ask_devices(root: &std::path::Path) -> Vec<koru_voice::InputDevice> {
    use koru_voice::{ClientHello, Event, FRAME_VERSION, Request, SessionId, WorkerGreeting};

    let endpoint = koru_ipc::Endpoint::for_service_root(
        crate::control::SERVICE,
        koru_channel::segment(),
        root,
    )
    .expect("the voice endpoint could not be named");
    let mut stream = koru_ipc::connect_timed(&endpoint, std::time::Duration::from_secs(10))
        .expect("nothing was listening");
    let hello = ClientHello {
        product: "koru-voice-devices-test".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    koru_ipc::frame::write_frame(&mut stream, FRAME_VERSION, &hello).expect("hello");
    match koru_ipc::frame::read_frame::<_, WorkerGreeting>(&mut stream) {
        Ok((_, WorkerGreeting::Welcome { .. })) => {}
        other => panic!("the worker did not welcome the client: {other:?}"),
    }

    let session = SessionId::new();
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &Request::Devices {
            session,
            generation: 7,
        },
    )
    .expect("the device request could not be sent");
    match koru_ipc::frame::read_frame::<_, Outgoing>(&mut stream) {
        Ok((
            _,
            Outgoing::Event(Event::Devices {
                session: answered,
                generation,
                devices,
            }),
        )) => {
            // **The correlation pair comes back unchanged**, which is what
            // lets a client match an answer to the question it asked without
            // arming a dictation.
            assert_eq!(answered, session);
            assert_eq!(generation, 7);
            devices
        }
        other => panic!("the worker did not answer with a device list: {other:?}"),
    }
}
