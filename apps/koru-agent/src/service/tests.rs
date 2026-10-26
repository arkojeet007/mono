// SPDX-License-Identifier: MPL-2.0

//! **The supervision decision, without a process.**
//!
//! [`super::ensure_voice_with`] takes the two effects -- asking, and launching
//! -- as closures precisely so this suite can drive the *decision* while
//! spawning nothing. That is not squeamishness: `ensure_voice` starts
//! `koru-voice-worker` beside this binary on whatever root `Paths` carries,
//! and a unit test that reached it would launch real infrastructure on a
//! made-up root and leave it holding an executable the next build needs to
//! overwrite.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::gateway_host::GatewayServiceError;

#[test]
fn every_voice_host_uses_the_store_service_root() {
    let paths = Paths::for_root(std::path::PathBuf::from("C:/isolated-koru"));
    let root = voice_service_root(&paths);

    assert_eq!(root, std::path::PathBuf::from("C:/isolated-koru/store"));
    assert_ne!(root, paths.explicit_root().unwrap());
}

fn ready(launch_id: Option<&str>) -> VoiceReady {
    VoiceReady {
        worker_version: "0.0.0-test".into(),
        process_id: 4242,
        supervisor_launch_id: launch_id.map(str::to_string),
        launched: false,
    }
}

/// The worker's endpoint belongs under Store, but its model cache does not.
/// Losing this child-only override made a successful rooted install invisible
/// at runtime and every press failed with `model_missing`.
#[test]
fn the_voice_child_carries_the_installers_model_root() {
    let service_root = std::path::Path::new("C:/isolated-koru/store");
    let model_root = std::path::Path::new("C:/isolated-koru/voice/models");
    let command = voice_command(
        std::path::Path::new("C:/bin/koru-voice-worker.exe"),
        service_root,
        model_root,
    );

    assert!(
        command
            .get_args()
            .any(|arg| arg == service_root.as_os_str())
    );
    assert!(command.get_envs().any(|(name, value)| {
        name == koru_env::name::KORU_VOICE_MODELS && value == Some(model_root.as_os_str())
    }));
    assert_eq!(
        command.get_program(),
        std::path::Path::new("C:/bin/koru-voice-worker.exe").as_os_str()
    );
}

/// **Connect first.** A worker that is already there is adopted, and nothing is
/// launched -- which is the ordinary case, because a worker outlives the
/// dictation that started it by five minutes of idle model and half an hour of
/// idle process.
#[test]
fn a_worker_that_is_already_running_is_adopted_rather_than_launched() {
    let launches = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&launches);
    let found = ensure_voice_with(
        None,
        || Ok(Some(ready(None))),
        move |_| {
            counted.fetch_add(1, Ordering::Release);
            Ok(())
        },
    )
    .expect("an already-running worker should be adopted");

    assert_eq!(launches.load(Ordering::Acquire), 0);
    assert!(!found.launched);
    assert_eq!(found.process_id, 4242);
}

/// **Two concurrent asks launch one process.**
///
/// Both threads reach the decision; the lock serialises them; the second one's
/// connect-first check finds what the first one started. Without the lock both
/// mint a launch id and spawn, the endpoint admits one, and the loser reports
/// `voice_incompatible` for a worker that is perfectly good.
#[test]
fn two_concurrent_ensure_calls_launch_one_worker() {
    // The fictional installation: `None` until something launches, then a
    // worker echoing the launch id it was given.
    let running: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let launches = Arc::new(AtomicU64::new(0));

    let ensure = || {
        let running = Arc::clone(&running);
        let launches = Arc::clone(&launches);
        move || {
            let asking = Arc::clone(&running);
            let launching = Arc::clone(&running);
            let counted = Arc::clone(&launches);
            ensure_voice_with(
                None,
                move || {
                    Ok(asking
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                        .map(|id| ready(Some(&id))))
                },
                move |launch_id| {
                    counted.fetch_add(1, Ordering::Release);
                    *launching.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(launch_id.to_string());
                    Ok(())
                },
            )
        }
    };

    let first = std::thread::spawn(ensure());
    let second = std::thread::spawn(ensure());
    let one = first.join().expect("the first ensure panicked");
    let two = second.join().expect("the second ensure panicked");

    one.expect("the first ensure failed");
    two.expect("the second ensure failed");
    assert_eq!(
        launches.load(Ordering::Acquire),
        1,
        "two concurrent asks started two workers"
    );
}

/// **Verification, not decoration.** A worker on this endpoint that was started
/// for somebody else's launch is refused rather than adopted: this Agent does
/// not know what it is talking to, and saying so is the only honest answer.
#[test]
fn a_worker_from_another_launch_is_refused_rather_than_adopted() {
    let launched = Arc::new(std::sync::Mutex::new(false));
    let flag = Arc::clone(&launched);
    let refused = ensure_voice_with(
        None,
        move || {
            if *flag.lock().unwrap_or_else(|error| error.into_inner()) {
                // Somebody else's worker won the bind.
                Ok(Some(ready(Some("a-different-launch"))))
            } else {
                Ok(None)
            }
        },
        {
            let flag = Arc::clone(&launched);
            move |_| {
                *flag.lock().unwrap_or_else(|error| error.into_inner()) = true;
                Ok(())
            }
        },
    )
    .expect_err("a foreign launch should not be reported as a successful one");

    assert_eq!(refused.code(), "voice_incompatible");
}

