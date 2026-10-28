// SPDX-License-Identifier: MPL-2.0

//! Diagram layout off the event loop, and the session cache of what came
//! back.
//!
//! The editor never lays a diagram out (`koru_editor::diagram`): on every
//! reflow it hashes each `mermaid` block's text and asks for what it does not
//! have. This module answers. The spelling worker's shape (`crate::spelling`),
//! with the image cache's sharing:
//!
//! - **The handle is a process global**, installed once by [`spawn`] from
//!   `App::new`. A view holds a [`DiagramClient`] — its own reply channel and
//!   an id — and a process with no worker (a test) gets `None` from
//!   [`client`], so a block then keeps its placeholder: quietly off, the
//!   spelling rule.
//! - **A bounded pool of [`THREADS`] threads** shares one measurer, the
//!   bundled Inter face at the diagram text size, so a layout is the same on
//!   every machine. Each job runs `koru_diagram::layout` and then shapes the
//!   result into a [`PreparedDiagram`] **on the worker** — shaping is the one
//!   expensive step between a scene and pixels, and the loop thread never
//!   pays it.
//! - **Requests coalesce per `(client, node)` to the newest text.** A queued
//!   older request for the same block is replaced in place; a job already
//!   running for it has its [`Cancel`] pulled, and merman stops at its next
//!   text measurement. The cancelled job sends no reply — the editor would
//!   drop it on the hash anyway — so typing cancels stale work rather than
//!   waiting it out. There is no debounce beyond this.
//! - **The [`DiagramCache`] is process-wide**, keyed by the text's hash and
//!   shared across every view and window, like the image cache. It is
//!   consulted before any request is queued, and a hit is answered through
//!   the same reply channel, so the editor still learns of it in `maintain`
//!   and never from inside `reflow`. Failures are cached too: whether a text
//!   draws is a function of the text alone. Not keyed on theme, width or
//!   DPI — none of them changes a layout; the painter applies the palette
//!   at draw time and the column only scales the drawing.
//! - **Every completion wakes the loop** with [`Wake::Diagram`]. The wake
//!   carries nothing: `about_to_wait` runs every view's `maintain`, which
//!   drains the reply channel, lands the answer and asks for a redraw.
//!
//! What goes wrong, and what the editor sees: a text that does not draw is
//! the engine's [`Status`]; a layout that passes its deadline or exhausts
//! merman's work budget is *too large*, uncounted; an engine failure or a
//! panic — caught inside `layout`, and again around the shaping here — is a
//! `NotParsed` at line 1 carrying the internal code, so the page says *this
//! line could not be read* and Koru keeps running. A worker thread that
//! dies is replaced. Logs are news only — the worker starting, a thread
//! replaced, a job that panicked (its code, never its text), a save that
//! failed — and nothing per request.
//!
//! The *Save as…* half lives here too: the file-name and format rules a
//! dialog's answer is read by ([`default_file_name`], [`save_target`]) and
//! what a view hands the shell to perform it ([`SaveDiagram`]). The dialog,
//! the raster and the write are `App`'s, beside its other file saves.

use std::collections::{HashMap, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};

use koru_core::NodeId;
use koru_diagram::{Cancel, DiagramScene, Fail, Kind, Limits, SceneBody, Status, TextMeasure};
use koru_draw::diagram::{InterMeasure, PreparedDiagram};
use winit::event_loop::EventLoopProxy;

use crate::app::Wake;
use crate::dialogs::FileFilter;

#[cfg(test)]
mod tests;

/// How many layouts run at once. Two: one for the block being typed in and
/// one for a page opening behind it, on the cores every machine has.
pub const THREADS: usize = 2;

/// The most prepared drawings the cache keeps.
pub const CACHE_ENTRIES: usize = 128;

/// The most the cache may hold by [`estimate`], in bytes.
pub const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// The code a panic in the engine or the shaping is reported under — the
/// one `koru_diagram::layout` uses, repeated here because the shaping runs
/// outside it.
pub const PANIC_CODE: &str = "koru.diagram.panic";

/// The PNG a diagram is saved as is rasterised at this many device pixels
/// per logical pixel: crisp on a high-density screen, and the size a slide
/// or a page wants.
pub const SAVE_SCALE: f64 = 2.0;

