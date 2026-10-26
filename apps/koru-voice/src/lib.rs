// SPDX-License-Identifier: MPL-2.0

//! **The Koru voice worker.** It opens the microphone, hosts a speech engine,
//! and performs the effects `koru_voice::Session` emits.
//!
//! ```text
//!                      koru-agent.exe
//!                    supervises, restarts
//!                            |
//!   microphone --> koru-voice-worker.exe -- koru-ipc --> koru.exe
//!                  capture callback                       transcripts
//!                  ring buffer                            a level bucket
//!                  koru_voice::Session
//!                  SpeechEngine
//!                  model store
//!        audio never leaves this process
//! ```
//!
//! ## Why this is a process of its own
//!
//! Three reasons, and each one on its own would be enough
//! ([ADR-249](../../../docs/decisions/ADR-226-250.md)). A native crash in an
//! ONNX runtime would take the window with it. Several hundred megabytes of
//! model memory are only *genuinely* released by ending a process. And a thread
//! that answers a person never waits on the world, which a
//! several-hundred-megabyte load unambiguously is.
//!
//! **Audio never crosses the socket.** The worker opens the device itself and
//! sends back transcripts and a coarse level bucket. That is both the strongest
//! form of the privacy story and the reason `koru_ipc::frame`'s 16 MiB cap is
//! never a question here: the largest message this wire carries is a sentence.
//!
//! ## The three threads, one rule each
//!
//! | | |
//! | --- | --- |
//! | [`server`] | answers messages and does **no inference** |
//! | [`capture`]'s callback | writes into [`ring`] and returns; allocates nothing, blocks on nothing |
//! | [`worker`] | loads the model and runs the engine |
//!
//! An overrun in the middle one drops the **oldest** frames and increments a
//! counter `koru_voice::status` reports, rather than blocking — because a device callback
//! that waited would glitch the whole machine's audio, and the newest audio is
//! what a person is still saying.
//!
//! ## The state machine is not in this binary
//!
//! `koru_voice::Session` is, and this performs its `Effect`s. Milestone 4 hosts
//! the same machine in a phone's process where there is no worker and no
//! socket, so a decision made here would be a decision made twice — and the two
//! would disagree about what a cancelled session does with a late final result.
//!
//! ## The package and binary are both `koru-voice-worker`
//!
//! The `-worker` suffix distinguishes the process package from the
//! `crates/koru-voice` contract package. The shipped file name is shared by
//! `koru_update::exe::VOICE`, release signing/staging, updater quiescence,
//! Agent sibling resolution, launchers, logs, docs, and tests. Koru Agent
//! roadmap Plan 1.1 and `Cargo.toml` name the synchronized inventory.

pub mod bootstrap;
pub mod capture;
pub mod cli;
pub mod control;
pub mod engine;
pub mod idle;
pub mod log;
pub mod models;
pub mod resample;
pub mod ring;
pub mod server;
pub mod worker;

/// **The whole of `main`.**
///
/// Parse, dispatch, and — for `serve` — bind the endpoint, start the inference
/// thread, start the accept loop, and then block until something says to stop.
///
/// The main thread blocks on the shutdown channel rather than joining a thread
/// or calling `process::exit`, for one reason: **it returns**, which drops the
/// `koru_log::Logging` guard and flushes the non-blocking file writer. A
/// `process::exit` here would silently lose whatever the last few lines were,
/// which on the idle-exit path is the only record that the exit was orderly.
pub fn run() -> std::process::ExitCode {
    let invocation = match cli::parse(std::env::args()) {
        Ok(invocation) => invocation,
        Err(rejected) => rejected.report(),
    };
    match invocation.command {
        cli::Command::Help => {
            print!("{}", cli::help());
            return std::process::ExitCode::SUCCESS;
        }
        cli::Command::Version => {
            println!("{}", cli::version());
            return std::process::ExitCode::SUCCESS;
        }
        _ => {}
    }

    let Some(root) = invocation.root.clone().or_else(koru_channel::store_root) else {
        eprintln!("koru-voice-worker: this build could not determine the channel's root");
        return std::process::ExitCode::FAILURE;
    };

    match invocation.command {
        cli::Command::Status => report(koru_voice_client::ask::status(&root)),
        cli::Command::Stop => match koru_voice_client::ask::stop(&root) {
            // Already gone is the state `stop` names, so it is a success and it
            // says so rather than printing a report about a worker that is not
            // there.
            koru_voice_client::ask::Asked::NotRunning => {
                println!("not running");
                std::process::ExitCode::SUCCESS
            }
            asked => report(asked),
        },
        // **The report is the answer, not an acknowledgement.** A worker that
        // refused because a dictation is running answers with `loaded yes`,
        // which is the one line that says what actually happened.
        cli::Command::Release => report(koru_voice_client::ask::release(&root)),
        cli::Command::Serve => serve(&root, &invocation),
        cli::Command::Help | cli::Command::Version => unreachable!("answered above"),
    }
}

