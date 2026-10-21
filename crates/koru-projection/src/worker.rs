// SPDX-License-Identifier: MPL-2.0

//! Keeping the read model level with the store.
//!
//! ## It is a tick, not a thread
//!
//! [`Projector::tick`] does one unit of work and says whether it did any. The
//! daemon owns the thread that calls it. That is not squeamishness about
//! threads — it is what makes every rule below testable with a fake clock and a
//! fake store, instead of with a sleep and a hope.
//!
//! ## One mechanism, not two
//!
//! Catching up after a restart, rebuilding a discarded database, and noticing a
//! document that was dropped are the same operation: **walk what the store
//! holds, compare revisions, act on what differs.** A sweep is that walk, it is
//! batched, and its cursor is in the projection's own state table so it survives
//! a restart mid-way.
//!
//! So there is no separate "rebuild" path to keep correct. On a healthy
//! workspace a sweep is one revision comparison per document and no document
//! opens at all; on an empty projection it is a full rebuild; and nothing had to
//! decide which.
//!
//! **It is per workspace, and it waits to be asked.** The store opens a
//! workspace lazily and promises that start-up costs the same whether it holds
//! one or a thousand — so a sweep is queued when a workspace shows its first
//! commit, by which point the store has it open anyway. Derived state does not
//! get to spend somebody else's budget.
//!
//! ## What it will not do
//!
//! Hold the store's lock while it walks. [`Source::fork`] is expected to take
//! whatever lock it needs, fork — a structural share, not a copy — and release.
//! A walk behind that lock would block every other workspace's commit.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use koru_store_core::Committed;
use koru_store_core::ids::{DocumentId, WorkspaceId};

use crate::consumer::Consumer;
use crate::model::{ListedDocument, apply_inherited_trash};
use crate::store::{Projections, Staleness};

/// Why the authoritative Store could not list one workspace's durable
/// positions. A missing workspace is final and retires stale derived state;
/// every other failure is transient and must remain retryable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PositionsError {
    WorkspaceNotFound,
    Other(String),
}

