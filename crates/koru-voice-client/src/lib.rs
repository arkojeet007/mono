// SPDX-License-Identifier: MPL-2.0

//! **Dials the voice worker, keeps the connection alive, and wakes its host.**
//!
//! ```text
//!   host -- begin / finish / cancel --> VoiceClient -- one send path --> koru-voice-worker.exe
//!   host <-------- drain() ------------ inbox  <-- reader thread <------ Event frames
//!                       ^                            |
//!                       +------------ wake ----------+
//! ```
//!
//! ## It names no widget, and that is what keeps `winit` out of it
//!
//! Waking is a closure the host installs ([`VoiceClient::install_wake`]). In
//! Koru that closure is `proxy.send_event(..)` on an `EventLoopProxy`, which is
//! the pattern `apps/koru/src/agent.rs` already uses for the Agent's reader
//! thread; in a bench it is a channel send, and on a phone there is no socket
//! and no client at all. **Queue first, wake second**, at every call site: the
//! other order lets a host's idle pass run between the two, find an empty inbox
//! and sleep past the transcript that has just arrived.
//!
//! The reader thread exists so that no caller is ever tempted to poll. A timer
//! that woke an idle window several times a second to ask whether a dictation
//! had said anything would break the one rule this feature may not break.
//!
//! ## One path a request may leave by
//!
//! [`link::Link::send`] is the only place a [`koru_voice::Request`] is written,
//! and reconnection lives inside [`VoiceClient::send`] around it. This is
//! `koru-store-client`'s `send_with_retry` lesson, learned there the expensive
//! way: a dozen methods each built their own request from a copy of the address
//! frozen at construction, so a service that restarted on a new port left every
//! one of them talking to nothing. Here the endpoint is **re-derived on every
//! attempt** from the channel and the root, never held; a worker that exited
//! after its idle timeout and was started again is dialled afresh by the next
//! `begin`, with no restart of the host and nothing for a caller to notice.
//!
//! `a_request_leaves_by_exactly_one_path` walks this crate's own source and
//! fails if a second `write_frame` appears anywhere.
//!
//! ## It does not start the worker, and it does not know what an Agent is
//!
//! Starting a process is a decision about a product
//! ([ADR-105](../../../docs/decisions/ADR-101-125.md)), and this crate's whole
//! charter is that it dials. So when nothing is listening it calls the
//! **ensure** closure the host installed -- in Koru that is
//! `EnsureService { service: Voice }` to `koru-agent`, which is the process
//! that owns supervision -- and then polls the endpoint for a bounded moment.
//! This crate links neither `koru-agent-wire` nor `koru-agent`.
//!
//! ## Generations are minted from the clock, not from one
//!
//! The worker refuses a `begin` whose generation is not strictly greater than
//! every generation it has run, which is what makes a reconnecting client's
//! replayed request harmless. Two clients of one worker therefore may not both
//! count from one: the second window to dictate, once the first had finished,
//! would be told [`ErrorCode::StaleGeneration`] rather than being allowed to
//! speak. So the counter is seeded from the wall clock in microseconds and
//! incremented from there -- monotonic within a client, and distinct between
//! clients not constructed in the same microsecond.
//!
//! ## Nothing here may block the host's loop
//!
//! Every read is on the reader thread and every write goes through a queue a
//! writer thread drains, so the loop only ever touches a mutex that is never
//! held across anything slower than a `push`. Begin, finish and cancel enqueue
//! onto one client-owned transport worker; that worker alone may dial, ensure,
//! retry and handshake. A terminal transport failure comes back as the same
//! generation-tagged `Ended(WorkerUnavailable)` event a dead reader produces.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub mod ask;
pub mod link;

pub use koru_voice::{
    DISABLE_VAR, EndReason, ErrorCode, Event, InputChoice, InputDevice, InputResolution,
    LevelBucket, ModelId, ModelTier, Off, Requested, SessionId, State, Transcript, VoiceError,
    VoiceLanguage, available, disabled, off,
};

