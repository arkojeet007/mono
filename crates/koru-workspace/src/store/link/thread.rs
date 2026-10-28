// SPDX-License-Identifier: MPL-2.0

//! The socket, on a thread of its own — and the one that finds it again.
//!
//! What the synchronous link did on whoever's thread asked — which was the event
//! loop's — this does on one dedicated to it. The event loop's share of the work
//! becomes a channel `try_recv`.
//!
//! ## Why a thread per connection, with a read timeout
//!
//! `tungstenite`'s `read` blocks and **cannot be interrupted by a writer**, so a
//! thread that owns the socket has to come up for air to see whether anything
//! has been queued for sending. That wait is the only thing [`SOCKET_READ`]
//! bounds: how long an update sits before it is written, and how long a shutdown
//! takes to be noticed. It does not delay anything *inbound* — an arriving frame
//! ends the read immediately.
//!
//! The alternatives were weighed and are worth not re-deriving:
//!
//! - **`try_clone` the stream for a write-only half** costs nothing in latency
//!   and needs a frame-atomic sink for the reader's automatic Pong, or a bet
//!   that the store never pings. It does not today. That is a remote-controlled
//!   invariant, and this codebase has a rule about those.
//! - **`mio`, or non-blocking with readiness**, buys the same for a new
//!   workspace dependency, hand-rolled `WouldBlock` and partial-flush handling
//!   around `tungstenite`, and a Windows waker that has to itself be a socket.
//!
//! Both remain reachable behind [`Link`] without a call site moving, which is
//! the point of the trait. Take one when somebody measures 5 ms mattering.
//!
//! ## What is on which thread
//!
//! The thread owns the `Connection` and nothing else. It never sees a
//! `VersionVector`, never sees a document, and never decides what is owed —
//! [`StoreSession`](crate::store::StoreSession) does all of that, on the thread
//! that holds the document, which is what makes "export and then record what was
//! exported" atomic without a lock.
//!
//! Batch ids are the one thing shared, and they are allocated **off** the socket:
//! `publish` has to answer with one synchronously, and they stay monotonic across
//! a reconnect so a late acknowledgement from a dead socket cannot be mistaken
//! for a new one.
//!
//! ## Reconnecting
//!
//! **A store that is down is expected to come back**, so this retries with
//! backoff and does not give up. It re-runs *discovery* each time rather than
//! reusing the address: a restarted store is a new process on a new port, and a
//! client that remembered the old one would wait forever on something that will
//! never answer.
//!
//! **A new socket is a new generation, and the old generation's queued bytes are
//! discarded.** They were deltas computed against a position the session is about
//! to rewind past, so writing them after the rejoin would hand the store updates
//! whose ancestors it may not have — which Loro accepts and silently applies
//! nothing. The thread therefore drops everything queued until the session sends
//! a `Join`, which is precisely what `publish` does when it consumes its rejoin
//! flag. That is the ordering invariant *Join precedes Update, per generation*,
//! enforced rather than hoped for.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_api_client::{Client, Connection, Discovery};
use koru_store_core::errors::{Code, Error, Result};
use loro_protocol::BatchId;

use super::{Inbound, Link, LinkState, Waker};
use crate::store::ClientFactory;

/// How long the socket thread waits for a frame before looking at its outbound
/// queue.
///
/// **Not the old `POLL_READ` moved to another thread.** That one was a blocking
/// read on the event loop, once per session per frame; this is a sleeping
/// thread. What it bounds is how long a queued update waits before it is
/// written — a third of a frame, so an edit is on the wire before the frame that
/// produced it is on the glass — and how long a teardown takes when there is no
/// abort handle.
const SOCKET_READ: Duration = Duration::from_millis(5);

/// How many inbound events are held before the thread stops reading.
///
/// **Stopping is the policy, not dropping.** An inbound frame cannot be
/// recovered without a rejoin, so refusing to read one is free while discarding
/// one is not: the receive window closes and the store's own send blocks, which
/// is TCP doing the backpressure for us.
const INBOX: usize = 256;