/// The save dialog's type list, in this order: PNG first because it is the
/// default, SVG second. macOS merges the two and shows no list; the typed
/// extension decides there (see [`save_target`]).
pub const SAVE_FILTERS: &[FileFilter] = &[
    FileFilter {
        name: "PNG",
        extensions: &["png"],
    },
    FileFilter {
        name: "SVG",
        extensions: &["svg"],
    },
];

/// What the worker answers for one text: the prepared drawing, or the
/// status that says why there is none.
pub type Outcome = Result<Arc<PreparedDiagram>, Status>;

/// One answer on a client's reply channel, naming the text it is for.
pub struct DiagramReply {
    pub node: NodeId,
    pub text_hash: u64,
    pub result: Outcome,
}

/// One layout to run, for one block of one view.
struct Job {
    client: u64,
    node: NodeId,
    text_hash: u64,
    text: Arc<str>,
    reply: mpsc::Sender<DiagramReply>,
    /// Pulled when a newer text for the same block arrives while this runs.
    cancel: Cancel,
    /// Tells the thread that ran a job whether the in-flight record it is
    /// about to clear is still its own, or a successor's.
    serial: u64,
}

/// What a job is coalesced on: the view that asked, and the block.
type Key = (u64, NodeId);

struct Queue {
    pending: VecDeque<Job>,
    /// The jobs the threads are running right now, by block, with the
    /// handle that stops each.
    inflight: HashMap<Key, (u64, Cancel)>,
    next_serial: u64,
    /// Set when the worker is dropped: the threads finish what is queued
    /// and leave.
    closed: bool,
}