/// How long to wait for a worker that is being started.
///
/// The host has just asked its Agent to ensure one, and an agent spawns a
/// process that then has to bind before it can be dialled -- so the wait is
/// real rather than a courtesy. Bounded, because a person is holding a key down
/// and a dictation that never begins has to say so.
pub const START_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How often to re-dial while waiting.
pub const RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(25);

/// **How long [`VoiceClient::devices`] waits for a list.**
///
/// The worker answers this from its inference thread, which can be inside a
/// decode -- so the wait is real, and it is bounded for the same reason
/// `koru-voice-worker status` bounds its own: the point of a question is to be
/// answered. Five seconds is the worker's own `STATUS_TIMEOUT`, taken rather
/// than invented, because it is the same thread and the same worst case.
pub const DEVICES_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Nudge the host's loop. See the module note on why this is a closure.
pub type Wake = Box<dyn Fn() + Send + Sync>;

/// Ask whoever owns supervision to make sure a worker is running.
///
/// Called from inside the one send path, and only when nothing is listening. It
/// returns nothing: the answer that matters is whether the endpoint answers
/// afterwards, which the poll finds out for itself.
pub type Ensure = Box<dyn Fn() + Send + Sync>;

/// The dictation this client believes is running.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Live {
    pub session: SessionId,
    pub generation: u64,
}

/// State shared with the reader and writer threads.
pub(crate) struct Shared {
    /// **A root, not an address.** The endpoint is derived from it on every
    /// attempt; see the module note.
    pub(crate) root: Option<PathBuf>,
    /// The label this client gives the worker's log. Never a person, never a
    /// device.
    pub(crate) product: String,
    inbox: Mutex<VecDeque<Event>>,
    wake: Mutex<Option<Wake>>,
    pub(crate) ensure: Mutex<Option<Ensure>>,
    live: Mutex<Option<Live>>,
    live_link: Mutex<Option<(Live, u64)>>,
    /// **Questions waiting for an answer, which are not dictations.**
    ///
    /// A device query carries a session id as a correlation id and never arms
    /// [`Shared::live`] -- see [`VoiceClient::devices`]. The reader looks here
    /// *before* the generation filter, because that filter is about which
    /// dictation an event belongs to and a query belongs to none.
    pub(crate) queries: Mutex<HashMap<SessionId, std::sync::mpsc::Sender<Vec<InputDevice>>>>,
    generation: AtomicU64,
    link: Mutex<Option<(u64, link::Link)>>,
    link_epoch: AtomicU64,
    next_link: AtomicU64,
    /// Set only when the last public client handle is dropped. The transport
    /// worker checks it around every blocking boundary so queued work cannot
    /// open a microphone after its owner has gone away.
    shutting_down: std::sync::atomic::AtomicBool,
    /// The last code this client produced or was told, for a diagnostic. A
    /// token, never prose, and never a transcript.
    last_error: Mutex<Option<ErrorCode>>,
    #[cfg(test)]
    test_hooks: Mutex<TestTransportHooks>,
}

#[cfg(test)]
#[derive(Default)]
struct TestTransportHooks {
    after_dequeue: Option<Arc<TestPause>>,
    before_send: Option<Arc<TestPause>>,
    after_write: Option<Arc<TestPause>>,
    command_done: Option<std::sync::mpsc::Sender<()>>,
}

#[cfg(test)]
struct TestPause {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
impl TestPause {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reached: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        })
    }

    fn wait_until_reached(&self) {
        self.reached.wait();
    }

    fn release(&self) {
        self.release.wait();
    }

    fn pause_worker(&self) {
        self.reached.wait();
        self.release.wait();
    }
}

#[derive(Clone)]
struct TransportCommand {
    request: koru_voice::Request,
    terminal: Option<Live>,
    epoch: u64,
}