/// The first wait after a connection is lost, and the ceiling it doubles to.
///
/// Short at first because the overwhelmingly common case is a store being
/// restarted, which is back within a second or two; capped because a store that
/// is properly gone should not be hammered.
const RETRY_FIRST: Duration = Duration::from_millis(100);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// How long a teardown waits for the thread to write what is queued before
/// taking the socket away from it.
///
/// **Closing must not discard an edit.** An update reaches the thread through a
/// channel, so at the moment a window closes there may be bytes queued and not
/// yet written — and shutting the socket down first would drop exactly the last
/// thing somebody typed. The thread flushes and exits within one
/// [`SOCKET_READ`], so this is a backstop rather than a wait anybody pays.
const FLUSH_ON_CLOSE: Duration = Duration::from_millis(250);
/// How long the thread waits for the store's Close after sending its own. Well
/// inside [`FLUSH_ON_CLOSE`], so a slow store costs a reset, not a forced
/// shutdown of the stream.
const CLOSE_HANDSHAKE: Duration = Duration::from_millis(120);

enum Command {
    Join(String, loro::VersionVector),
    Update(String, Vec<u8>, BatchId),
    Leave(String),
}

/// Where to look for the store, every time it has to be found again.
#[derive(Clone)]
struct Where {
    segment: String,
    root: Option<PathBuf>,
    factory: Option<ClientFactory>,
}

impl Where {
    fn discover(&self) -> Result<Discovery> {
        if let Some(factory) = &self.factory {
            return Ok(factory()?.store().discovery());
        }
        let client = match &self.root {
            Some(root) => Client::open_at(&self.segment, root),
            None => Client::open(&self.segment),
        }?;
        Ok(client.store().discovery())
    }
}

pub struct ThreadLink {
    outbound: Sender<Command>,
    inbound: Mutex<Receiver<Inbound>>,
    /// How full the inbox is, as the thread sees it. A `Receiver` cannot be
    /// asked its length from the sending side.
    queued: Arc<AtomicU64>,
    next_batch: AtomicU64,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<LinkState>>,
    /// How to tell the event loop something arrived. `OnceLock` because it is
    /// installed once, just after construction, and read on the socket thread
    /// on every inbound frame — a lock on that path would be a lock the UI
    /// thread contends with for nothing.
    waker: Arc<std::sync::OnceLock<Waker>>,
    /// The thread says here that it has written everything queued and is about
    /// to end. `JoinHandle::join` cannot be given a deadline, and a teardown
    /// needs one.
    ///
    /// `Mutex` only to make it `Sync`: a `Receiver` is not, and only `Drop`
    /// ever reads this — on one thread, once.
    finished: Mutex<Receiver<()>>,
    /// Breaks a blocking read, for a thread that did not answer in time.
    abort: Option<std::net::TcpStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ThreadLink {
    /// Connect, and keep connecting.
    ///
    /// Takes *where to look* rather than a `Discovery`, because a reconnect has
    /// to look again: a restarted store is a new process on a new port.
    pub fn open(segment: &str, root: Option<&std::path::Path>) -> Result<Self> {
        let finder = Where {
            segment: segment.to_owned(),
            root: root.map(|r| r.to_path_buf()),
            factory: None,
        };
        Self::open_with_finder(finder)
    }

    pub fn open_with_client_factory(factory: ClientFactory) -> Result<Self> {
        Self::open_with_finder(Where {
            segment: String::new(),
            root: None,
            factory: Some(factory),
        })
    }

    fn open_with_finder(finder: Where) -> Result<Self> {
        // The first connection is the caller's to fail on: there is nothing to
        // open a workspace *from* if the store cannot be reached at all, and a
        // window that opened anyway would be the empty-window failure this
        // codebase has a rule about.
        let mut connection = Connection::open(&finder.discover()?)?;
        connection.set_read_timeout(Some(SOCKET_READ))?;
        let abort = connection.abort_handle();

        let (outbound, commands) = channel::<Command>();
        let (events, inbound) = channel::<Inbound>();
        let (done, finished) = channel::<()>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(LinkState::Live));
        let waker: Arc<std::sync::OnceLock<Waker>> = Arc::new(std::sync::OnceLock::new());

        let thread = std::thread::Builder::new()
            .name("koru-store-sync".to_owned())
            .spawn({
                let shutdown = shutdown.clone();
                let queued = queued.clone();
                let state = state.clone();
                let waker = waker.clone();
                move || {
                    run(Worker {
                        connection,
                        finder,
                        commands,
                        events,
                        shutdown,
                        queued,
                        state,
                        waker,
                        connected_at: Instant::now(),
                        last_reported_failure: None,
                    });
                    // Whether it flushed or gave up, the thread is done and a
                    // teardown should stop waiting for it.
                    let _ = done.send(());
                }
            })
            .map_err(|e| Error::new(Code::StorageFailed, format!("could not start sync: {e}")))?;

        Ok(Self {
            outbound,
            inbound: Mutex::new(inbound),
            queued,
            next_batch: AtomicU64::new(1),
            shutdown,
            state,
            waker,
            finished: Mutex::new(finished),
            abort,
            thread: Some(thread),
        })
    }