/// What the worker handle, its clients and its threads share.
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    measure: Arc<dyn TextMeasure>,
    cache: DiagramCache,
    counters: Counters,
    /// Ends the event loop's wait after a reply is sent.
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        // A thread that panicked while holding the lock has already been
        // replaced; the queue it left is still a queue.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue `job`, replacing an older request for the same block and
    /// stopping one that is already running for it.
    fn enqueue(&self, mut job: Job) {
        let key = (job.client, job.node);
        let mut queue = self.queue();
        job.serial = queue.next_serial;
        queue.next_serial += 1;
        if let Some((_, cancel)) = queue.inflight.remove(&key) {
            cancel.cancel();
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(slot) = queue
            .pending
            .iter_mut()
            .find(|queued| (queued.client, queued.node) == key)
        {
            *slot = job;
        } else {
            queue.pending.push_back(job);
        }
        drop(queue);
        self.ready.notify_one();
    }

    /// Forget any work for `key`: a cache hit answered the block before the
    /// worker could, so a queued or running job for it is wasted.
    fn supersede(&self, key: Key) {
        let mut queue = self.queue();
        queue
            .pending
            .retain(|queued| (queued.client, queued.node) != key);
        if let Some((_, cancel)) = queue.inflight.remove(&key) {
            cancel.cancel();
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ----------------------------------------------------------------- counters

/// What the worker has done this session, for the inspector. Plain atomics:
/// read without a lock from whichever thread asks.
#[derive(Default)]
pub struct Counters {
    /// Requests answered from the cache without a job.
    pub cache_hits: AtomicU64,
    /// Requests that became jobs.
    pub cache_misses: AtomicU64,
    /// Jobs a thread started.
    pub jobs: AtomicU64,
    /// Jobs that ran to an end of any kind, cancellation included.
    pub finished: AtomicU64,
    /// Running jobs stopped because a newer text for their block arrived.
    pub cancelled: AtomicU64,
    /// Jobs that ended in a panic — the engine's, or the shaping's.
    pub panicked: AtomicU64,
}

/// The counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountersSnapshot {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub jobs: u64,
    pub finished: u64,
    pub cancelled: u64,
    pub panicked: u64,
}

impl Counters {
    pub fn snapshot(&self) -> CountersSnapshot {
        CountersSnapshot {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            jobs: self.jobs.load(Ordering::Relaxed),
            finished: self.finished.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            panicked: self.panicked.load(Ordering::Relaxed),
        }
    }
}

// -------------------------------------------------------------------- cache

struct CacheEntry {
    result: Outcome,
    bytes: usize,
    /// The tick of the last `get` or `insert`; the smallest goes first.
    used: u64,
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<u64, CacheEntry>,
    bytes: usize,
    tick: u64,
}

/// The session's answers by text hash, least recently used evicted past
/// [`CACHE_ENTRIES`] entries or [`CACHE_BYTES`] of [`estimate`]d size.
///
/// A diagram is cheap to recompute from its text, so a bound costs nothing
/// a person notices; the image cache's never-evict rule is a known cost this
/// cache does not repeat. A lookup is one hash-map read under a lock nothing
/// holds for long.
#[derive(Default)]
pub struct DiagramCache {
    inner: Mutex<CacheInner>,
}

impl DiagramCache {
    fn inner(&self) -> MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The answer for `text_hash`, if one is held; marks it recently used.
    pub fn get(&self, text_hash: u64) -> Option<Outcome> {
        let mut inner = self.inner();
        inner.tick += 1;
        let tick = inner.tick;
        let entry = inner.entries.get_mut(&text_hash)?;
        entry.used = tick;
        Some(entry.result.clone())
    }

    /// Record the answer for `text_hash`, evicting the least recently used
    /// entries until both bounds hold.
    pub fn insert(&self, text_hash: u64, result: &Outcome) {
        let bytes = estimate(result);
        let mut inner = self.inner();
        inner.tick += 1;
        let tick = inner.tick;
        if let Some(old) = inner.entries.insert(
            text_hash,
            CacheEntry {
                result: result.clone(),
                bytes,
                used: tick,
            },
        ) {
            inner.bytes -= old.bytes;
        }
        inner.bytes += bytes;
        while inner.entries.len() > CACHE_ENTRIES
            || (inner.bytes > CACHE_BYTES && inner.entries.len() > 1)
        {
            let Some(oldest) = inner
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(hash, _)| *hash)
            else {
                break;
            };
            if let Some(gone) = inner.entries.remove(&oldest) {
                inner.bytes -= gone.bytes;
            }
        }
    }

    /// How many answers are held.
    pub fn len(&self) -> usize {
        self.inner().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The [`estimate`]d size of everything held, in bytes.
    pub fn bytes(&self) -> usize {
        self.inner().bytes
    }
}

/// A shaped line of a label: parley's layout of a short line is a few
/// hundred bytes of runs, clusters and glyphs plus the string.
const LINE_BYTES: usize = 1024;
/// A shape, a path or a marker in the scene.
const SHAPE_BYTES: usize = 256;

/// Roughly what an answer costs to hold: the scene's shapes and every label
/// line's shaping. An estimate from counts, not a measurement — a prepared
/// drawing does not expose its layouts' sizes — but one that grows with the
/// thing it bounds, which is what an eviction policy needs.
pub fn estimate(result: &Outcome) -> usize {
    match result {
        Err(status) => {
            64 + match status {
                Status::NotParsed { message, code, .. } => message.len() + code.len(),
                Status::KindNotDrawn { kind } => kind.as_ref().map_or(0, String::len),
                Status::Draws(_) | Status::TooLarge { .. } | Status::Empty => 0,
            }
        }
        Ok(prepared) => scene_weight(prepared.scene()),
    }
}

fn scene_weight(scene: &DiagramScene) -> usize {
    let label = |one: &koru_diagram::Label| one.lines.len() * LINE_BYTES + SHAPE_BYTES;
    let optional = |maybe: &Option<koru_diagram::Label>| maybe.as_ref().map_or(0, label);
    match &scene.body {
        SceneBody::Flowchart(flowchart) => {
            flowchart
                .nodes
                .iter()
                .map(|node| SHAPE_BYTES + label(&node.label))
                .sum::<usize>()
                + flowchart
                    .edges
                    .iter()
                    .map(|edge| SHAPE_BYTES + optional(&edge.label))
                    .sum::<usize>()
                + flowchart
                    .clusters
                    .iter()
                    .map(|cluster| SHAPE_BYTES + optional(&cluster.title))
                    .sum::<usize>()
        }
        SceneBody::Sequence(sequence) => {
            sequence
                .participants
                .iter()
                .map(|participant| 2 * SHAPE_BYTES + label(&participant.label))
                .sum::<usize>()
                + sequence.lifelines.len() * SHAPE_BYTES
                + sequence
                    .messages
                    .iter()
                    .map(|message| SHAPE_BYTES + label(&message.label) + optional(&message.number))
                    .sum::<usize>()
                + sequence
                    .notes
                    .iter()
                    .map(|note| SHAPE_BYTES + label(&note.label))
                    .sum::<usize>()
                + sequence.activations.len() * SHAPE_BYTES
                + sequence
                    .frames
                    .iter()
                    .map(|frame| {
                        SHAPE_BYTES
                            + LINE_BYTES
                            + label(&frame.title)
                            + frame
                                .sections
                                .iter()
                                .map(|section| SHAPE_BYTES + optional(&section.label))
                                .sum::<usize>()
                    })
                    .sum::<usize>()
                + sequence
                    .boxes
                    .iter()
                    .map(|group| SHAPE_BYTES + optional(&group.title))
                    .sum::<usize>()
                + optional(&sequence.title)
        }
    }
}

// ------------------------------------------------------------------- worker

/// The layout worker: its threads, its queue and its cache. One per
/// process, behind [`spawn`]; a test builds its own with [`start`].
///
/// [`start`]: DiagramWorker::start
pub struct DiagramWorker {
    shared: Arc<Shared>,
    clients: AtomicU64,
}

impl DiagramWorker {
    /// Start `threads` threads laying out with `measure`, calling `wake`
    /// after every reply. The threads wait on an empty queue and cost
    /// nothing until a block asks.
    pub fn start(
        measure: Arc<dyn TextMeasure>,
        threads: usize,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                pending: VecDeque::new(),
                inflight: HashMap::new(),
                next_serial: 0,
                closed: false,
            }),
            ready: Condvar::new(),
            measure,
            cache: DiagramCache::default(),
            counters: Counters::default(),
            wake: Box::new(wake),
        });
        for index in 0..threads.max(1) {
            spawn_thread(Arc::clone(&shared), index);
        }
        Self {
            shared,
            clients: AtomicU64::new(0),
        }
    }

    /// A view's own line to this worker.
    pub fn client(&self) -> DiagramClient {
        let (reply, rx) = mpsc::channel();
        DiagramClient {
            id: self.clients.fetch_add(1, Ordering::Relaxed),
            shared: Arc::clone(&self.shared),
            reply,
            rx,
        }
    }

    pub fn counters(&self) -> &Counters {
        &self.shared.counters
    }

    pub fn cache(&self) -> &DiagramCache {
        &self.shared.cache
    }
}