impl std::fmt::Display for PositionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceNotFound => f.write_str("workspace_not_found"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// Where a projector reads authoritative state from.
///
/// **A trait so this crate never learns what a lock is.** The daemon implements
/// it over its `Mutex<Store>`; a test implements it over a `HashMap`.
pub trait Source {
    /// A read-only copy of one document.
    ///
    /// The implementation takes whatever lock it needs, forks and releases —
    /// the fork is cheap, the walk that follows is not, and only one of them
    /// belongs inside the lock.
    fn fork(&self, workspace: WorkspaceId, document: DocumentId) -> Result<loro::LoroDoc, String>;

    /// The store's current revision for a document — `VersionVector::encode()`.
    fn revision(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, String>;

    /// Every document in one workspace, with the revision it is durable at.
    ///
    /// **Must not open the documents.** Answering "has this changed" by loading
    /// each document's history would make a sweep cost the whole workspace even
    /// when the answer is *no* for all of it — and nothing evicts, so the store
    /// would stay opened. The durable position is a row in the journal.
    fn positions(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<(DocumentId, Vec<u8>)>, PositionsError>;

    /// When the **store** last durably wrote one document, in epoch millis.
    ///
    /// The one stamp above this layer that outlives the projection: it is a row
    /// in the workspace's own database, so dropping `projection.db` and building
    /// it again recovers it rather than restamping everything at the moment of
    /// the rebuild. `Projections::replace` prefers it and falls back to its
    /// content digest, which is what `None` selects — an implementation that
    /// cannot answer costs the sort accuracy it always had, never a failure.
    ///
    /// Its semantics are the store's, and are written down on
    /// `koru_store_core::Store::document_updated_at`: this document's own
    /// journal, never the manifest's, so a rename or a bin does not move it.
    fn modified_at(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Option<i64>, String>;

    /// The workspace's document listing — titles, parents, trash flags.
    ///
    /// **Must not open the documents either.** The store answers this from its
    /// own derived index, which is what makes mirroring the listing cheap
    /// enough to do on every manifest commit.
    fn entries(&self, workspace: WorkspaceId) -> Result<Vec<ListedDocument>, String>;

    /// Every workspace the store knows, and what it is called.
    fn workspaces(&self) -> Result<Vec<(WorkspaceId, String)>, String>;

    /// Commits since this was last called.
    fn take_committed(&self) -> Vec<Committed>;

    /// The commits of this tick the projection **follows** — every manifest
    /// commit and every commit to a [`crate::model::NOT_PROJECTED`] kind
    /// already removed — for whatever per-document bookkeeping the source
    /// keeps beside the read model. Called once per tick, with an empty
    /// slice when nothing was committed, so a source with its own clock
    /// still gets to look at it.
    ///
    /// **This is where automatic history is enrolled** (the daemon's
    /// implementation), and the reason it is a hook on the projection's
    /// decision rather than a second look at the same commits: a run's
    /// history is its own timeline, and a capture of it every ten minutes
    /// would be tens of megabytes of `document_versions` nothing opens. One
    /// kind set, decided once, in the layer that already holds the listing.
    fn record_activity(&self, _followed: &[Committed], _now_millis: i64) {}

    /// Durable synchronized reminder-root changes. Defaulted for projection
    /// sources that do not host profile state.
    fn take_reminder_committed(&self) -> Vec<koru_store_core::ReminderCommitted> {
        Vec::new()
    }

    /// Durable personal-dictionary changes. Defaulted for sources without
    /// Profile-private state.
    fn take_dictionary_committed(&self) -> Vec<koru_store_core::DictionaryCommitted> {
        Vec::new()
    }

    /// Durable Profile identity changes that arrived from a peer or the Cloud.
    /// Defaulted for sources that do not host Profile state.
    fn take_identity_committed(&self) -> Vec<koru_store_core::IdentityCommitted> {
        Vec::new()
    }
}

/// Asking for a workspace to be compared against the store, and finding out
/// when one has been.
///
/// **A handle, because a consumer cannot reach the projector.** Consumers are
/// owned by the [`Projector`] as `Box<dyn Consumer>`, so
/// [`Projector::request_sweep`] — which takes `&mut self` — is unreachable from
/// inside one. This is the way back in, and it is the only one.
///
/// It matters because the projector follows the store's laziness: a **restarted
/// store does not re-project a workspace until something uses it**. Nothing is
/// stale that anybody has written to; what is stale is a workspace nobody has
/// touched since the store came up — which is exactly the workspace a search
/// query is about to ask about. Getting that wrong is quiet: results from
/// before the restart, and no error anywhere.
///
/// In memory rather than on disk, deliberately, for the same reason `swept` is:
/// a restart *should* sweep again, because a restart is when this layer may
/// have missed something.
#[derive(Clone, Default)]
pub struct Sweeps(Arc<SweepSignal>);

#[derive(Default)]
struct SweepSignal {
    state: Mutex<SweepState>,
    requested: Condvar,
    /// Set only by [`Sweeps::answering_for_test`].
    answering: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct SweepState {
    requested: HashMap<WorkspaceId, u64>,
    issued: HashMap<WorkspaceId, u64>,
    finished: HashMap<WorkspaceId, u64>,
}

impl Sweeps {
    /// Ask for one and return the exact successful completion ticket required
    /// by this request. Issuing and enqueueing are one locked operation, so a
    /// sweep finishing concurrently cannot satisfy a request it preceded.
    pub fn request(&self, workspace: WorkspaceId) -> u64 {
        let mut state = self.lock();
        let ticket = next_ticket(&mut state, workspace);
        if self.0.answering.load(std::sync::atomic::Ordering::Acquire) {
            state.finished.insert(workspace, ticket);
            return ticket;
        }
        state.requested.insert(workspace, ticket);
        drop(state);
        // A query never waits for this generation — it reports the workspace
        // as updating and the caller retries — but the daemon's otherwise-idle
        // projector sleeps for 250 ms, so wake it now rather than have the
        // retry find it still asleep.
        self.0.requested.notify_one();
        ticket
    }

    /// Sleep until a consumer asks for a sweep, or until ordinary projection
    /// polling is due. The predicate closes the notify-before-wait race: a
    /// request already queued returns immediately rather than losing its wake.
    pub fn wait_for_request(&self, timeout: std::time::Duration) -> bool {
        let state = self.lock();
        if !state.requested.is_empty() {
            return true;
        }
        let (state, _) = self
            .0
            .requested
            .wait_timeout_while(state, timeout, |state| state.requested.is_empty())
            .unwrap_or_else(|error| error.into_inner());
        !state.requested.is_empty()
    }

    /// Sweeps of this workspace that have **completed** since start-up.
    ///
    /// Zero means nothing has yet compared it against the store, which is not
    /// the same as *it is up to date* and must not be read as it.
    pub fn finished(&self, workspace: WorkspaceId) -> u64 {
        self.lock().finished.get(&workspace).copied().unwrap_or(0)
    }

    fn take_requests(&self) -> Vec<(WorkspaceId, u64)> {
        std::mem::take(&mut self.lock().requested)
            .into_iter()
            .collect()
    }

    fn issue(&self, workspace: WorkspaceId) -> u64 {
        next_ticket(&mut self.lock(), workspace)
    }

    /// Record a finished sweep without a projector, so a consumer's own tests
    /// can reach the state a projector would have put it in.
    #[doc(hidden)]
    pub fn record_for_test(&self, workspace: WorkspaceId) {
        let ticket = self.issue(workspace);
        self.record(workspace, ticket);
    }

    /// Sweeps whose every request is finished as it is issued: the state of a
    /// projector that has caught up, for a consumer's tests to read through.
    ///
    /// A query requests its sweep and reads the completion in one call, with
    /// no wait between (a query never waits for a sweep). A test thread that
    /// completes the request with [`Self::take_request_and_record_for_test`]
    /// therefore has to be scheduled inside that gap, and on a loaded runner
    /// it is not; this makes "the sweep finished" a fact instead of a race.
    #[doc(hidden)]
    pub fn answering_for_test() -> Self {
        let sweeps = Self::default();
        sweeps
            .0
            .answering
            .store(true, std::sync::atomic::Ordering::Release);
        sweeps
    }

    /// Atomically observe the exact queued request a test is waiting for and
    /// complete it. This avoids timing sleeps around the generation contract.
    #[doc(hidden)]
    pub fn take_request_and_record_for_test(&self, workspace: WorkspaceId) -> bool {
        let mut state = self.lock();
        let Some(ticket) = state.requested.remove(&workspace) else {
            return false;
        };
        state.finished.insert(workspace, ticket);
        true
    }

    fn record(&self, workspace: WorkspaceId, ticket: u64) {
        let mut state = self.lock();
        let finished = state.finished.entry(workspace).or_insert(0);
        *finished = (*finished).max(ticket);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SweepState> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn next_ticket(state: &mut SweepState, workspace: WorkspaceId) -> u64 {
    let next = state.issued.get(&workspace).copied().unwrap_or(0) + 1;
    state.issued.insert(workspace, next);
    next
}

/// How long a document must be quiet before it is projected.
///
/// Twenty edits in a second must cost one projection, and this is what makes
/// that true. Shorter than `GLOBAL_SEARCH.md`'s five seconds on purpose: that
/// number was chosen when the *app* was the indexer and had a window to protect,
/// and the store is the writer now.
///
/// **The value is the wire's**, not this crate's: a client that has just
/// written re-asks the read model no sooner than this, so the two numbers must
/// be one number, and the contract is published where the client can read it.
pub const QUIET_MILLIS: i64 = koru_store_wire::tasks::PROJECTION_QUIET_MILLIS;

/// The longest a document may be held back while somebody keeps typing. The
/// wire's, for [`QUIET_MILLIS`]'s reason: a client's retry ceiling is derived
/// from it.
pub const CEILING_MILLIS: i64 = koru_store_wire::tasks::PROJECTION_HOLD_CEILING_MILLIS;

/// How many documents one sweep step looks at.
///
/// Small on purpose: a step holds nothing across its boundary, so a large batch
/// buys throughput on a rebuild nobody is watching at the cost of latency on the
/// edits somebody is.
pub const SWEEP_BATCH: usize = 32;

/// Where a sweep has got to, so it survives a restart mid-way.
const K_SWEEP_CURSOR: &str = "sweep_cursor";

/// How many times one document may fail before it is left alone.
///
/// The backoff doubles to a cap of `QUIET_MILLIS * 16`, so six attempts spend
/// about half a minute before giving up — long enough that a lock or a busy
/// disk resolves itself, short enough that a document which can never project
/// stops costing a fork and a walk every eight seconds.
const MAX_RETRY_ATTEMPTS: u8 = 6;

/// What the projection layer is doing, for `GET /v1/debug/projection`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectorStats {
    /// Documents waiting out their debounce.
    pub pending: usize,
    pub projected: u64,
    pub removed: u64,
    /// Listings that actually moved. A manifest commit that changed nothing a
    /// consumer joins against is not one.
    pub listings: u64,
    pub failed: u64,
    /// Sweeps finished since start-up.
    pub sweeps: u64,
    /// Whether one is running now.
    pub sweeping: bool,
    /// Documents left in the running sweep, if one is running.
    pub sweep_remaining: usize,
    pub last_error: Option<String>,
    /// Milliseconds the last projection took, fork, walk and write together.
    pub last_millis: u64,
    /// Every projection since start-up, added up. With [`Self::projected`] this
    /// gives a mean; on its own it is what the layer has cost the machine.
    pub total_millis: u64,
    /// The slowest single projection. **An average hides exactly the document
    /// worth looking at** — four hundred at two milliseconds and one at nine
    /// hundred is a healthy mean and an unhealthy store.
    pub max_millis: u64,
    /// The last projection, split by stage. `last_millis` covered all three at
    /// once, which could say a projection was slow but never which part of it.
    ///
    /// Forking a `LoroDoc` out of the store, under the store's lock.
    pub fork_millis: u64,
    /// Walking the node model into rows. Ours, and the part that grows with a
    /// document.
    pub walk_millis: u64,
    /// Writing them to `projection.db`.
    pub write_millis: u64,
    /// Documents a sweep found needing projection, since start-up.
    ///
    /// **Not the same shape as the index's `behind`, deliberately.** The index
    /// compares two local tables and can answer "how far behind am I" whenever
    /// it is asked. This layer's comparison is against the *store*, which is
    /// per-workspace and deliberately lazy — a workspace nobody has touched is
    /// not compared until something asks — so there is no cheap global number
    /// to report, and this is the honest substitute.
    ///
    /// **It is not a count of missed events, and reading it as one is wrong.**
    /// A workspace's *first* sweep finds every document it has never projected,
    /// which is the mechanism working rather than failing: after a restart the
    /// projector knows nothing until something asks about a workspace. The
    /// signal is the number climbing **after** a workspace is warm — that is a
    /// commit whose event did not arrive, reaching the projection only because
    /// something compared.
    pub stale_found: u64,
    /// Documents withheld because their source schema is newer than this build.
    pub incompatible: u64,
    /// Commits to documents of a kind this layer does not follow — a
    /// delegated run, a schedule ([`crate::model::NOT_PROJECTED`]) — since
    /// start-up. Each one is a fork, a walk and a history enrolment that did
    /// not happen; the number climbing is a run taking steps, not a problem.
    pub skipped: u64,
    /// Documents that failed [`MAX_RETRY_ATTEMPTS`] times and are no longer
    /// being retried, since start-up.
    ///
    /// **Not a worse [`Self::failed`], a different fact.** `failed` counts
    /// attempts and so climbs while something is merely slow to settle; this
    /// counts documents the layer has stopped working on, which is the number
    /// that says the read model is missing content and will stay missing until
    /// that document changes. One is worth looking at. It is deliberately not
    /// reset by a later success, because the question it answers is "did this
    /// store need a repair", not "does it need one now".
    pub abandoned: u64,
}

/// A document waiting to be projected.
struct Waiting {
    workspace: WorkspaceId,
    revision: Vec<u8>,
    first_seen: i64,
    last_seen: i64,
    retry_not_before: i64,
    retry_attempts: u8,
}

/// Keeps the projection level with the store.
pub struct Projector<S: Source> {
    source: S,
    projections: Projections,
    consumers: Vec<Box<dyn Consumer>>,
    pending: HashMap<DocumentId, Waiting>,
    /// Workspaces owed a sweep, and the remaining documents of the running one.
    owed: Vec<(WorkspaceId, u64)>,
    sweep: Vec<(WorkspaceId, DocumentId, Vec<u8>)>,
    /// Whose sweep is running, so finishing one can be reported against the
    /// workspace it was about.
    sweeping: Option<(WorkspaceId, u64)>,
    sweep_failed: bool,
    sweep_started: Option<std::time::Instant>,
    sweep_documents: usize,
    sweeps: Sweeps,
    /// Workspaces already swept this run.
    ///
    /// In memory rather than on disk, deliberately: a restart *should* sweep
    /// again, because a restart is exactly when this layer may have missed
    /// something.
    swept: std::collections::HashSet<WorkspaceId>,
    stats: ProjectorStats,
}

impl<S: Source> Projector<S> {
    /// **Nothing is swept until a workspace shows signs of life.**
    ///
    /// A sweep at construction was the obvious shape and it was wrong: it opened
    /// every workspace and every document at start-up, and the store's own
    /// promise is that start-up costs the same whether it holds one workspace or
    /// a thousand. Derived state does not get to spend somebody else's budget.
    ///
    /// So a workspace is swept the first time a commit is seen from it — by
    /// which point the store has it open anyway — or when
    /// [`request_sweep`](Self::request_sweep) asks. That is still how a document
    /// changed while this was stopped is noticed, how a discarded schema is
    /// rebuilt, and how a dropped document's projection is removed; it just
    /// happens when the workspace is in use rather than before anybody has asked
    /// for anything.
    pub fn new(source: S, projections: Projections) -> Self {
        Self {
            source,
            projections,
            consumers: Vec::new(),
            pending: HashMap::new(),
            owed: Vec::new(),
            sweep: Vec::new(),
            sweeping: None,
            sweep_failed: false,
            sweep_started: None,
            sweep_documents: 0,
            sweeps: Sweeps::default(),
            swept: std::collections::HashSet::new(),
            stats: ProjectorStats::default(),
        }
    }

    pub fn add_consumer(&mut self, consumer: Box<dyn Consumer>) {
        self.consumers.push(consumer);
    }

    /// The handle a consumer holds to ask for a sweep and see when one has
    /// finished. See [`Sweeps`] for why it exists at all.
    pub fn sweeps(&self) -> Sweeps {
        self.sweeps.clone()
    }

    pub fn stats(&self) -> ProjectorStats {
        let mut stats = self.stats.clone();
        stats.pending = self.pending.len();
        stats.sweeping = !self.sweep.is_empty() || !self.owed.is_empty();
        stats.sweep_remaining = self.sweep.len();
        stats
    }

    pub fn projections(&self) -> &Projections {
        &self.projections
    }

    #[cfg(test)]
    fn into_projections_for_test(self) -> Projections {
        self.projections
    }

    /// Ask for one workspace to be compared against the store. Resumable,
    /// batched, and it blocks nothing.
    pub fn request_sweep(&mut self, workspace: WorkspaceId) {
        let ticket = self.sweeps.issue(workspace);
        self.queue_sweep(workspace, ticket);
    }

    fn queue_sweep(&mut self, workspace: WorkspaceId, ticket: u64) {
        self.swept.insert(workspace);
        if let Some((_, queued)) = self
            .owed
            .iter_mut()
            .find(|(queued, _)| *queued == workspace)
        {
            *queued = (*queued).max(ticket);
        } else {
            self.owed.push((workspace, ticket));
        }
    }

    /// Do one unit of work. Returns whether there is more to do now.
    ///
    /// Order matters: draining first means a commit arriving during a sweep is
    /// not stuck behind it, and projecting before sweeping means somebody's
    /// typing outranks a rebuild nobody asked to watch.
    pub fn tick(&mut self, now_millis: i64) -> bool {
        for consumer in &mut self.consumers {
            consumer.flush_pending();
        }
        self.drain(now_millis);
        if self.project_due(now_millis) {
            return true;
        }
        self.sweep_step()
    }

    /// Take what the store has committed and fold it into what is waiting.
    fn drain(&mut self, now_millis: i64) {
        // Somebody outside asked for one — a query about to trust a workspace
        // nobody has touched since the store came up.
        for (workspace, ticket) in self.sweeps.take_requests() {
            self.queue_sweep(workspace, ticket);
        }
        let reminder_changes = self.source.take_reminder_committed();
        if !reminder_changes.is_empty() {
            for consumer in &mut self.consumers {
                consumer.reminder_changes(&reminder_changes);
            }
        }
        let dictionary_changes = self.source.take_dictionary_committed();
        if !dictionary_changes.is_empty() {
            for consumer in &mut self.consumers {
                consumer.dictionary_changes(&dictionary_changes);
            }
        }
        let identity_changes = self.source.take_identity_committed();
        if !identity_changes.is_empty() {
            for consumer in &mut self.consumers {
                consumer.identity_changes(&identity_changes);
            }
        }
        let mut followed = Vec::new();
        for event in self.source.take_committed() {
            // **The first sign of life from a workspace is what queues its
            // sweep.** The store has it open by now, so comparing revisions
            // costs a SQLite read per document and opens nothing further.
            if self.swept.insert(event.workspace) {
                let ticket = self.sweeps.issue(event.workspace);
                self.owed.push((event.workspace, ticket));
            }
            if !event.is_manifest && !self.follows(event.document) {
                // A run or a schedule: nothing to walk, and a fork that
                // would cost the whole history on every step. The listing
                // mirror already knows its kind because the manifest commit
                // that registered it came first.
                self.stats.skipped += 1;
                continue;
            }
            if event.is_manifest {
                // Nothing to walk — the manifest's shape is the store's own.
                // What it *holds* is another matter: a consumer that joins
                // against titles or the trash needs the new listing, so this is
                // where the mirror is refreshed.
                //
                // **And a listing that moved queues a sweep**, because a
                // listing is how a document that has never been written
                // announces that it exists. Without this, a document created
                // after the workspace's first sweep and never opened is never
                // projected at all — the read model would be missing a document
                // the store holds, and nothing would say so. A sweep over a
                // healthy workspace is one comparison per document and opens
                // none of them.
                if self.refresh_listing(event.workspace, now_millis) == Some(true) {
                    self.request_sweep(event.workspace);
                }
                continue;
            }
            followed.push(event.clone());
            self.pending
                .entry(event.document)
                .and_modify(|w| {
                    // **Coalesced to the newest location and revision.** Twenty
                    // commits are one projection because the projection
                    // re-reads state rather than replaying what it was told.
                    // A cross-workspace move preserves DocumentId, so keeping
                    // the first event's workspace would later fork the moved
                    // document from its old location and report a false
                    // document_not_found.
                    w.workspace = event.workspace;
                    w.revision = event.version_vector.clone();
                    w.last_seen = now_millis;
                    w.retry_not_before = now_millis;
                    w.retry_attempts = 0;
                })
                .or_insert(Waiting {
                    workspace: event.workspace,
                    revision: event.version_vector,
                    first_seen: now_millis,
                    last_seen: now_millis,
                    retry_not_before: now_millis,
                    retry_attempts: 0,
                });
        }
        self.source.record_activity(&followed, now_millis);
    }

    /// Whether a commit to this document is walked at all: its listed kind
    /// is not one of [`crate::model::NOT_PROJECTED`]. A document the mirror
    /// does not name yet is followed — the fallback is the ordinary path,
    /// which projects a run to no rows rather than refusing it.
    fn follows(&self, document: DocumentId) -> bool {
        match self.projections.listed_kind(document) {
            Ok(Some(kind)) => crate::model::projected(&kind),
            Ok(None) | Err(_) => true,
        }
    }

    /// Project one document whose debounce has elapsed. Returns whether it did.
    fn project_due(&mut self, now_millis: i64) -> bool {
        let due = self.pending.iter().find(|(_, w)| {
            now_millis >= w.retry_not_before
                && (now_millis - w.last_seen >= QUIET_MILLIS
                    || now_millis - w.first_seen >= CEILING_MILLIS)
        });
        let Some((&document, waiting)) = due else {
            return false;
        };
        let workspace = waiting.workspace;
        let revision = waiting.revision.clone();
        let mut waiting = self
            .pending
            .remove(&document)
            .expect("waiting entry exists");
        if !self.project(workspace, document, Some(revision), now_millis) {
            // A commit is a wake, not replayable truth. Retain a failed direct
            // projection so a transient fork/walk/write error cannot leave an
            // old due registration live forever. Backoff is bounded and a new
            // commit resets it; schema-too-new is a successful, deliberately
            // non-actionable projection and therefore is not retried.
            waiting.retry_attempts = waiting.retry_attempts.saturating_add(1);
            // **Retrying is for transient errors, and a bounded backoff alone
            // does not say which kind this was.** The delay caps, so a
            // deterministic failure — a malformed document, a constraint its
            // rows can never satisfy — repeats at that cap for as long as the
            // process lives, spending a fork and a walk each time to reach the
            // same error. Give up instead, and let the next thing that would
            // have re-queued it say whether anything changed: a commit inserts
            // a fresh `Waiting` with the attempts back at zero, and a sweep
            // compares against the store. Nothing is lost by stopping, because
            // nothing was being gained by continuing.
            if waiting.retry_attempts >= MAX_RETRY_ATTEMPTS {
                self.stats.abandoned += 1;
                tracing::warn!(
                    %document,
                    attempts = waiting.retry_attempts,
                    "giving up on a document until it changes again: {}",
                    self.stats.last_error.as_deref().unwrap_or("unknown")
                );
                return true;
            }
            let exponent = waiting.retry_attempts.min(4) as u32;
            let delay = QUIET_MILLIS.saturating_mul(1_i64 << exponent);
            waiting.first_seen = now_millis;
            waiting.last_seen = now_millis;
            waiting.retry_not_before = now_millis.saturating_add(delay);
            self.pending.insert(document, waiting);
        }
        true
    }

    /// One step of a sweep, starting the next owed workspace if none is running.
    fn sweep_step(&mut self) -> bool {
        if self.sweep.is_empty() {
            let Some((workspace, ticket)) = self.owed.pop() else {
                return false;
            };
            // **A sweep that could not read the store did not happen.** Saying
            // otherwise would tell a consumer this workspace had been compared
            // against a store that never answered — which is the one thing the
            // handshake exists to prevent.
            if !self.begin_sweep(workspace, ticket) {
                return false;
            }
            if self.sweep.is_empty() {
                self.finish_sweep();
                return false;
            }
        }

        let taken = self.sweep.len().min(SWEEP_BATCH);
        let batch: Vec<_> = self.sweep.drain(self.sweep.len() - taken..).collect();
        let mut cursor_can_advance = !self.sweep_failed;
        for (workspace, document, revision) in batch.into_iter().rev() {
            // **The comparison that makes a missed event a delay, not a loss** —
            // and it is against a position already read out of the journal, so a
            // workspace with nothing to do opens no documents at all.
            //
            // Equal is current. Older, newer and *incomparable* are all
            // re-projections: a recovered document's revision can go backwards,
            // and a rule of "only newer replaces older" would leave it stale for
            // good.
            if matches!(
                self.projections.staleness(document, &revision),
                Ok(Staleness::Current)
            ) {
                continue;
            }
            // Counted here rather than inside `project`, because this is the
            // one call site that reached it by *comparison* rather than by an
            // event — which is what makes the number mean "the events missed
            // one" rather than "somebody edited something".
            self.stats.stale_found += 1;
            if !self.project(workspace, document, Some(revision), 0) {
                self.sweep_failed = true;
                cursor_can_advance = false;
                // Keep processing independent documents, but leave the
                // durable cursor before the first failure. A restart retries
                // that comparison instead of skipping stale rows, while one
                // corrupt document does not cost healthy siblings their view.
                continue;
            }
            if cursor_can_advance {
                let cursor = format!("{}|{}", workspace.to_storage(), document.to_storage());
                let _ = self.projections.set_state(K_SWEEP_CURSOR, &cursor);
            }
        }

        if self.sweep.is_empty() {
            self.finish_sweep();
        }
        true
    }

    /// Set a sweep going. `false` means the store would not say what it holds,
    /// so nothing was compared and nothing may be claimed.
    fn begin_sweep(&mut self, workspace: WorkspaceId, ticket: u64) -> bool {
        self.sweep_failed = false;
        let positions = match self.source.positions(workspace) {
            Ok(positions) => positions,
            Err(PositionsError::WorkspaceNotFound) => {
                // Read before the delete, or there is nothing left to say the
                // events from. One query for the whole workspace.
                let known_attrs = self.projections.attrs_in_workspace(workspace).ok();
                let removed = match self.projections.remove_workspace(workspace) {
                    Ok(removed) => removed,
                    Err(why) => {
                        let why = why.to_string();
                        tracing::warn!(%workspace, "could not retire missing workspace: {why}");
                        self.stats.last_error = Some(why);
                        self.swept.remove(&workspace);
                        return false;
                    }
                };
                self.stats.removed += removed.len() as u64;
                for document in removed {
                    for consumer in &mut self.consumers {
                        consumer.removed(workspace, document);
                    }
                    if let Some(attrs) = known_attrs.as_ref().and_then(|k| k.get(&document)) {
                        let events = crate::events::cleared(workspace, document, attrs);
                        self.deliver_attribute_events(&events);
                    }
                }
                // A listing notification reaches consumers even when the
                // projection already had no rows. Search may still have stale
                // workspace facets from before a crash or schema rebuild.
                for consumer in &mut self.consumers {
                    consumer.listing_changed(workspace);
                }
                tracing::info!(%workspace, "retired missing workspace from derived state");
                self.sweeping = Some((workspace, ticket));
                return true;
            }
            Err(why) => {
                tracing::warn!(%workspace, "could not sweep: {why}");
                self.stats.last_error = Some(why.to_string());
                // Forget that it was swept, so the next commit or request tries
                // again. Otherwise one transient failure leaves the workspace
                // uncompared for the life of the process.
                self.swept.remove(&workspace);
                return false;
            }
        };
        self.sweeping = Some((workspace, ticket));

        // A sweep is also how a listing missed while this was stopped is
        // noticed — the same comparison, one layer up from the documents. It
        // does **not** queue another sweep the way a commit does: this is one.
        if self.refresh_listing(workspace, 0).is_none() {
            self.sweep_failed = true;
        }

        // Anything projected in this workspace that the store no longer holds
        // was dropped — not binned, which is a flag on a manifest entry and
        // keeps its content.
        let held: std::collections::HashSet<DocumentId> =
            positions.iter().map(|(d, _)| *d).collect();
        if let Ok(projected) = self.projections.revisions_in(workspace) {
            for document in projected {
                if !held.contains(&document) {
                    // Read before the delete: a dropped document's attrs are
                    // cleared to whoever tracks them, in the same vocabulary
                    // an edit uses.
                    let known_attrs = self.projections.document_attrs(document).ok();
                    let _ = self.projections.remove(document);
                    self.stats.removed += 1;
                    for consumer in &mut self.consumers {
                        consumer.removed(workspace, document);
                    }
                    if let Some(attrs) = known_attrs {
                        let events = crate::events::cleared(workspace, document, &attrs);
                        self.deliver_attribute_events(&events);
                    }
                }
            }
        }

        // A run or a schedule is never compared: it has no projection to be
        // stale against, and comparing would fork it every sweep for good.
        let mut positions = positions;
        if let Ok(unfollowed) = self
            .projections
            .listed_of_kinds(workspace, &crate::model::NOT_PROJECTED)
            && !unfollowed.is_empty()
        {
            positions.retain(|(d, _)| !unfollowed.contains(d));
        }

        // A stable order, so a cursor means something across a restart.
        positions.sort_by_key(|(d, _)| d.to_storage());
        if let Ok(Some(cursor)) = self.projections.state(K_SWEEP_CURSOR)
            && let Some((cursor_workspace, cursor_document)) = cursor.split_once('|')
            && cursor_workspace == workspace.to_storage()
        {
            positions.retain(|(d, _)| d.to_storage().as_str() > cursor_document);
        }
        // Reversed, so draining from the end walks in order.
        positions.reverse();
        self.sweep = positions
            .into_iter()
            .map(|(d, r)| (workspace, d, r))
            .collect();
        self.sweep_documents = self.sweep.len();
        self.sweep_started = Some(std::time::Instant::now());
        tracing::info!(
            %workspace,
            documents = self.sweep_documents,
            "projection workspace update started"
        );
        true
    }

    fn finish_sweep(&mut self) {
        let _ = self.projections.clear_state(K_SWEEP_CURSOR);
        self.stats.sweeps += 1;
        if let Some((workspace, ticket)) = self.sweeping.take() {
            let elapsed_ms = self
                .sweep_started
                .take()
                .map_or(0.0, |started| started.elapsed().as_secs_f64() * 1_000.0);
            tracing::info!(
                %workspace,
                documents = self.sweep_documents,
                elapsed_ms,
                "projection workspace update completed"
            );
            self.sweep_documents = 0;
            if !self.sweep_failed {
                self.sweeps.record(workspace, ticket);
            }
        }
    }

    /// Bring one workspace's mirrored listing level with the store, and tell
    /// the consumers only if it moved. Returns whether it did.
    ///
    /// Both callers are moments the store is already open: a manifest commit,
    /// and the start of a sweep. Nothing here opens a document — the store
    /// answers from its own derived index.
    fn refresh_listing(&mut self, workspace: WorkspaceId, at_millis: i64) -> Option<bool> {
        let name = self
            .source
            .workspaces()
            .ok()
            .and_then(|all| {
                all.into_iter()
                    .find(|(id, _)| *id == workspace)
                    .map(|(_, name)| name)
            })
            .unwrap_or_default();
        let entries = match self.source.entries(workspace) {
            Ok(entries) => entries,
            Err(why) => {
                tracing::warn!(%workspace, "could not read the listing: {why}");
                self.stats.last_error = Some(why);
                return None;
            }
        };
        // Trash is inherited (ADR-95): the manifest stamps only the binned
        // entry, so a child of a trashed document carries no stamp of its own
        // and would answer a consumer's live filter forever. Carry the
        // inherited stamp down the tree before mirroring — a consumer reads
        // rows, not trees.
        let mut entries = entries;
        apply_inherited_trash(&mut entries);
        match self
            .projections
            .set_listing(workspace, &name, &entries, at_millis)
        {
            // Unchanged. A manifest commit that moved nothing a consumer joins
            // against costs nothing downstream, which is why the digest exists.
            Ok(false) => Some(false),
            Ok(true) => {
                self.stats.listings += 1;
                for consumer in &mut self.consumers {
                    consumer.listing_changed(workspace);
                }
                Some(true)
            }
            Err(why) => {
                tracing::warn!(%workspace, "could not store the listing: {why}");
                self.stats.last_error = Some(why.0);
                None
            }
        }
    }

    /// Fork, walk, write, tell.
    fn project(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        revision: Option<Vec<u8>>,
        at_millis: i64,
    ) -> bool {
        let started = std::time::Instant::now();
        let revision = match revision {
            Some(r) => r,
            None => match self.source.revision(workspace, document) {
                Ok(r) => r,
                Err(why) => return self.failed(document, why),
            },
        };

        // Timed in three, because "the projection was slow" and "the *store*
        // was slow to hand it over" are different problems with different
        // fixes, and one number could not tell them apart.
        let forking = std::time::Instant::now();
        let forked = match self.source.fork(workspace, document) {
            Ok(doc) => doc,
            Err(why) => return self.failed(document, why),
        };
        let ws = match koru_core::schema::WorkspaceDoc::adopt(forked) {
            Ok(ws) => ws,
            Err(why) => return self.failed(document, why.to_string()),
        };
        let fork_millis = forking.elapsed().as_millis() as u64;

        if let koru_core::schema::Compatibility::TooNew { found, understood } = ws.compatibility() {
            if let Err(why) = self
                .projections
                .mark_schema_too_new(workspace, document, &revision, found, understood, at_millis)
            {
                return self.failed(document, why.0);
            }
            self.stats.incompatible += 1;
            for consumer in &mut self.consumers {
                consumer.source_incompatible(workspace, document);
            }
            return true;
        }

        let walking = std::time::Instant::now();
        let projection = match crate::walk::project(&ws, workspace, document, revision) {
            Ok(p) => p,
            Err(why) => return self.failed(document, why),
        };
        let walk_millis = walking.elapsed().as_millis() as u64;

        let writing = std::time::Instant::now();
        // The one look at the document's previous attr rows, before `replace`
        // swaps them — one query per projection, never one per node, and the
        // events derived from it are transient. A read that fails costs this
        // pass its events and nothing else: events are triggers, and a
        // consumer that missed one reconciles the way it already does.
        let previous_attrs = match self.projections.document_attrs(document) {
            Ok(attrs) => Some(attrs),
            Err(why) => {
                tracing::warn!(%document, "could not read previous attrs: {}", why.0);
                None
            }
        };
        let previous_nodes = self.projections.node_ids(document).ok();
        // The durable stamp, asked for once per projection and never per node.
        // A store that will not answer costs this document the accuracy of its
        // modified-at and nothing else — the digest gate below still holds the
        // stamp still for a re-projection that changed nothing, which is what a
        // sweep mostly is.
        let modified_millis = match self.source.modified_at(workspace, document) {
            Ok(stamp) => stamp,
            Err(why) => {
                tracing::warn!(%document, "could not read the durable modified-at: {why}");
                None
            }
        };
        if let Err(why) = self
            .projections
            .replace(&projection, at_millis, modified_millis)
        {
            return self.failed(document, why.0);
        }
        let write_millis = writing.elapsed().as_millis() as u64;

        // **After it is on the disk.** A consumer told about a projection that
        // failed to write would build on something nobody can read back.
        for consumer in &mut self.consumers {
            consumer.projected(&projection);
        }
        if let Some(previous_nodes) = previous_nodes {
            let now: std::collections::HashSet<_> = projection
                .nodes
                .iter()
                .map(|node| node.id.as_str())
                .collect();
            let removed: Vec<_> = previous_nodes
                .into_iter()
                .filter(|node| !now.contains(node.as_str()))
                .collect();
            if !removed.is_empty() {
                for consumer in &mut self.consumers {
                    consumer.entities_removed(workspace, document, &removed);
                }
            }
        }
        if let Some(previous) = previous_attrs {
            let events = crate::events::diff(workspace, document, &previous, &projection.nodes);
            self.deliver_attribute_events(&events);
        }
        self.stats.projected += 1;
        let took = started.elapsed().as_millis() as u64;
        self.stats.last_millis = took;
        self.stats.total_millis += took;
        self.stats.max_millis = self.stats.max_millis.max(took);
        self.stats.fork_millis = fork_millis;
        self.stats.walk_millis = walk_millis;
        self.stats.write_millis = write_millis;
        true
    }

    /// Hand one document's attribute events to every consumer — after
    /// `projected`/`removed`, same thread, and only when there is something to
    /// say. The log is counts and ids, never a key-with-value pair: a tag name
    /// or a due date in a log is somebody's content.
    fn deliver_attribute_events(&mut self, events: &[crate::events::AttributeEvent]) {
        if events.is_empty() {
            return;
        }
        tracing::debug!(
            target: "koru_projection::events",
            document = %events[0].document,
            count = events.len(),
            "attribute events"
        );
        for consumer in &mut self.consumers {
            consumer.attribute_events(events);
        }
    }

    fn failed(&mut self, document: DocumentId, why: String) -> bool {
        tracing::warn!(%document, "could not project: {why}");
        self.stats.failed += 1;
        self.stats.last_error = Some(why);
        false
    }
}

#[cfg(test)]
mod tests;