impl Shared {
    #[cfg(test)]
    fn test_pause(
        &self,
        take: impl FnOnce(&mut TestTransportHooks) -> &mut Option<Arc<TestPause>>,
    ) {
        let pause = {
            let mut hooks = self
                .test_hooks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            take(&mut hooks).take()
        };
        if let Some(pause) = pause {
            pause.pause_worker();
        }
    }

    #[cfg(test)]
    fn test_command_done(&self) {
        if let Some(done) = self
            .test_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .command_done
            .take()
        {
            let _ = done.send(());
        }
    }

    /// Queue an event, then wake -- in that order, always.
    pub(crate) fn deliver(&self, event: Event) {
        self.inbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(event);
        self.wake();
    }

    pub(crate) fn wake(&self) {
        if let Some(wake) = self
            .wake
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            wake();
        }
    }

    pub(crate) fn live(&self) -> Option<Live> {
        *self.live.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn clear_live(&self) {
        *self.live.lock().unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .live_link
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    fn take_live(&self, expected: Live) -> bool {
        let mut held = self.live.lock().unwrap_or_else(|error| error.into_inner());
        if *held != Some(expected) {
            return false;
        }
        *held = None;
        true
    }

    pub(crate) fn current_link(&self, id: u64) -> bool {
        self.link
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
    }

    /// Bind a successfully written Begin to the link that carried it.
    ///
    /// The link check and binding are one critical section. Otherwise a reader
    /// EOF can retire `id` between them and leave a live generation bound to a
    /// link that no longer exists.
    fn bind_live_if_current(&self, id: u64, live: Live) -> bool {
        let link = self.link.lock().unwrap_or_else(|error| error.into_inner());
        if !link.as_ref().is_some_and(|(current, _)| *current == id) {
            return false;
        }
        let current = self.live.lock().unwrap_or_else(|error| error.into_inner());
        if *current != Some(live) {
            return false;
        }
        *self
            .live_link
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((live, id));
        true
    }

    pub(crate) fn retire_link(&self, id: u64, expected: Option<Live>) {
        let retired = {
            let mut held = self.link.lock().unwrap_or_else(|error| error.into_inner());
            if held.as_ref().is_some_and(|(current, _)| *current == id) {
                *held = None;
                true
            } else {
                false
            }
        };
        if !retired {
            return;
        }
        let bound = {
            let mut binding = self
                .live_link
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match *binding {
                Some((live, link)) if link == id => {
                    *binding = None;
                    Some(live)
                }
                _ => None,
            }
        };
        let live = expected.or(bound);
        if let Some(live) = live
            && self.take_live(live)
        {
            self.note(ErrorCode::WorkerUnavailable);
            self.deliver(Event::Ended {
                session: live.session,
                generation: live.generation,
                reason: EndReason::Failed {
                    code: ErrorCode::WorkerUnavailable,
                    message: "the voice worker connection ended before the command completed"
                        .into(),
                },
            });
        } else {
            self.wake();
        }
    }

    /// Hand a device answer to whoever asked for it. `false` when nobody is
    /// waiting any more, which is the ordinary fate of an answer that arrived
    /// after its timeout.
    pub(crate) fn answer_query(&self, session: SessionId, devices: Vec<InputDevice>) -> bool {
        let waiting = self
            .queries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&session);
        match waiting {
            Some(tx) => tx.send(devices).is_ok(),
            None => false,
        }
    }

    pub(crate) fn note(&self, code: ErrorCode) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(code);
    }
}

/// **The client.** One per host process; cheap to clone, because everything is
/// behind one `Arc`.
#[derive(Clone)]
pub struct VoiceClient {
    shared: Arc<Shared>,
    transport: Arc<Mutex<Option<std::sync::mpsc::Sender<TransportCommand>>>>,
}

impl VoiceClient {
    /// A client for the worker on this channel's own root.
    pub fn new() -> VoiceClient {
        VoiceClient::at(None)
    }