impl Drop for DiagramWorker {
    fn drop(&mut self) {
        self.shared.queue().closed = true;
        self.shared.ready.notify_all();
    }
}

/// One thread of the pool. A thread that panics outside a job — a job's own
/// panic is caught and answered — is replaced by a fresh one on the same
/// shared queue, and the replacement is the one log line it leaves.
fn spawn_thread(shared: Arc<Shared>, index: usize) {
    let spawned = std::thread::Builder::new()
        .name(format!("koru-diagram-{index}"))
        .spawn(move || {
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| run(&shared)));
            if outcome.is_err() {
                eprintln!("[Koru][diagram] worker thread replaced after panic");
                spawn_thread(shared, index);
            }
        });
    if let Err(error) = spawned {
        eprintln!("[Koru][diagram] worker thread could not be started: {error}");
    }
}

/// A thread's life: take the next job, run it, answer it, until the queue
/// is closed and empty.
fn run(shared: &Shared) {
    loop {
        let job = {
            let mut queue = shared.queue();
            let job = loop {
                if let Some(job) = queue.pending.pop_front() {
                    break job;
                }
                if queue.closed {
                    return;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            };
            queue
                .inflight
                .insert((job.client, job.node), (job.serial, job.cancel.clone()));
            job
        };
        shared.counters.jobs.fetch_add(1, Ordering::Relaxed);
        let outcome = perform(&shared.measure, &job.text, &job.cancel);
        {
            let mut queue = shared.queue();
            let key = (job.client, job.node);
            if queue
                .inflight
                .get(&key)
                .is_some_and(|(serial, _)| *serial == job.serial)
            {
                queue.inflight.remove(&key);
            }
        }
        shared.counters.finished.fetch_add(1, Ordering::Relaxed);
        let Some(result) = map_fail(&job.text, outcome, &shared.counters) else {
            // Cancelled: a newer text for this block is queued or answered,
            // and the editor would drop this reply on its hash anyway.
            continue;
        };
        shared.cache.insert(job.text_hash, &result);
        if job
            .reply
            .send(DiagramReply {
                node: job.node,
                text_hash: job.text_hash,
                result,
            })
            .is_ok()
        {
            (shared.wake)();
        }
    }
}

/// Lay `text` out and shape it, on this thread. The layout catches its own
/// panics; the shaping is caught here, under the same code.
fn perform(
    measure: &Arc<dyn TextMeasure>,
    text: &str,
    cancel: &Cancel,
) -> Result<Arc<PreparedDiagram>, Fail> {
    match panic::catch_unwind(AssertUnwindSafe(|| {
        koru_diagram::layout(text, measure, &Limits::default(), cancel)
            .map(|scene| Arc::new(PreparedDiagram::new(Arc::new(scene))))
    })) {
        Ok(result) => result,
        Err(payload) => Err(Fail::Internal {
            code: PANIC_CODE.to_owned(),
            message: panic_message(payload.as_ref()),
        }),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic".to_owned()
    }
}

/// The engine's answer in the editor's terms. `None` for a cancelled job:
/// no reply is owed. A timeout or an exhausted work budget is *too large*
/// in the uncounted form, with the kind the header names so the words fit;
/// an internal failure or a panic is a line that could not be read, at line
/// 1, with the code kept for the inspector. The code is logged; the text
/// and the message never are.
fn map_fail(
    text: &str,
    laid: Result<Arc<PreparedDiagram>, Fail>,
    counters: &Counters,
) -> Option<Outcome> {
    match laid {
        Ok(prepared) => Some(Ok(prepared)),
        Err(Fail::Status(status)) => Some(Err(status)),
        Err(Fail::Cancelled) => None,
        Err(Fail::TimedOut) => Some(Err(Status::TooLarge {
            kind: koru_diagram::detect(text).unwrap_or(Kind::Flowchart),
            count: 0,
        })),
        Err(Fail::Internal { code, message }) => {
            if code == PANIC_CODE {
                counters.panicked.fetch_add(1, Ordering::Relaxed);
                eprintln!("[Koru][diagram] job panicked code={code}");
            } else {
                eprintln!("[Koru][diagram] job failed code={code}");
            }
            Some(Err(Status::NotParsed {
                line: 1,
                message,
                code,
            }))
        }
    }
}

// ------------------------------------------------------------------- client

/// A view's private line to the worker. Cheap to make; dropped with the
/// view, after which replies for it go nowhere and are freed.
pub struct DiagramClient {
    id: u64,
    shared: Arc<Shared>,
    reply: mpsc::Sender<DiagramReply>,
    rx: mpsc::Receiver<DiagramReply>,
}

impl DiagramClient {
    /// Ask for `text`'s layout for `node`. A cached answer is on the reply
    /// channel before this returns; otherwise a job is queued, replacing or
    /// cancelling any older one for the same block. Never blocks beyond the
    /// queue's lock.
    pub fn request(&self, node: NodeId, text_hash: u64, text: Arc<str>) {
        if let Some(result) = self.shared.cache.get(text_hash) {
            self.shared
                .counters
                .cache_hits
                .fetch_add(1, Ordering::Relaxed);
            self.shared.supersede((self.id, node));
            let _ = self.reply.send(DiagramReply {
                node,
                text_hash,
                result,
            });
            return;
        }
        self.shared
            .counters
            .cache_misses
            .fetch_add(1, Ordering::Relaxed);
        self.shared.enqueue(Job {
            client: self.id,
            node,
            text_hash,
            text,
            reply: self.reply.clone(),
            cancel: Cancel::new(),
            serial: 0,
        });
    }

    /// Every reply that has arrived. One empty `try_recv` when idle.
    pub fn drain(&self) -> Vec<DiagramReply> {
        self.rx.try_iter().collect()
    }
}

// ------------------------------------------------------------------ globals

static WORKER: OnceLock<DiagramWorker> = OnceLock::new();

/// Start the worker and install the process-global handle. Called once from
/// `App::new`; a second call is a no-op.
pub fn spawn(proxy: &EventLoopProxy<Wake>) {
    if WORKER.get().is_some() {
        return;
    }
    let proxy = Mutex::new(proxy.clone());
    let worker = DiagramWorker::start(Arc::new(InterMeasure), THREADS, move || {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(Wake::Diagram);
        }
    });
    if WORKER.set(worker).is_ok() {
        println!("[Koru][diagram] worker spawned threads={THREADS}");
    }
}