fn report(asked: koru_voice_client::ask::Asked) -> std::process::ExitCode {
    match asked {
        koru_voice_client::ask::Asked::Answered(report) => {
            print!("{}", report.render());
            std::process::ExitCode::SUCCESS
        }
        koru_voice_client::ask::Asked::NotRunning => {
            println!("not running");
            std::process::ExitCode::SUCCESS
        }
        koru_voice_client::ask::Asked::Failed(why) => {
            eprintln!("koru-voice-worker: {why}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// **The kill switch, answered before anything is opened.**
///
/// `KORU_VOICE_DISABLED` means this installation does not offer dictation, and
/// the predicate is `koru_voice::disabled()` rather than a second reading of
/// the variable — four components have to agree about it and the one that
/// disagrees is the one that binds an endpoint nothing will dial.
///
/// It is checked here rather than left to the supervisor because a worker is
/// startable by hand, and "no worker starts" has to be true of the way somebody
/// actually starts one. `None` when voice is on, so the caller reads as a
/// guard.
fn stand_down_notice() -> Option<&'static str> {
    koru_voice::disabled()
        .then_some("koru-voice-worker: dictation is off (KORU_VOICE_DISABLED); no worker started")
}

/// Run the worker until something stops it.
fn serve(root: &std::path::Path, invocation: &cli::Invocation) -> std::process::ExitCode {
    // **Before the bootstrap, the log file, the endpoint and the model cache.**
    // A disabled feature must not create a directory, claim a name or read a
    // pipe; and standing down is a SUCCESS for the same reason `AlreadyBound`
    // is — a supervisor reading a failure would treat it as a crash loop and
    // restart a worker that has just explained why it will not run.
    if let Some(notice) = stand_down_notice() {
        eprintln!("{notice}");
        return std::process::ExitCode::SUCCESS;
    }
    // **The bootstrap is read before logging starts**, because it arrives on
    // stdin and a supervisor closes that pipe immediately after writing; a
    // worker that opened a log file first would be fine today and would be
    // reading a closed pipe the first time logging got slower.
    let bootstrap = if invocation.supervised {
        match bootstrap::read(std::io::stdin()) {
            Ok(bootstrap) => Some(bootstrap),
            Err(why) => {
                eprintln!("koru-voice-worker: {why}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let _logging = log::init(root, invocation.console);

    if let Some(bootstrap) = &bootstrap {
        if let Err(why) = bootstrap::validate(bootstrap, root, koru_channel::segment()) {
            tracing::error!("{why}");
            eprintln!("koru-voice-worker: {why}");
            return std::process::ExitCode::FAILURE;
        }
    }

    // **Bind before anything else that costs.** A second worker on this channel
    // and root has to fail here, before it has opened a model cache or touched
    // a device — which is what makes the exclusion an operating-system fact
    // rather than a race this code has to win.
    let listener = match server::bind(root) {
        Ok(listener) => listener,
        Err(error) => {
            // `AlreadyBound` is not a fault: whoever wanted a worker there has
            // one, and standing down quietly is the right answer.
            let already = matches!(error, koru_ipc::IpcError::AlreadyBound(_));
            tracing::info!(
                code = error.code(),
                "the voice endpoint could not be claimed"
            );
            if already {
                eprintln!("koru-voice-worker: another voice worker already holds this endpoint");
                return std::process::ExitCode::SUCCESS;
            }
            eprintln!("koru-voice-worker: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut config = match worker::WorkerConfig::product(Some(root)) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(code = ?error.code, "the speech model cache could not be opened");
            eprintln!("koru-voice-worker: the model cache could not be opened: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    config.supervisor_launch_id = bootstrap.map(|bootstrap| bootstrap.launch_id);

    let (shutdown, stopped) = worker::Shutdown::channel();
    let handle = worker::spawn(config, shutdown.clone());
    let accepting = std::thread::Builder::new()
        .name("koru-voice-worker-ipc".into())
        .spawn(move || server::serve(listener, handle, shutdown));
    if let Err(error) = accepting {
        eprintln!("koru-voice-worker: the listening thread could not be started: {error}");
        return std::process::ExitCode::FAILURE;
    }

    // Every sender being dropped means both threads have gone, which is the
    // same instruction as a signal.
    let _ = stopped.recv();
    tracing::info!("the voice worker has stopped");
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod tests;