    /// The thread has gone. Every method answers the same way, because the
    /// distinction the caller cares about is *this link is finished*, not which
    /// call noticed.
    fn gone() -> Error {
        Error::new(Code::StorageFailed, "the store connection ended")
    }

    fn queue(&self, command: Command) -> Result<()> {
        self.outbound.send(command).map_err(|_| Self::gone())
    }
}

impl Link for ThreadLink {
    fn join(&self, room: &str, version: &loro::VersionVector) -> Result<()> {
        self.queue(Command::Join(room.to_owned(), version.clone()))
    }

    fn send_update(&self, room: &str, bytes: Vec<u8>) -> Result<BatchId> {
        // Allocated here, so `publish` can record what it is owed without
        // waiting for the socket, and monotonic across a reconnect so a late
        // acknowledgement cannot be mistaken for a new one.
        let n = self.next_batch.fetch_add(1, Ordering::Relaxed);
        let batch = BatchId(n.to_le_bytes());
        self.queue(Command::Update(room.to_owned(), bytes, batch))?;
        Ok(batch)
    }

    fn leave(&self, room: &str) -> Result<()> {
        self.queue(Command::Leave(room.to_owned()))
    }

    fn take(&self, max: usize) -> Result<Vec<Inbound>> {
        // **Before the drain.** A frame arriving while this is running has to
        // wake the loop again; rearming afterwards would swallow it, and the
        // window would sit on somebody else's edit until something unrelated
        // woke it.
        if let Some(waker) = self.waker.get() {
            waker.rearm();
        }
        let inbound = self
            .inbound
            .lock()
            .map_err(|_| Error::new(Code::StorageFailed, "the store inbox was poisoned"))?;
        let mut out = Vec::new();
        for _ in 0..max {
            match inbound.try_recv() {
                Ok(event) => out.push(event),
                // **Not an error, and not the same as an error.** An empty inbox
                // is the normal answer; a disconnected one means the thread has
                // stopped, and what it already delivered is still worth taking.
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let taken = out.len() as u64;
        let before = self.queued.fetch_sub(taken, Ordering::AcqRel);
        let remaining = before.saturating_sub(taken);
        drop(inbound);

        // `take` is deliberately bounded. If it stopped at `max`, the inbox
        // still contains work but no new socket read is required to trigger a
        // wake. Rearming above without announcing that backlog leaves the
        // remainder asleep until an unrelated mouse/key event happens to run
        // the loop again.
        if remaining > 0
            && let Some(waker) = self.waker.get()
        {
            waker.wake();
        }
        Ok(out)
    }

    fn set_waker(&self, waker: Waker) {
        // The socket thread starts before the shell can install its event-loop
        // proxy. A Ready, Joined or catch-up frame may therefore already be in
        // the inbox. Installing the waker must announce that existing work;
        // waiting for the *next* frame makes the first one visible only after
        // an unrelated window event (commonly a click).
        if self.waker.set(waker.clone()).is_ok() && self.queued.load(Ordering::Acquire) > 0 {
            waker.wake();
        }
    }

    fn state(&self) -> LinkState {
        self.state
            .lock()
            .map(|s| s.clone())
            .unwrap_or(LinkState::Stopped {
                why: "the store connection was poisoned".to_owned(),
            })
    }
}

impl Drop for ThreadLink {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // **Ask, wait, then insist.** The thread answers by writing everything
        // still queued and reporting that it is done; only a thread that does
        // not answer in time has its socket taken away. Doing that
        // unconditionally is faster and loses the last edit.
        let flushed = self
            .finished
            .lock()
            .is_ok_and(|f| f.recv_timeout(FLUSH_ON_CLOSE).is_ok());
        if !flushed && let Some(stream) = &self.abort {
            // A reset on the gateway's side if the thread had not answered:
            // said, so it can be told from the thread simply ending.
            tracing::warn!(
                target: "koru_workspace::sync",
                waited_ms = FLUSH_ON_CLOSE.as_millis() as u64,
                "sync link did not finish in time; socket shut down by force"
            );
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Everything the socket thread owns.
struct Worker {
    connection: Connection,
    finder: Where,
    commands: Receiver<Command>,
    events: Sender<Inbound>,
    shutdown: Arc<AtomicBool>,
    queued: Arc<AtomicU64>,
    state: Arc<Mutex<LinkState>>,
    waker: Arc<std::sync::OnceLock<Waker>>,
    /// Console diagnostics are transitions, not retry-loop telemetry. Keep the
    /// last distinct cause until a connection has stayed healthy long enough
    /// to count as recovered, or a 100 ms reconnect loop would flood stdout in
    /// exactly the way the search schema failure flooded the Store log.
    connected_at: Instant,
    last_reported_failure: Option<String>,
}

impl Worker {
    fn set_state(&self, next: LinkState) {
        if let Ok(mut state) = self.state.lock() {
            *state = next;
        }
    }

    fn report(&self, event: Inbound) -> bool {
        self.queued.fetch_add(1, Ordering::Release);
        if self.events.send(event).is_err() {
            return false;
        }
        // **After the send, never before.** The loop may wake and drain on
        // another core the instant this is called; waking it first would let it
        // look at an inbox this event has not reached yet, find nothing, rearm,
        // and go back to sleep holding it.
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
        true
    }

    fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn report_failure(&mut self, why: &str) {
        if self.last_reported_failure.as_deref() != Some(why) {
            super::diagnostic(
                super::DiagnosticLevel::Warn,
                &format!("[Koru][store-sync] connection lost; reconnecting: {why}"),
            );
            self.last_reported_failure = Some(why.to_owned());
        }
    }

    fn report_stable(&mut self) {
        if self.last_reported_failure.is_some()
            && self.connected_at.elapsed() >= Duration::from_secs(1)
        {
            super::diagnostic(
                super::DiagnosticLevel::Info,
                "[Koru][store-sync] connection restored",
            );
            self.last_reported_failure = None;
        }
    }
}

/// The socket thread.
///
/// **Outbound before inbound, every turn.** A local edit must be on its way
/// before this replica starts applying somebody else's, which is the ordering
/// `tick_store` states and the reason `publish` runs before `drain` up there.
///
/// **And outbound one last time on the way out.** A shutdown seen while blocked
/// in a read would otherwise end the thread with bytes still in the channel, and
/// those bytes are the last thing somebody typed before closing the window.
fn run(mut w: Worker) {
    let started = Instant::now();
    let why = run_loop(&mut w);
    // The connection ends with a Close frame and the store's answer read, not
    // dropped: a socket dropped with the store's replies unread is reset on
    // Windows, which the gateway logged as *forcibly closed* once per session —
    // a read joined a room and the socket was gone a millisecond later. The
    // budget sits inside `FLUSH_ON_CLOSE`, which is what a closing window waits.
    let polite = w.connection.close(CLOSE_HANDSHAKE);
    tracing::info!(
        target: "koru_workspace::sync",
        why,
        lived_ms = started.elapsed().as_millis() as u64,
        closed_politely = polite,
        "sync link thread ended"
    );
}

/// The loop of [`run`], returning why it ended.
fn run_loop(w: &mut Worker) -> &'static str {
    // Bytes queued before the session was told the connection died are deltas
    // against a position it is about to rewind past. Dropped until it says
    // `Join`, which is how it announces that it has rewound.
    let mut stale = false;

    loop {
        let stopping = w.stopping();

        match write_queued(&mut w.connection, &w.commands, &mut stale) {
            Wrote::Everything => {}
            Wrote::LinkGone => return "the session dropped its link",
            Wrote::SocketDied(why) => {
                if !reconnect(w, why) {
                    return "reconnect gave up after a write failed";
                }
                stale = true;
                continue;
            }
        }
        if stopping {
            return "shutdown requested, queue flushed";
        }

        // Full inbox: keep writing, stop reading. See `INBOX`.
        if w.queued.load(Ordering::Acquire) >= INBOX as u64 {
            std::thread::sleep(SOCKET_READ);
            continue;
        }

        match w.connection.next_event() {
            Ok(None) => w.report_stable(),
            Ok(Some(event)) => {
                w.report_stable();
                if !w.report(Inbound::Event(event)) {
                    return "nobody is listening to events";
                }
            }
            Err(e) => {
                if !reconnect(w, e.to_string()) {
                    return "reconnect gave up after a read failed";
                }
                stale = true;
            }
        }
    }
}

/// Find the store again, however long it takes.
///
/// `false` when the thread should end instead — the window is closing, or
/// nobody is listening to what it reports any more.
fn reconnect(w: &mut Worker, why: String) -> bool {
    w.report_failure(&why);
    if !w.report(Inbound::Lost { why: why.clone() }) {
        return false;
    }
    w.set_state(LinkState::Reconnecting {
        since: Instant::now(),
        why,
    });

    let mut wait = RETRY_FIRST;
    loop {
        // A window closing while the store is down must not wait for it.
        if w.stopping() {
            return false;
        }
        // Slept in slices so a shutdown is noticed within one, rather than after
        // however long the backoff had grown to.
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if w.stopping() {
                return false;
            }
            std::thread::sleep(SOCKET_READ);
        }

        // **Discovery every time, not the address we had.** A restarted store is
        // a new process on a new port; the endpoint is how it is found, and a
        // remembered port is a promise nobody made.
        if let Ok(found) = w.finder.discover()
            && let Ok(mut fresh) = Connection::open(&found)
            && fresh.set_read_timeout(Some(SOCKET_READ)).is_ok()
        {
            w.connection = fresh;
            w.connected_at = Instant::now();
            w.set_state(LinkState::Live);
            // The rooms are not rejoined here: a session knows which room it
            // wants and at which version, and this thread knows neither.
            return w.report(Inbound::Ready);
        }
        wait = (wait * 2).min(RETRY_MAX);
    }
}

enum Wrote {
    Everything,
    /// The socket failed. What was being written is gone with it.
    SocketDied(String),
    /// The link was dropped, and so is anything it might still have sent.
    LinkGone,
}

/// Write everything queued, in order.
///
/// **In order, and never selectively.** Update *n+1* is a delta from the version
/// after *n*: drop or reorder one and Loro parks the next as pending, which
/// `import` reports as `Ok` while applying nothing.
///
/// The one thing that *is* dropped is a whole generation: while `stale`, updates
/// computed before the session learned the connection died are discarded, and a
/// `Join` — which only `publish` sends, and only after rewinding — ends that.
fn write_queued(
    connection: &mut Connection,
    commands: &Receiver<Command>,
    stale: &mut bool,
) -> Wrote {
    loop {
        match commands.try_recv() {
            Ok(Command::Join(room, version)) => {
                *stale = false;
                if let Err(e) = connection.join(&room, &version) {
                    return Wrote::SocketDied(e.to_string());
                }
            }
            Ok(Command::Update(room, bytes, batch)) => {
                if *stale {
                    continue;
                }
                if let Err(e) = connection.send_update_as(&room, bytes, batch) {
                    return Wrote::SocketDied(e.to_string());
                }
            }
            Ok(Command::Leave(room)) => {
                if *stale {
                    continue;
                }
                if let Err(e) = connection.leave(&room) {
                    return Wrote::SocketDied(e.to_string());
                }
            }
            Err(TryRecvError::Empty) => return Wrote::Everything,
            Err(TryRecvError::Disconnected) => return Wrote::LinkGone,
        }
    }
}