    /// A client for the worker on an explicit root -- `--root`, or a test's
    /// temporary directory.
    ///
    /// **A rooted client and an unrooted worker find nothing rather than each
    /// other**, and that is the point: `Endpoint::for_service_root` qualifies
    /// the name with a digest of any root that is not the channel's own, so a
    /// suite cannot reach -- or stop -- the worker somebody is dictating
    /// through.
    pub fn at(root: Option<PathBuf>) -> VoiceClient {
        let shared = Arc::new(Shared {
            root,
            product: "koru".into(),
            inbox: Mutex::new(VecDeque::new()),
            wake: Mutex::new(None),
            ensure: Mutex::new(None),
            live: Mutex::new(None),
            live_link: Mutex::new(None),
            queries: Mutex::new(HashMap::new()),
            // Seeded from the clock so two clients of one worker do not
            // collide on the worker's strictly-increasing rule. See the
            // module note.
            generation: AtomicU64::new(clock_micros()),
            link: Mutex::new(None),
            link_epoch: AtomicU64::new(0),
            next_link: AtomicU64::new(0),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            last_error: Mutex::new(None),
            #[cfg(test)]
            test_hooks: Mutex::new(TestTransportHooks::default()),
        });
        VoiceClient {
            shared,
            transport: Arc::new(Mutex::new(None)),
        }
    }

    /// The label this client gives the worker's log -- `koru`, a bench, a test
    /// harness. Never a person and never a device.
    pub fn as_product(mut self, product: &str) -> VoiceClient {
        Arc::get_mut(&mut self.shared)
            .expect("as_product is called before the client is shared")
            .product = product.to_string();
        self
    }

