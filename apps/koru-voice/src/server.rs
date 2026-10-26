// SPDX-License-Identifier: MPL-2.0

//! **The IPC thread: it answers messages and does no inference.**
//!
//! One rule, and it is the whole of this file's design. Every message that
//! arrives here becomes a [`crate::worker::Command`] and leaves; nothing on
//! this side of the channel loads a model, opens a device or holds a session.
//! A thread that answers a person never waits on the world, and this is the
//! thread that answers.
//!
//! ```text
//! bind    koru_ipc::VOICE on this root      <- nothing else may hold it
//! accept  ...one connection thread each
//!         ClientHello  -> Welcome | Refused
//!         Request      -> the inference thread
//!         Control      -> the inference thread
//!         Event        <- a writer thread, from the inference thread
//! ```
//!
//! ## Binding is the claim, and connecting is the liveness check
//!
//! Both come free from `koru-ipc` and both are load-bearing here. A second
//! worker on this channel and root fails at *bind*, before it has touched
//! anything — which is what makes "one dictation at a time per installation" an
//! operating-system fact rather than a lock this code has to keep in step. And
//! `koru-voice-worker status` finds a worker by connecting to it, so there is no file
//! that can say one is running when it is not.
//!
//! **Per installation, not per machine.** Stable, beta, nightly and a rooted
//! test build each derive their own endpoint and therefore their own worker.
//! That is deliberate — a lock spanning them would let a throwaway dev build
//! block a real session — and it means two installations capturing at once
//! produce an OS-level device conflict, which surfaces as
//! `koru_voice::ErrorCode::CaptureFailed` rather than as arbitration this
//! process pretends to do.
//!
//! ## Two threads per connection, and why not one
//!
//! A reader parked on a blocking read cannot also write, and the events a
//! dictation produces arrive from the inference thread rather than in reply to
//! anything. So each connection gets a reader and a writer over
//! `koru_ipc::split`, and the inference thread holds only the writer's
//! `Sender`. The alternative — the inference thread writing to the socket
//! itself — would put a syscall that can block on a full pipe onto the thread
//! that is decoding audio.
//!
//! ## An unknown frame version closes; an unknown protocol version answers
//!
//! The distinction is `koru_voice::wire`'s and it is worth keeping: a refusal
//! is itself a frame, so writing one in a format the peer has just said it does
//! not share is a promise that cannot be kept.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use koru_ipc::{Endpoint, Listener, Stream};
use koru_voice::{ClientHello, FRAME_VERSION, PROTOCOL_VERSION, WorkerGreeting};

use crate::worker::{Command, Shutdown, WorkerHandle};
use koru_voice::control::{Control, Incoming, Outgoing};

/// How long a `status` may wait for the inference thread.
///
/// It can be mid-utterance with a decoder holding the loop, so this is not
/// instant; it is bounded because the point of a diagnostic is to answer.
const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Claim the endpoint for this channel and root.
///
/// Fails when something already holds it, which is the answer a second worker
/// should get. `Endpoint::for_service_root` gives the channel's own root the
/// same unqualified name a plain client dials and appends a digest for any
/// other — so a rooted worker and a rooted client meet, and a rooted client and
/// an unrooted worker find nothing.
pub fn bind(root: &std::path::Path) -> Result<Listener, koru_ipc::IpcError> {
    let endpoint =
        Endpoint::for_service_root(crate::control::SERVICE, koru_channel::segment(), root)?;
    Listener::bind(endpoint)
}

/// Accept connections until the process ends. Blocks; runs on its own thread.
pub fn serve(listener: Listener, worker: WorkerHandle, shutdown: Shutdown) {
    // The service and the channel, never `endpoint.name()`: on Unix that name
    // is a filesystem path, which docs/contributing/operations/LOGGING.md names beside
    // credentials, and on Windows it carries the account SID. What a reader
    // needs is which service on which channel, and that is both of them.
    tracing::info!(
        service = listener.endpoint().service(),
        channel = listener.endpoint().segment(),
        "the voice worker is listening"
    );
    let next_client = Arc::new(AtomicU64::new(1));
    loop {
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(error) => {
                // One failed accept is not a reason to stop answering — a peer
                // that hung up between connect and accept produces exactly
                // this — but a listener that has genuinely gone would spin, so
                // it is reported and the loop continues at the OS's pace.
                tracing::warn!(%error, "a voice connection could not be accepted");
                continue;
            }
        };
        // **Same user, or nothing.** The endpoint's own security descriptor
        // already restricts it, and this is the second half: an endpoint is a
        // well-known name on a shared machine, and the cost of being wrong here
        // is somebody else's process listening to this one's microphone.
        if !koru_ipc::same_user(&stream) {
            tracing::warn!("a voice connection from another user was refused");
            continue;
        }
        let client = next_client.fetch_add(1, Ordering::Relaxed);
        let worker = worker.clone();
        let shutdown = shutdown.clone();
        if let Err(error) = std::thread::Builder::new()
            .name(format!("koru-voice-worker-conn-{client}"))
            .spawn(move || connection(client, stream, worker, shutdown))
        {
            tracing::warn!(%error, "a voice connection thread could not be started");
        }
    }
}