/// A worker that never binds is a bounded wait and a stable code, not a hang.
#[test]
fn a_worker_that_never_binds_is_reported_rather_than_waited_on_forever() {
    let began = std::time::Instant::now();
    let failed = ensure_voice_with(None, || Ok(None), |_| Ok(()))
        .expect_err("a worker that never appeared should not be a success");

    assert_eq!(failed.code(), "voice_unreachable");
    assert!(
        began.elapsed() < VOICE_START_DEADLINE * 3,
        "the readiness poll ran long past its deadline"
    );
}

/// A binary that is not beside this one is `voice_not_found`, and it says so
/// before anything is spawned.
#[test]
fn a_launch_failure_keeps_its_own_code() {
    let failed = ensure_voice_with(
        None,
        || Ok(None),
        |_| {
            Err(VoiceServiceError::NotFound(
                "no worker beside this build".into(),
            ))
        },
    )
    .expect_err("a missing binary is not a success");
    assert_eq!(failed.code(), "voice_not_found");
}

/// **`KORU_VOICE_DISABLED` refuses before anything is dialled.** Q-V44 names
/// four surfaces that have to agree about the kill switch, and this is
/// supervision's: a switched-off installation starts no worker, and it says
/// *switched off* rather than *not answering* -- which is a different sentence
/// and a different remedy.
///
/// The flag is a parameter rather than the environment, deliberately: the
/// variable is process-global and this suite runs beside thirty others.
/// `koru_voice::availability`'s own tests own the reading of it.
#[test]
fn a_switched_off_installation_supervises_nothing() {
    let launches = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&launches);
    let refused = ensure_voice_with(
        Some(koru_voice::Off::Switch),
        || panic!("a switched-off installation must not even dial"),
        move |_| {
            counted.fetch_add(1, Ordering::Release);
            Ok(())
        },
    )
    .expect_err("a switched-off installation should not report a worker");

    assert_eq!(refused.code(), "voice_disabled");
    assert!(
        refused.to_string().contains(koru_voice::DISABLE_VAR),
        "the refusal should name the variable a person has to unset: {refused}"
    );
    assert_eq!(launches.load(Ordering::Acquire), 0);
}

/// The five codes are distinct from the gateway's, so a log line and a shell
/// branch can tell a dictation that could not start from a store that could
/// not. The gateway's vocabulary moved to `gateway_host` with its supervision
/// in Plan 4; this stays here, beside the codes it is protecting.
#[test]
fn the_voice_codes_are_not_the_gateways() {
    let voice = [
        VoiceServiceError::Disabled(koru_voice::Off::Switch, String::new()).code(),
        VoiceServiceError::NotFound(String::new()).code(),
        VoiceServiceError::Launch(String::new()).code(),
        VoiceServiceError::Unavailable(String::new()).code(),
        VoiceServiceError::Incompatible(String::new()).code(),
    ];
    let gateway = [
        GatewayServiceError::NotFound(String::new()).code(),
        GatewayServiceError::Launch(String::new()).code(),
        GatewayServiceError::Unavailable(String::new()).code(),
        GatewayServiceError::Incompatible(String::new()).code(),
    ];
    for code in voice {
        assert!(code.starts_with("voice_"), "{code} is not a voice code");
        assert!(!gateway.contains(&code), "{code} is spelled twice");
    }
}

/// **The worker fingerprints its own image with its own copy of the digest,
/// and this process fingerprints the same bytes with the bridge's.** A
/// bootstrap carrying a different spelling would be refused at every launch,
/// so both sides assert this same literal -- the BLAKE3 reference vector for
/// "abc" -- rather than each comparing a function with itself.
#[test]
fn the_launch_fingerprint_is_the_one_the_worker_computes() {
    assert_eq!(
        koru_network_bridge::fingerprint(b"abc"),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
}

/// **A platform with no voice runtime refuses the same way, and blames no
/// switch.** Nothing is dialled and nothing is launched; the code is the one a
/// shell already branches on, and the text does not tell a person to unset a
/// variable they never set.
#[test]
fn a_platform_without_voice_supervises_nothing() {
    let refused = ensure_voice_with(
        Some(koru_voice::Off::Platform),
        || panic!("a platform without voice must not even dial"),
        |_| panic!("a platform without voice must not launch a worker"),
    )
    .expect_err("a platform without voice should not report a worker");

    assert_eq!(refused.code(), "voice_disabled");
    assert!(
        !refused.to_string().contains(koru_voice::DISABLE_VAR),
        "a platform refusal must not name the switch: {refused}"
    );
}