    /// Give the client the way to nudge the host's loop. Installed once the
    /// loop exists, which is after construction in every host that has one.
    pub fn install_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .wake
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Box::new(wake));
    }

    /// Give the client the way to ask for a worker when none is listening.
    /// Without one, a dictation attempted with no worker running fails with
    /// [`ErrorCode::WorkerUnavailable`] rather than starting anything.
    pub fn install_ensure(&self, ensure: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .ensure
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(Box::new(ensure));
    }

    /// Begin one dictation, connecting if this is the first.
    ///
    /// The session id is minted here and the generation comes off this client's
    /// counter, so the caller has both before the request has left -- which is
    /// what lets a host arm its composition and then discard events that do not
    /// belong to it.
    pub fn begin(
        &self,
        language: Requested,
        model: Option<ModelId>,
        input: InputChoice,
    ) -> Result<Live, VoiceError> {
        self.begin_with_tier(language, koru_voice::ModelTier::default(), model, input)
    }

    /// Begin with the person's explicit quality preference.
    pub fn begin_with_tier(
        &self,
        language: Requested,
        tier: koru_voice::ModelTier,
        model: Option<ModelId>,
        input: InputChoice,
    ) -> Result<Live, VoiceError> {
        let live = Live {
            session: SessionId::new(),
            generation: self.next_generation(),
        };
        // **Armed before the request leaves.** If the socket dies between here
        // and the worker's first event, the reader's end-of-stream is what
        // turns that into an `Ended` the host can act on -- and it can only do
        // that for a session it knows about.
        *self
            .shared
            .live
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(live);
        match self.enqueue(
            koru_voice::Request::Begin {
                session: live.session,
                generation: live.generation,
                language,
                tier,
                model,
                input,
            },
            Some(live),
        ) {
            Ok(()) => Ok(live),
            Err(error) => {
                self.shared.clear_live();
                Err(error)
            }
        }
    }

    /// Stop capturing and ask for the final result. The composition stays
    /// provisional until the matching `Event::Final` arrives.
    pub fn finish(&self) -> Result<(), VoiceError> {
        let live = self.require_live()?;
        self.enqueue(
            koru_voice::Request::Finish {
                session: live.session,
                generation: live.generation,
            },
            Some(live),
        )
    }

    /// Throw the dictation away. No final, no text, no trace.
    pub fn cancel(&self) -> Result<(), VoiceError> {
        let live = self.require_live()?;
        let sent = self.enqueue(
            koru_voice::Request::Cancel {
                session: live.session,
                generation: live.generation,
            },
            None,
        );
        // **A cancel that could not be sent has still cancelled.** An
        // interruption discards, and a host told its request failed must not be
        // left holding provisional text for a session nothing will ever end.
        self.shared.clear_live();
        sent
    }

    /// **Which microphones can the worker see?** Blocking, bounded by
    /// [`DEVICES_TIMEOUT`], and it starts nothing.
    ///
    /// Three properties, each of them deliberate.
    ///
    /// **It never arms a dictation.** The `session` it mints is a correlation
    /// id, registered in `Shared::queries` and answered out of the reader
    /// before the generation filter -- so asking what the microphones are
    /// while somebody is mid-sentence cannot disturb, replace or end that
    /// sentence.
    ///
    /// **It will not start a worker.** A tray drawing its menu and a settings
    /// page being built are not somebody asking to dictate, and spawning an
    /// inference process to populate a list would be a menu with a cost. A
    /// machine with no worker running therefore answers
    /// [`ErrorCode::WorkerUnavailable`], and the caller draws what it knows --
    /// which is *System default* and whatever name is remembered.
    ///
    /// **It blocks, so it is not called from a loop.** The same rule
    /// `koru_workspace_host::voice_install::installed_models` follows: a surface asks
    /// when it is built or when something has changed, on a thread, never from
    /// a frame.
    pub fn devices(&self) -> Result<Vec<InputDevice>, VoiceError> {
        self.devices_maybe_starting(false)
    }

    /// Which microphones the worker can see, starting it through the host's
    /// ensure route when necessary.
    ///
    /// Settings calls this because opening an input picker is an explicit
    /// request for the live machine list. Passive surfaces use [`Self::devices`]
    /// and retain its no-start contract.
    pub fn devices_starting_worker(&self) -> Result<Vec<InputDevice>, VoiceError> {
        self.devices_maybe_starting(true)
    }

    fn devices_maybe_starting(&self, start: bool) -> Result<Vec<InputDevice>, VoiceError> {
        let session = SessionId::new();
        let generation = self.next_generation();
        let (tx, rx) = std::sync::mpsc::channel();
        self.shared
            .queries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session, tx);
        let sent = send_blocking(
            &self.shared,
            koru_voice::Request::Devices {
                session,
                generation,
            },
            start,
            self.shared.link_epoch.load(Ordering::Acquire),
        );
        let answered = match sent {
            Ok(()) => rx.recv_timeout(DEVICES_TIMEOUT).map_err(|_| {
                ErrorCode::WorkerUnavailable
                    .with("the voice worker did not answer which input devices it can see")
            }),
            Err(error) => Err(error),
        };
        // **Always deregistered**, on every path: a query left in the map is a
        // `Sender` nothing will ever read, and a settings page that is opened
        // often would leak one per visit.
        self.shared
            .queries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&session);
        if let Err(error) = &answered {
            self.shared.note(error.code);
        }
        answered
    }

    /// Everything the worker has said since this was last called.
    ///
    /// Events for a generation this client is not running are already gone --
    /// the reader drops them, which is the same rule the worker applies on the
    /// way out, belt and braces across a socket that can reorder nothing but
    /// can certainly deliver late.
    pub fn drain(&self) -> Vec<Event> {
        self.shared
            .inbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain(..)
            .collect()
    }

    /// The dictation this client believes is running.
    pub fn live(&self) -> Option<Live> {
        self.shared.live()
    }

    /// Whether there is a worker on the other end **right now**. A question
    /// about the link, not a promise about the next request: the send path
    /// reconnects on its own, so `false` here is not a reason for a caller to
    /// do anything.
    pub fn connected(&self) -> bool {
        self.shared
            .link
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|(_, link)| link.alive())
    }

    /// What the worker said it was when this link was made -- its version and
    /// the `launch_id` a supervisor gave it, when there was one. `None` until a
    /// link exists.
    pub fn worker(&self) -> Option<link::Worker> {
        self.shared
            .link
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|(_, link)| link.worker())
    }

    /// The last code this client produced or was told. A diagnostic.
    pub fn last_error(&self) -> Option<ErrorCode> {
        *self
            .shared
            .last_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Drop the link, so the next request dials again. What a host calls when
    /// it is going away; nothing else needs it.
    pub fn disconnect(&self) {
        self.shared.link_epoch.fetch_add(1, Ordering::AcqRel);
        *self
            .shared
            .link
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.shared.clear_live();
    }

    /// The next generation this client will use.
    ///
    /// **One ahead of its own last, and never behind the clock.** The worker
    /// refuses a generation that is not strictly greater than every one it has
    /// run, so two clients of one worker counting from one would mean the
    /// second window to dictate was told its request was stale. Re-reading the
    /// clock on every dictation is what keeps two clients apart without either
    /// of them knowing the other exists: they collide only if they begin in the
    /// same microsecond, and the next dictation on either side re-synchronises.
    fn next_generation(&self) -> u64 {
        let mut current = self.shared.generation.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(1).max(clock_micros());
            match self.shared.generation.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(seen) => current = seen,
            }
        }
    }

    fn require_live(&self) -> Result<Live, VoiceError> {
        self.shared.live().ok_or_else(|| {
            self.shared.note(ErrorCode::NoSession);
            ErrorCode::NoSession.with("this client is not running a dictation")
        })
    }

    fn enqueue(
        &self,
        request: koru_voice::Request,
        terminal: Option<Live>,
    ) -> Result<(), VoiceError> {
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(ErrorCode::WorkerUnavailable.with("the voice client is shutting down"));
        }
        let sender = {
            let mut held = self
                .transport
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if held.is_none() {
                let (sender, commands) = std::sync::mpsc::channel();
                let worker = Arc::clone(&self.shared);
                std::thread::Builder::new()
                    .name("koru-voice-client transport".into())
                    .spawn(move || transport_loop(worker, commands))
                    .map_err(|_| {
                        ErrorCode::WorkerUnavailable
                            .with("the voice transport worker could not start")
                    })?;
                *held = Some(sender);
            }
            held.as_ref().expect("installed above").clone()
        };
        let epoch = self.shared.link_epoch.load(Ordering::Acquire);
        sender
            .send(TransportCommand {
                request,
                terminal,
                epoch,
            })
            .map_err(|_| {
                let error = ErrorCode::WorkerUnavailable
                    .with("the voice client's transport worker is unavailable");
                self.shared.note(error.code);
                error
            })
    }
}