/// One client, from its hello to its socket closing.
fn connection(client: u64, stream: Stream, worker: WorkerHandle, shutdown: Shutdown) {
    let (mut reader, mut writer) = koru_ipc::split(stream);

    let (version, hello) = match koru_ipc::frame::read_frame::<_, ClientHello>(&mut reader) {
        Ok(read) => read,
        Err(error) => {
            tracing::debug!(client, %error, "a voice client's hello could not be read");
            return;
        }
    };
    if version != FRAME_VERSION {
        // No answer at all: a refusal is a frame, and one written in a format
        // the peer has just shown it does not share is a promise that cannot be
        // kept.
        tracing::warn!(
            client,
            theirs = version,
            ours = FRAME_VERSION,
            "a voice client speaks a frame version this worker cannot read"
        );
        return;
    }
    if hello.protocol_version != PROTOCOL_VERSION {
        // Both numbers, always: "incompatible" alone sends somebody to read two
        // changelogs to find out which end is behind.
        let refusal = WorkerGreeting::Refused {
            what: "protocol".into(),
            ours: PROTOCOL_VERSION,
            theirs: hello.protocol_version,
            message: format!(
                "this worker speaks voice protocol {PROTOCOL_VERSION} and the client speaks {}",
                hello.protocol_version
            ),
        };
        let _ = koru_ipc::frame::write_frame(&mut writer, FRAME_VERSION, &refusal);
        tracing::warn!(
            client,
            theirs = hello.protocol_version,
            ours = PROTOCOL_VERSION,
            "a voice client was refused on the protocol version"
        );
        return;
    }
    // `product` is a label for this log — `koru`, a bench, a test harness —
    // and never a person and never a device.
    tracing::debug!(
        client,
        product = hello.product.as_str(),
        "a voice client connected"
    );
    let welcome = WorkerGreeting::Welcome {
        worker_version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    if koru_ipc::frame::write_frame(&mut writer, FRAME_VERSION, &welcome).is_err() {
        return;
    }

    // The writer thread. It exists so the inference thread never writes to a
    // socket: a full pipe blocks, and the thread decoding audio may not.
    let (sink, outbound) = std::sync::mpsc::channel::<Outgoing>();
    let writer_thread = std::thread::Builder::new()
        .name(format!("koru-voice-worker-out-{client}"))
        .spawn(move || {
            while let Ok(message) = outbound.recv() {
                if koru_ipc::frame::write_frame(&mut writer, FRAME_VERSION, &message).is_err() {
                    break;
                }
            }
        });
    if let Err(error) = writer_thread {
        tracing::warn!(client, %error, "a voice writer thread could not be started");
        return;
    }

    loop {
        let message = match koru_ipc::frame::read_frame::<_, Incoming>(&mut reader) {
            Ok((version, message)) if version == FRAME_VERSION => message,
            Ok((version, _)) => {
                tracing::warn!(
                    client,
                    version,
                    "a voice frame arrived at an unknown version"
                );
                break;
            }
            // The ordinary end of a connection: the client closed it.
            Err(_) => break,
        };
        match message {
            // **The one request that is a question rather than a dictation.**
            // It never reaches the session machine: no session is armed, no
            // generation is taken, and the answer carries the request's own
            // correlation pair straight back. See `koru_voice::wire`.
            Incoming::Session(koru_voice::Request::Devices {
                session,
                generation,
            }) => {
                let devices = worker.devices(STATUS_TIMEOUT).unwrap_or_default();
                if sink
                    .send(Outgoing::Event(koru_voice::Event::Devices {
                        session,
                        generation,
                        devices,
                    }))
                    .is_err()
                {
                    break;
                }
            }
            Incoming::Session(request) => {
                if !worker.send(Command::Request {
                    client,
                    request,
                    sink: sink.clone(),
                }) {
                    break;
                }
            }
            Incoming::Control(Control::Status) => {
                let Some(report) = worker.status(STATUS_TIMEOUT) else {
                    break;
                };
                if sink.send(Outgoing::Status(report)).is_err() {
                    break;
                }
            }
            Incoming::Control(Control::Release) => {
                // The unload happens on the inference thread, which is the only
                // thread that owns the engine; this one waits for the snapshot
                // that says whether it did.
                let Some(report) = worker.release(STATUS_TIMEOUT) else {
                    break;
                };
                if sink.send(Outgoing::Status(report)).is_err() {
                    break;
                }
            }
            Incoming::Control(Control::Stop) => {
                // Answer first, so `koru-voice-worker stop` can say what it stopped,
                // and only then ask the worker to end.
                if let Some(report) = worker.status(STATUS_TIMEOUT) {
                    let _ = sink.send(Outgoing::Status(report));
                }
                worker.send(Command::Stop);
                shutdown.signal();
                break;
            }
        }
    }
    // **An interruption discards.** Telling the worker is what turns a closed
    // socket into a cancelled dictation rather than a session nothing will ever
    // finish.
    worker.send(Command::Disconnected(client));
    tracing::debug!(client, "a voice client disconnected");
}

#[cfg(test)]
mod tests;