/// A client for the installed worker, or `None` when none runs in this
/// process (a test, or a host that never called [`spawn`]).
pub fn client() -> Option<DiagramClient> {
    WORKER.get().map(DiagramWorker::client)
}

/// The installed worker's counters, for the inspector.
pub fn counters() -> Option<&'static Counters> {
    WORKER.get().map(DiagramWorker::counters)
}

/// The installed worker's cache.
pub fn cache() -> Option<&'static DiagramCache> {
    WORKER.get().map(DiagramWorker::cache)
}

// ----------------------------------------------------------------- save as…

/// *Save as…* asked on a drawn diagram: what the shell needs to write the
/// picture. The view hands over the drawing it shows and the page's title;
/// the dialog, the raster and the file are the shell's.
#[derive(Clone)]
pub struct SaveDiagram {
    /// The drawing exactly as the view paints it.
    pub prepared: Arc<PreparedDiagram>,
    /// The page's title, for the default file name. Empty when it has none.
    pub title: String,
}

// A request is compared by *which* drawing it names, and printed by its
// size: a title is a person's words and does not belong in a log.
impl PartialEq for SaveDiagram {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.prepared, &other.prepared) && self.title == other.title
    }
}

impl Eq for SaveDiagram {}

impl std::fmt::Debug for SaveDiagram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveDiagram")
            .field("natural_size", &self.prepared.natural_size())
            .field("title_chars", &self.title.chars().count())
            .finish()
    }
}