impl Drop for VoiceClient {
    fn drop(&mut self) {
        // The worker owns Shared, but never this owner-only Arc. A strong count
        // of one therefore means no public handle can submit another command.
        if Arc::strong_count(&self.transport) != 1 {
            return;
        }
        self.shared.shutting_down.store(true, Ordering::Release);
        self.shared.link_epoch.fetch_add(1, Ordering::AcqRel);
        *self
            .shared
            .link
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.shared.clear_live();
        self.transport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

fn transport_loop(shared: Arc<Shared>, commands: std::sync::mpsc::Receiver<TransportCommand>) {
    while let Ok(command) = commands.recv() {
        #[cfg(test)]
        shared.test_pause(|hooks| &mut hooks.after_dequeue);
        if shared.shutting_down.load(Ordering::Acquire) {
            #[cfg(test)]
            shared.test_command_done();
            break;
        }
        if let Err(error) = send_blocking(&shared, command.request, true, command.epoch)
            && let Some(live) = command.terminal
            && shared.take_live(live)
        {
            shared.note(error.code);
            shared.deliver(Event::Ended {
                session: live.session,
                generation: live.generation,
                reason: EndReason::Failed {
                    code: error.code,
                    message: "the voice transport could not deliver the session command".into(),
                },
            });
        }
        #[cfg(test)]
        shared.test_command_done();
    }
    shared.link_epoch.fetch_add(1, Ordering::AcqRel);
    *shared
        .link
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    shared.clear_live();
}

/// The sole serialized dial/retry/send effect. Dictation commands reach it on
/// the transport worker; explicitly blocking device queries call it only from
/// their documented off-loop contexts.
fn send_blocking(
    shared: &Arc<Shared>,
    request: koru_voice::Request,
    start: bool,
    command_epoch: u64,
) -> Result<(), VoiceError> {
    let mut last: Option<VoiceError> = None;
    // One command belongs to the epoch in which it was enqueued. A disconnect
    // before the transport worker receives it is still a terminal cancellation
    // boundary, not permission to redial and replay the stale request.
    for _ in 0..2 {
        if shared.shutting_down.load(Ordering::Acquire) {
            return Err(ErrorCode::WorkerUnavailable.with("the voice client is shutting down"));
        }
        if shared.link_epoch.load(Ordering::Acquire) != command_epoch {
            return Err(ErrorCode::WorkerUnavailable
                .with("the voice client disconnected before delivering the command"));
        }
        let mut selected = shared
            .link
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|(_, link)| link.alive())
            .cloned();
        if selected.is_none() {
            let id = shared
                .next_link
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            match link::Link::dial(shared, start, id) {
                Ok(link) => {
                    if shared.shutting_down.load(Ordering::Acquire)
                        || shared.link_epoch.load(Ordering::Acquire) != command_epoch
                    {
                        return Err(ErrorCode::WorkerUnavailable
                            .with("the voice client disconnected while dialling"));
                    }
                    let mut held = shared
                        .link
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if held.as_ref().is_none_or(|(_, current)| !current.alive()) {
                        *held = Some((id, link.clone()));
                    }
                    selected = held.clone();
                }
                Err(error) => {
                    last = Some(error);
                    continue;
                }
            }
        }
        let Some((id, link)) = selected else { continue };
        #[cfg(test)]
        shared.test_pause(|hooks| &mut hooks.before_send);
        if shared.shutting_down.load(Ordering::Acquire) {
            return Err(ErrorCode::WorkerUnavailable.with("the voice client is shutting down"));
        }
        if shared.link_epoch.load(Ordering::Acquire) != command_epoch {
            return Err(ErrorCode::WorkerUnavailable
                .with("the voice client disconnected before delivering the command"));
        }
        if link.send(&request) {
            #[cfg(test)]
            shared.test_pause(|hooks| &mut hooks.after_write);
            if let koru_voice::Request::Begin {
                session,
                generation,
                ..
            } = &request
            {
                let live = Live {
                    session: *session,
                    generation: *generation,
                };
                if !shared.bind_live_if_current(id, live) {
                    return Err(ErrorCode::WorkerUnavailable
                        .with("the voice worker connection ended while beginning the session"));
                }
            } else if !shared.current_link(id) {
                return Err(ErrorCode::WorkerUnavailable
                    .with("the voice worker connection ended while delivering the command"));
            }
            return Ok(());
        }
        shared.retire_link(id, None);
        last = Some(
            ErrorCode::WorkerUnavailable.with("the voice worker's connection closed mid-request"),
        );
    }
    let error = last.unwrap_or_else(|| {
        ErrorCode::WorkerUnavailable.with("the voice worker could not be reached")
    });
    shared.note(error.code);
    Err(error)
}

impl Default for VoiceClient {
    fn default() -> VoiceClient {
        VoiceClient::new()
    }
}

/// Microseconds since the epoch, or 1 on a clock that will not answer.
///
/// **A seed, not a timestamp.** Nothing reads it back as a time and nothing
/// compares it to one; it exists only so two clients of one worker start far
/// apart on a counter the worker requires to increase. That is also why a
/// clock that refuses is not a failure: the counter still only has to go up.
fn clock_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_micros() as u64)
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod test_worker;
#[cfg(test)]
mod tests;