/// *Copy image* asked on a drawn diagram: the drawing the shell rasterises
/// for the clipboard, at [`SAVE_SCALE`] like a saved PNG, so what is pasted
/// elsewhere is as crisp as what is saved. Compared and printed like
/// [`SaveDiagram`], for the same reasons.
#[derive(Clone)]
pub struct CopyDiagram {
    pub prepared: Arc<PreparedDiagram>,
}

impl PartialEq for CopyDiagram {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.prepared, &other.prepared)
    }
}

impl Eq for CopyDiagram {}

impl std::fmt::Debug for CopyDiagram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyDiagram")
            .field("natural_size", &self.prepared.natural_size())
            .finish()
    }
}

/// The two files a diagram is saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFormat {
    Png,
    Svg,
}

/// The name the save dialog opens with: the page's title made safe for a
/// file name, as a PNG; `Diagram.png` when the page has no title.
pub fn default_file_name(title: &str) -> String {
    let title = title.trim();
    if !title.chars().any(char::is_alphanumeric) {
        return "Diagram.png".to_owned();
    }
    format!("{}.png", koru_export::sanitize_stem(title))
}

/// What the dialog's answer means: **the typed extension decides**. `.svg`,
/// in any case, is an SVG; anything else is a PNG, and a name with no
/// extension gets `.png`. The rule is the same on every platform because
/// macOS merges the filters and shows no type list, so the extension is the
/// only thing a person can say there; Windows shows the list, and picking
/// a type sets the extension, which is this same rule.
pub fn save_target(chosen: PathBuf) -> (PathBuf, SaveFormat) {
    match chosen.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("svg") => (chosen, SaveFormat::Svg),
        Some(_) => (chosen, SaveFormat::Png),
        None => (chosen.with_extension("png"), SaveFormat::Png),
    }
}

/// What the save worker writes: the SVG's text, or the PNG's pixels still to
/// be encoded — encoding a 2× raster is tens of milliseconds the loop thread
/// does not spend.
pub enum SavePayload {
    Svg(String),
    Rgba(koru_window::offscreen::Rgba),
}

impl SavePayload {
    /// The file's bytes.
    pub fn encode(self) -> Result<Vec<u8>, String> {
        match self {
            Self::Svg(text) => Ok(text.into_bytes()),
            Self::Rgba(rgba) => koru_clipboard::png(rgba.width, rgba.height, &rgba.bytes)
                .map_err(|error| error.to_string()),
        }
    }
}

/// The name a notice calls the saved file.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
