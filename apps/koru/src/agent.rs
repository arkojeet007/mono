// SPDX-License-Identifier: MPL-2.0

//! This window's end of the conversation with the Koru Agent.
//!
//! The agent owns the things that are true of the *machine* rather than of a
//! window: the global shortcut, the search overlay it opens, and the decision
//! about which window a chosen result belongs in. This module is what makes
//! this window reachable by that decision.
//!
//! ```text
//!   koru.exe  ──Hello / WorkspaceChanged / Active──>  koru-agent
//!             <────────── Open(target) / Restore ───
//! ```
//!
//! ## A missing agent is not an error
//!
//! One is started if nothing answers, and if that does not work the window runs
//! on without one. What is lost is the global shortcut and cross-window
//! routing — which is the state Koru was in before the agent existed, so it is
//! a degradation rather than a failure. **Refusing to open a workspace because
//! a helper process would not start would be the worse bug**, and the same
//! reasoning the shell already applies to a version-skewed agent below.
//!
//! The store is the opposite case and stays that way: `Session::open` failing
//! is fatal, because an empty window must only ever mean an empty workspace.
//!
//! ### …and an unseated window means an empty *store* (S21)
//!
//! That rule is about **silence**, not about emptiness. What it forbids is a
//! window that looks like an empty workspace when the truth is a store that
//! would not answer — because then somebody types into something that cannot
//! save and is told nothing. A first-run window is the opposite of that case in
//! every respect: the store answered, its answer was that it holds no profile,
//! and the window says so on its face rather than by looking blank. It offers
//! *Create profile*, and *Manage workspaces with no profile* when a removal
//! retained some (R8/S21).
//!
//! **The difference is what exists to be named.** An empty workspace is still a
//! workspace — an id, an owner, a name — so drawing it empty is drawing the
//! truth. On first run nothing exists yet to name, and a profile document
//! carrying a freshly minted `ProfileId` and a persona nobody chose would be
//! this rule's own failure one layer earlier: a window that looks like somebody
//! is seated in it when nobody is. So the document is unseated, `id()` answers
//! `None` as it has always been able to, and nothing is written anywhere.
//! `koru_workspace::fromstore`'s `create_profile_if_empty` is the flag, and
//! [ADR-269](../../../docs/decisions/ADR-251-275.md) the reasoning.
//!
//! ## Nothing here may block the event loop
//!
//! A named pipe blocks when the peer stops reading. Every read is on a reader
//! thread and every write goes through a queue drained by a writer thread, so
//! the loop only ever touches a `Mutex<Vec<_>>` that is never held across
//! anything slower than a `push`. A window that froze because the agent was
//! being debugged would be a spectacular way to lose the trade.
//!
//! ## Two versions, and only one of them can be answered politely
//!
//! An unknown **framing** version means "this is not something I can read at
//! all", and the answer is to close: a refusal is itself a frame, and writing
//! one in a format the peer has just said it does not share is a promise we
//! cannot keep. An unknown **protocol** version is a refusal we can send,
//! because we agreed on the envelope. `koru-ipc` owns the first number and
//! `koru-agent-wire` the second, which is why the frame codec takes its version
//! as a parameter.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use koru_agent_wire::{
    AgentGreeting, FRAME_VERSION, FromAgent, FromShell, PROTOCOL_VERSION, Push, Request, Response,
    ShellHello, UpdateChoice, UpdateReport, UpdateStage,
};
use koru_core::{ProfileId, WorkspaceId};

/// How long to wait for an agent that is starting up.
///
/// A spawned agent has to bind its endpoint before it can be dialled, and
/// binding is the exclusion that stops a second one — so the wait is real
/// rather than a courtesy. Short, because the window opens without one.
const START_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);
const RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(100);
const ACCOUNT_RESULT_CAPACITY: usize = 32;
const BROWSER_PENDING_CAPACITY: usize = 2;

type ScopedAccountResult = (u64, u64, koru_agent_wire::AccountOperationResult);

mod account_admission;

pub type LocalProfileRemovalResult = (
    String,
    Result<koru_agent_wire::LocalProfileRemovalOutcome, String>,
);
pub type LocalProfileBindingResult = (
    String,
    Result<koru_agent_wire::LocalProfileAccountBindingOutcome, String>,
);

/// Something the agent asked this window to do, waiting for the loop.
#[derive(Clone, Debug)]
pub enum FromTheAgent {
    /// A result chosen in the overlay, already in this app's vocabulary.
    Open(Box<koru_search_ui::Target>),
    /// A validated notification gesture. Navigation and the semantic
    /// management handoff stay paired until the shell drains this one ask.
    ReminderSnooze(ReminderSnoozeAsk),
    /// Open the profile-global All Reminders overlay.
    OpenReminders,
    /// Come back from the notification area.
    Restore,
    /// Save and close: the agent is about to replace the files this process is
    /// running from.
    ///
    /// **The one part of the update conversation that is an ask.** Everything
    /// else the agent says about updating is a *report* — state a banner reads
    /// and redraws from, held in [`UpdateFeed`] — and routing that through this
    /// queue would mean a new `View` method to deliver it into every pane, for a
    /// value nobody outside one banner looks at. `Installing` is different in
    /// kind: it is the agent telling this window to stop existing, and only the
    /// shell can do that.
    CloseForUpdate,
    /// An install finished; open the page that says what it changed.
    ///
    /// **The second ask, and the same argument as the first.** A version is not
    /// state a banner draws from — it is a one-shot instruction to open a tab,
    /// and it arrives exactly once because the agent forgets it the moment this
    /// window has been told. Putting it in [`UpdateFeed`] instead would deliver
    /// it to every window on every subsequent change, which is the opposite of
    /// what it means.
    ShowWhatsNew {
        version: String,
    },
    ShowFeedback,
    /// Open Contact support: the notification area's row was chosen.
    ShowSupport,
    /// Set voice input up: the notification area's row was chosen and the agent
    /// has nowhere to ask the question.
    ///
    /// **A one-shot ask, like the two above it**, rather than state a surface
    /// draws from: it is somebody having just pressed a menu row, and it is
    /// answered by opening a modal in one window. See `koru_agent_wire::Push`
    /// for why the offer's own terms do not travel with it.
    SetUpVoice,
    /// Listen with this microphone. `None` is *follow the operating system*.
    /// The name travels as a string; see `koru_agent_wire::Push::VoiceInput`.
    SetVoiceInput(Option<String>),
    /// A machine-wide Dictation chord which Agent proved belonged to this
    /// Workspace process. It enters the existing local controller; `route`
    /// only correlates content-free lifecycle state back to Agent.
    DictationGesture {
        route: u64,
        gesture: koru_agent_wire::DictationGesture,
    },
    /// The authenticated Agent link ended. Always queued so a routed local
    /// dictation cannot retain an owner token across an Agent restart.
    AgentDisconnected,
    /// A trusted device reported that one exact Profile was deleted from the
    /// hosted Koru identity. The shell owns the recoverable local cascade.
    ProfileLifecycleDeleted(ProfileId),
    /// Open the tab for this running terminal session — a notification-area
    /// Agents row (Q-E8 P9). Parsed here, at the boundary: an id the wire
    /// carried that is not a session id is refused, never guessed at.
    ShowSession(koru_session_protocol::SessionId),
    /// Start a new agent session — the Agents menu's New. Answered by opening
    /// the `+` chooser, because which agent to start is the person's choice.
    NewAgentSession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReminderSnoozeAsk {
    /// Device-local one-shot token retained only until the event loop queues
    /// the exact navigation and semantic handoff.
    pub delivery: String,
    pub profile: koru_store_core::ProfileId,
    pub reminder: koru_store_core::ReminderId,
    pub occurrence: koru_store_core::OccurrenceKey,
    pub entity: koru_store_core::EntityRef,
}

/// The agent's last word on updating, and the way to answer it.
///
/// Cheap to clone and safe to hold in a view — which is the point, since the
/// banner and the Welcome screen both read it and neither can reach the socket.
/// Shaped like the `UpdateController` handle it replaced so the call sites that
/// draw from it barely moved.
///
/// **A report is state, not an event.** The agent pushes the whole of what it
/// knows on every change ([`Push::Update`] says why whole rather than a delta),
/// so this holds exactly one and overwrites it. Nothing here is a queue, and
/// there is no update state machine on this side to fall out of step with the
/// agent's: a banner is a function of the last report and nothing else.
///
/// [`take_dirty`](Self::take_dirty) is what tells the event loop a redraw is
/// owed — the report arrives on the reader thread, and a window that only
/// noticed on the next keystroke would sit on a finished download.
#[derive(Clone)]
pub struct UpdateFeed {
    last: Arc<Mutex<Option<UpdateReport>>>,
    /// The same queue every other write uses. `None` when there is no agent, and
    /// then every choice is dropped — see [`Self::absent`].
    writer: Option<Arc<Mutex<Option<std::sync::mpsc::Sender<FromShell>>>>>,
    /// The link's liveness, shared with the [`AgentLink`] that minted this.
    /// Cleared by the reader thread on EOF — so a choice can refuse to
    /// pretend it was sent when the socket is already known to be gone.
    live: Option<Arc<AtomicBool>>,
    /// Shared with the [`AgentLink`] that minted this, so a choice sent from a
    /// banner and a `Hello` sent from the shell cannot be given the same
    /// correlation id.
    next_id: Arc<AtomicU64>,
    dirty: Arc<AtomicBool>,
    test_upgrade: bool,
}

impl UpdateFeed {
    /// No agent, so no reports and nowhere to send a choice. What every test
    /// gets, and what a window that could not reach an agent runs on: the
    /// Welcome screen draws the buttons disabled and says nothing is wrong,
    /// because nothing is — see the module note on a missing agent.
    pub fn absent() -> Self {
        Self {
            last: Arc::new(Mutex::new(None)),
            writer: None,
            live: None,
            next_id: Arc::new(AtomicU64::new(1)),
            dirty: Arc::new(AtomicBool::new(false)),
            test_upgrade: false,
        }
    }

    /// A local, inert update offer for checking the install consent UI. It has
    /// no writer, so pressing it can never contact an agent or replace files.
    pub fn test_upgrade() -> Self {
        let mut feed = Self::absent();
        feed.test_upgrade = true;
        *feed.last.lock().unwrap_or_else(|error| error.into_inner()) = Some(UpdateReport {
            stage: UpdateStage::Ready,
            version: "0.0.3-test".into(),
            build_number: 0,
            title: "Test upgrade".into(),
            summary: Vec::new(),
            notes_url: None,
            received: 0,
            total: 0,
            can_install: true,
            download_url: None,
            message: None,
            detail: None,
            channel: "test".into(),
            automatic_checks: false,
            active_session_count: 0,
            session_generation: 1,
        });
        feed
    }

    /// The last report, cloned. `None` until the agent has said anything —
    /// which includes the whole of a window with no agent.
    pub fn report(&self) -> Option<UpdateReport> {
        self.last
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// This feed was minted by `--test-upgrade`, so its primary action must
    /// exercise the updater handoff locally instead of asking an Agent.
    pub fn is_test_upgrade(&self) -> bool {
        self.test_upgrade
    }

    /// The person pressed something. Queued, never sent from here — the module
    /// note's rule: nothing on the loop's thread may touch the socket.
    ///
    /// Answers whether the choice was actually accepted for delivery: `false`
    /// when there is no agent, the reader has already recorded the socket as
    /// gone, or the writer thread is dead. A caller that ignores `false` turns
    /// a dropped click into one that spends the whole round-trip timeout
    /// pretending to be slow.
    #[must_use]
    pub fn choose(&self, choice: UpdateChoice) -> bool {
        let Some(writer) = &self.writer else {
            return false;
        };
        if let Some(live) = &self.live
            && !live.load(Ordering::Acquire)
        {
            return false;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let Some(writer) = writer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        else {
            return false;
        };
        writer
            .send(FromShell::Request {
                id,
                body: Request::UpdateChoice { choice },
            })
            .is_ok()
    }

    /// Whether a report has arrived since this was last asked. Taken by the
    /// asking, so two windows cannot both wait for the other to notice.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// A feed already holding `report`, and the end of the queue its choices go
    /// into.
    ///
    /// **No socket and no agent.** A test that dialled one would find whatever
    /// the developer running it has open — the same rule `SearchSurface` follows
    /// — so this is a plain in-process channel, and asserting on what came out
    /// of it is asserting on exactly what would have been written.
    #[cfg(test)]
    pub(crate) fn for_test(report: UpdateReport) -> (Self, std::sync::mpsc::Receiver<FromShell>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self {
                last: Arc::new(Mutex::new(Some(report))),
                writer: Some(Arc::new(Mutex::new(Some(tx)))),
                live: None,
                next_id: Arc::new(AtomicU64::new(1)),
                dirty: Arc::new(AtomicBool::new(false)),
                test_upgrade: false,
            },
            rx,
        )
    }
}

/// The live link, or the absence of one.
///
/// Held by `Manager` and polled by the shell. Every method is safe to call when
/// there is no agent: they do nothing, which is what "runs on without one"
/// means in practice rather than in a comment.
#[derive(Clone)]
pub struct AgentLink {
    /// Replaced atomically when an Agent restarts. Views and update feeds keep
    /// this shared slot rather than a snapshot of the first connection.
    writer: Arc<Mutex<Option<std::sync::mpsc::Sender<FromShell>>>>,
    /// Credential scoping is available only after the Agent acknowledges this
    /// installation's durable Device ID.
    credentials: Option<koru_oauth_credentials::CredentialContext>,
    /// Why this device cannot sync, in the Agent's own word from its welcome
    /// (`identity_unavailable`, `incomplete_identity`, …); `None` when its
    /// identity opened. The Link Device window offers *Start over as a new
    /// device* exactly when this is set.
    identity_problem: Arc<Mutex<Option<String>>>,
    /// Where a *Start over* stands. See [`IdentityReset`].
    identity_reset: Arc<Mutex<IdentityReset>>,
    /// Cleared by the reader on EOF, so hiding never trusts a sender whose
    /// socket has already lost the only agent-owned recovery path.
    live: Arc<AtomicBool>,
    inbox: Arc<Mutex<Vec<FromTheAgent>>>,
    /// Correlation ids. Every request carries one and every response echoes it,
    /// because a stream that also carries unsolicited pushes cannot match
    /// replies by arrival order.
    ///
    /// `Arc` because [`UpdateFeed`] writes on the same stream and mints from the
    /// same counter.
    next_id: Arc<AtomicU64>,
    /// Requests whose answer is user-facing rather than a statement we may
    /// discard. Tracking the correlation id keeps an unrelated Agent failure
    /// from being mislabeled as a standalone-chat launch problem.
    pending_ai_chat: Arc<Mutex<HashSet<u64>>>,
    notification_controls: Arc<Mutex<VecDeque<(u64, koru_agent_wire::NotificationControlResult)>>>,
    pending_notification_controls: Arc<Mutex<HashSet<u64>>>,
    reminder_status: Arc<Mutex<VecDeque<(u64, Result<koru_agent_wire::ReminderDiagnostics, ()>)>>>,
    pending_reminder_status: Arc<Mutex<HashSet<u64>>>,
    /// Latest whole content-free scheduler snapshot pushed by Agent.
    reminder_diagnostics: Arc<Mutex<Option<koru_agent_wire::ReminderDiagnostics>>>,
    reminder_diagnostics_dirty: Arc<AtomicBool>,
    /// The durable update launch claim carried by this process, if any.
    /// Returned on every `Hello`: the token is opaque and unique, so retaining
    /// it also lets a shell reconnect after an Agent crash without letting an
    /// ordinary workspace change spend reopen intent.
    reopen_claim: Option<String>,
    /// The latest window presence. A replacement Agent starts with an empty
    /// registry, so reconnecting without replaying this would leave native
    /// notification actions unable to find the already-open window.
    presence: Arc<Mutex<Option<Request>>>,
    /// Connection identity and bounded reconnect ownership. An old reader may
    /// observe EOF after its replacement is live; only the current generation
    /// may clear the shared writer and schedule recovery.
    connection_generation: Arc<AtomicU64>,
    reconnecting: Arc<AtomicBool>,
    reconnect_root: Option<PathBuf>,
    reconnect_enabled: bool,
    /// The Agent said it is stopping because a person asked it to
    /// ([`Push::Quitting`]). Read when the link ends: a quit is not a crash,
    /// and starting a replacement would undo the person's *Quit*. Cleared by
    /// the next connection, whoever started its Agent.
    quit_announced: Arc<AtomicBool>,
    /// Whether the Agent last said it has a notification-area icon
    /// ([`Push::NotificationArea`]). `false` until it says so and again
    /// whenever the link ends: hiding behind an icon is safe only while there
    /// is an Agent to show it.
    notification_area: Arc<AtomicBool>,
    /// In-pane reports produced by the reader thread, drained one at a time by
    /// the shell. A queue preserves two rapid gestures instead of letting the
    /// latter overwrite the first before the event loop wakes.
    notices: Arc<Mutex<VecDeque<koru_workspace_host::ErrorReport>>>,
    /// The agent's last [`UpdateReport`], and whether it has been looked at.
    /// Handed out by [`Self::update_feed`]; see [`UpdateFeed`] for why this is
    /// held as state rather than queued into `inbox`.
    update: Arc<Mutex<Option<UpdateReport>>>,
    update_dirty: Arc<AtomicBool>,
    /// Whole privacy-safe account state. Account UI arrives in Plan 3.6; this
    /// feed makes the Plan 3.5 transport usable without inventing that UI.
    account: Arc<Mutex<koru_agent_wire::AccountState>>,
    account_dirty: Arc<AtomicBool>,
    account_results: Arc<Mutex<VecDeque<ScopedAccountResult>>>,
    account_admissions: Arc<Mutex<account_admission::Admissions>>,
    pending_profile_removals: Arc<Mutex<HashMap<u64, String>>>,
    profile_removal_results: Arc<Mutex<VecDeque<LocalProfileRemovalResult>>>,
    pending_profile_bindings: Arc<Mutex<HashMap<u64, String>>>,
    profile_binding_results: Arc<Mutex<VecDeque<LocalProfileBindingResult>>>,
    /// Browser-only race state. Each bounded attempt records browser and Agent
    /// terminal sides until both arrive; capacity refusals are also bounded.
    /// Ordinary account results never consume either bound.
    browser_completions: Arc<Mutex<BrowserCompletions>>,
    /// Whole pairing UI state, updated by either the correlated response or an
    /// unsolicited Agent push. The QR secret never enters application logs.
    pairing: Arc<Mutex<koru_agent_wire::PairingView>>,
    pairing_dirty: Arc<AtomicBool>,
    /// Whole linked-device list. Like pairing state, this is a snapshot so the
    /// modal may close and reopen without replaying relationship deltas. The
    /// first element is this device itself, when the agent knows it.
    linked_devices: Arc<
        Mutex<(
            Option<koru_agent_wire::LinkedDeviceView>,
            Vec<koru_agent_wire::LinkedDeviceView>,
            Option<koru_agent_wire::RelayView>,
        )>,
    >,
    linked_devices_dirty: Arc<AtomicBool>,
    /// Connection generation that produced the linked-device snapshot. The
    /// UI uses it as the acknowledgement for its bootstrap request, so an
    /// answer lost with a dead writer is retried on the replacement link.
    linked_devices_generation: Arc<AtomicU64>,
    device_history: Arc<
        Mutex<
            Option<(
                String,
                Vec<koru_agent_wire::DeviceStateEventView>,
                Option<i64>,
            )>,
        >,
    >,
    device_history_dirty: Arc<AtomicBool>,
    /// The global-search chord the agent actually registered, as a label the
    /// taskbar Jump List can draw (`Ctrl+Shift+K`, or `None` when there is no
    /// chord). State, not an ask — stored like the update report and pairing
    /// view, never queued into `inbox`.
    shortcut: Arc<Mutex<Option<String>>>,
    shortcut_dirty: Arc<AtomicBool>,
    /// The agent process's pid, read off the transport at connect time (never
    /// from a message — see `koru_ipc::peer_pid`). `None` when there is no
    /// agent. Held so a taskbar *Search* click can pass the foreground grant
    /// on to the process that actually owns the overlay.
    peer_pid: Arc<Mutex<Option<u32>>>,
    /// What went wrong, if anything, for the one line the window shows.
    trouble: Arc<Mutex<Option<String>>>,
    /// Installed by the shell once its event loop exists. The reader queues a
    /// push first and wakes second, so `about_to_wait` cannot race an empty
    /// inbox and go back to sleep past a navigation.
    wake: Arc<Mutex<Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>>>,
}

impl AgentLink {
    /// Not connected, and not going to be. The state every test wants.
    pub fn absent() -> Self {
        Self {
            writer: Arc::new(Mutex::new(None)),
            credentials: None,
            identity_problem: Arc::new(Mutex::new(None)),
            identity_reset: Arc::new(Mutex::new(IdentityReset::Idle)),
            live: Arc::new(AtomicBool::new(false)),
            inbox: Arc::new(Mutex::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            pending_ai_chat: Arc::new(Mutex::new(HashSet::new())),
            notification_controls: Arc::new(Mutex::new(VecDeque::new())),
            pending_notification_controls: Arc::new(Mutex::new(HashSet::new())),
            reminder_status: Arc::new(Mutex::new(VecDeque::new())),
            pending_reminder_status: Arc::new(Mutex::new(HashSet::new())),
            reminder_diagnostics: Arc::new(Mutex::new(None)),
            reminder_diagnostics_dirty: Arc::new(AtomicBool::new(false)),
            reopen_claim: None,
            presence: Arc::new(Mutex::new(None)),
            connection_generation: Arc::new(AtomicU64::new(0)),
            reconnecting: Arc::new(AtomicBool::new(false)),
            reconnect_root: None,
            reconnect_enabled: false,
            quit_announced: Arc::new(AtomicBool::new(false)),
            notification_area: Arc::new(AtomicBool::new(false)),
            notices: Arc::new(Mutex::new(VecDeque::new())),
            update: Arc::new(Mutex::new(None)),
            update_dirty: Arc::new(AtomicBool::new(false)),
            account: Arc::new(Mutex::new(koru_agent_wire::AccountState::default())),
            account_dirty: Arc::new(AtomicBool::new(false)),
            account_results: Arc::new(Mutex::new(VecDeque::new())),
            account_admissions: Arc::new(Mutex::new(account_admission::Admissions::default())),
            pending_profile_removals: Arc::new(Mutex::new(HashMap::new())),
            profile_removal_results: Arc::new(Mutex::new(VecDeque::new())),
            pending_profile_bindings: Arc::new(Mutex::new(HashMap::new())),
            profile_binding_results: Arc::new(Mutex::new(VecDeque::new())),
            browser_completions: Arc::new(Mutex::new(BrowserCompletions::default())),
            pairing: Arc::new(Mutex::new(koru_agent_wire::PairingView {
                state: koru_agent_wire::PairingState::Idle,
                qr_payload: None,
                expires_in_seconds: 0,
                peer_name: None,
                error_code: None,
            })),
            pairing_dirty: Arc::new(AtomicBool::new(false)),
            linked_devices: Arc::new(Mutex::new((None, Vec::new(), None))),
            linked_devices_dirty: Arc::new(AtomicBool::new(false)),
            linked_devices_generation: Arc::new(AtomicU64::new(0)),
            device_history: Arc::new(Mutex::new(None)),
            device_history_dirty: Arc::new(AtomicBool::new(false)),
            shortcut: Arc::new(Mutex::new(None)),
            shortcut_dirty: Arc::new(AtomicBool::new(false)),
            peer_pid: Arc::new(Mutex::new(None)),
            trouble: Arc::new(Mutex::new(None)),
            wake: Arc::new(Mutex::new(None)),
        }
    }

    /// Seed the account state with what this build knows before any Agent
    /// answers. A build with no Koru Cloud (`koru_env::cloud_configured`) is
    /// *not configured* from the first frame — and stays so with no Agent at
    /// all — rather than *unavailable* until one says otherwise. The Agent of
    /// the same build answers the same, and its answer replaces this as usual.
    fn seed_cloud(&self, configured: bool) {
        if configured {
            return;
        }
        *self.account.lock().unwrap_or_else(|e| e.into_inner()) =
            koru_agent_wire::AccountState::not_configured();
        self.account_dirty.store(true, Ordering::Release);
    }

    /// A request-only Agent link for container tests. No socket, reader, or
    /// process is involved; the receiver observes the exact frame the writer
    /// thread would have been given.
    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, std::sync::mpsc::Receiver<FromShell>) {
        let (writer, receive) = std::sync::mpsc::channel();
        let link = Self::absent();
        *link
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(writer);
        (link, receive)
    }

    /// A handle on the update conversation, for the views that draw it.
    ///
    /// Cloned freely: the banner's copy, the Welcome screen's copy and the
    /// container's copy are one shared report and one shared queue.
    pub fn update_feed(&self) -> UpdateFeed {
        UpdateFeed {
            last: Arc::clone(&self.update),
            writer: Some(Arc::clone(&self.writer)),
            live: Some(Arc::clone(&self.live)),
            next_id: Arc::clone(&self.next_id),
            dirty: Arc::clone(&self.update_dirty),
            test_upgrade: false,
        }
    }

    #[allow(dead_code)] // consumed by the supplied Account surface in Plan 3.6
    pub fn account_state(&self) -> koru_agent_wire::AccountState {
        self.account
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    #[allow(dead_code)] // consumed by the supplied Account surface in Plan 3.6
    pub fn take_account_dirty(&self) -> bool {
        if self
            .account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .expire(std::time::Instant::now())
        {
            self.account_dirty.store(true, Ordering::Release);
        }
        self.account_dirty.swap(false, Ordering::AcqRel)
    }

    #[allow(dead_code)] // consumed by the supplied Account surface in Plan 3.6
    pub fn take_account_result(&self) -> Option<(u64, koru_agent_wire::AccountOperationResult)> {
        self.take_scoped_account_result()
            .map(|(_, generation, result)| (generation, result))
    }

    pub fn take_scoped_account_result(&self) -> Option<ScopedAccountResult> {
        self.account_results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    #[allow(dead_code)] // consumed by the supplied Account surface in Plan 3.6
    pub fn account(&self, command: koru_agent_wire::AccountCommand) {
        self.account_owned(None, command);
    }

    pub fn account_owned(
        &self,
        expected_owner_scope: Option<u64>,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.request(Request::Account {
            expected_owner_scope,
            command,
        });
    }
    pub fn account_tracked(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.account_tracked_owned(submission, None, command);
    }

    pub fn account_tracked_owned(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        expected_owner_scope: Option<u64>,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.account_tracked_owned_with(submission, expected_owner_scope, command, || {});
    }

    #[cfg(test)]
    fn account_tracked_with(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        command: koru_agent_wire::AccountCommand,
        before_enqueue: impl FnOnce(),
    ) {
        self.account_tracked_owned_with(submission, None, command, before_enqueue);
    }

    fn account_tracked_owned_with(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        expected_owner_scope: Option<u64>,
        command: koru_agent_wire::AccountCommand,
        before_enqueue: impl FnOnce(),
    ) {
        let mut admissions = self
            .account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(writer) = self.current_writer() else {
            admissions.fail(submission, false);
            self.account_dirty.store(true, Ordering::Release);
            let _ = wake_the_loop(&self.wake);
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        before_enqueue();
        let connection = self.connection_generation.load(Ordering::Acquire);
        if admissions.insert(id, submission, connection, std::time::Instant::now()) {
            if writer
                .send(FromShell::Request {
                    id,
                    body: Request::Account {
                        expected_owner_scope,
                        command,
                    },
                })
                .is_err()
            {
                admissions.failed_send(id);
                self.account_dirty.store(true, Ordering::Release);
                let _ = wake_the_loop(&self.wake);
            }
        } else {
            self.account_dirty.store(true, Ordering::Release);
            let _ = wake_the_loop(&self.wake);
        }
    }
    pub fn take_account_command_admission(
        &self,
    ) -> Option<koru_workspace_host::account_ui::CommandAdmission> {
        self.account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_admission()
    }

    pub fn take_account_command_failure(
        &self,
    ) -> Option<koru_workspace_host::account_ui::CommandFailure> {
        self.account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_failure()
    }
    pub fn account_command_deadline(&self) -> Option<std::time::Instant> {
        self.account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .deadline()
    }
    pub fn remove_local_profile(&self, profile: ProfileId) {
        let profile_id = profile.to_storage();
        let Some(writer) = self.current_writer() else {
            self.profile_removal_results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push_back((profile_id, Err("agent_unavailable".into())));
            let _ = wake_the_loop(&self.wake);
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending_profile_removals
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id, profile_id.clone());
        if writer
            .send(FromShell::Request {
                id,
                body: Request::RemoveLocalProfile { profile_id },
            })
            .is_err()
        {
            let profile_id = self
                .pending_profile_removals
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            if let Some(profile_id) = profile_id {
                self.profile_removal_results
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push_back((profile_id, Err("agent_disconnected".into())));
                let _ = wake_the_loop(&self.wake);
            }
        }
    }
    pub fn take_local_profile_removal(&self) -> Option<LocalProfileRemovalResult> {
        self.profile_removal_results
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    pub fn bind_local_profile_account(
        &self,
        profile: ProfileId,
        expected_identity_id: Option<String>,
        new_identity_id: String,
    ) {
        let profile_id = profile.to_storage();
        let Some(writer) = self.current_writer() else {
            self.profile_binding_results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push_back((profile_id, Err("agent_unavailable".into())));
            let _ = wake_the_loop(&self.wake);
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending_profile_bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id, profile_id.clone());
        if writer
            .send(FromShell::Request {
                id,
                body: Request::BindLocalProfileAccount {
                    profile_id,
                    expected_identity_id,
                    new_identity_id,
                },
            })
            .is_err()
        {
            let profile_id = self
                .pending_profile_bindings
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            if let Some(profile_id) = profile_id {
                self.profile_binding_results
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push_back((profile_id, Err("agent_disconnected".into())));
                let _ = wake_the_loop(&self.wake);
            }
        }
    }

    pub fn take_local_profile_binding(&self) -> Option<LocalProfileBindingResult> {
        self.profile_binding_results
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    pub fn install_wake_proxy(
        &self,
        proxy: winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>,
    ) {
        *self.wake.lock().unwrap_or_else(|error| error.into_inner()) = Some(proxy);
        // Startup replies can arrive before the window loop exists. Wake once
        // after installing it so onboarding consumes that buffered account scope.
        let _ = wake_the_loop(&self.wake);
    }

    /// The event-loop proxy the shell installed, for another per-window worker
    /// that has to wake the loop the same way this link's reader does. `None`
    /// until the shell has an event loop — and in every test, which is what
    /// keeps a test container from ever spawning a session backend.
    pub fn wake_proxy(
        &self,
    ) -> Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>> {
        self.wake
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Connect, starting an agent if nothing answers.
    ///
    /// `root` is `--root`, which has to reach the endpoint name: the agent, the
    /// store and this window all derive one from it, and a mismatch finds
    /// nothing rather than the wrong thing.
    pub fn connect(root: Option<&std::path::Path>, reopen_claim: Option<String>) -> Self {
        let mut link = Self::absent();
        link.seed_cloud(koru_env::cloud_configured());
        link.reopen_claim = reopen_claim;
        link.reconnect_root = root.map(Path::to_path_buf);
        link.reconnect_enabled = true;
        match dial(root) {
            Ok(connected) => {
                link.credentials = credential_context(root, &connected.device_id);
                *link
                    .identity_problem
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = connected.identity_problem;
                link.adopt(connected.stream);
            }
            Err(why) => {
                koru_ipc::ipc_debug!("agent: no link — {why}");
                *link.trouble.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
            }
        }
        link
    }

    /// Whether there is an agent on the other end.
    pub fn connected(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Whether that agent has a notification-area icon a hidden window can be
    /// brought back from. Implies [`Self::connected`].
    pub fn has_notification_area(&self) -> bool {
        self.connected() && self.notification_area.load(Ordering::Acquire)
    }

    /// The identity of the live Agent connection. A caller can bind a
    /// one-shot bootstrap ask to this value and retry only after reconnect.
    pub fn connected_generation(&self) -> Option<u64> {
        loop {
            let generation = self.connection_generation.load(Ordering::Acquire);
            if !self.live.load(Ordering::Acquire) {
                return None;
            }
            if generation == self.connection_generation.load(Ordering::Acquire) {
                return Some(generation);
            }
        }
    }

    pub fn credential_context(&self) -> Option<koru_oauth_credentials::CredentialContext> {
        self.credentials.clone()
    }

    fn current_writer(&self) -> Option<std::sync::mpsc::Sender<FromShell>> {
        self.writer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn replay_presence(&self) {
        if let Some(hello) = self
            .presence
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            self.request(hello);
        }
    }

    /// The one line worth showing a person, taken by the asking.
    pub fn take_trouble(&self) -> Option<String> {
        self.trouble
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// One asynchronous Agent failure that belongs in this workspace.
    ///
    /// The reader queues before waking. The shell takes one per maintenance
    /// pass, matching the view's existing rule that a newer failure may replace
    /// one already showing.
    pub fn take_notice(&self) -> Option<koru_workspace_host::ErrorReport> {
        self.notices
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    /// Everything the agent has asked for since this was last called.
    pub fn drain(&self) -> Vec<FromTheAgent> {
        std::mem::take(&mut *self.inbox.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Confirm that the shell event loop retained a routed snooze ask.
    ///
    /// The reader thread deliberately cannot call this: an item in its inbox
    /// is not yet owned by the event loop and would be lost if the process
    /// disconnected before the next maintenance pass.
    pub fn accept_reminder_snooze(&self, delivery: String) {
        self.request(Request::AcceptReminderSnooze { delivery });
    }

    /// Ask the agent to make sure the store is up.
    ///
    /// **Supervision, not proxying.** The answer is only that it is running;
    /// this process then connects to it directly and mints its own session
    /// credential, which is per connection and could not have been handed over
    /// even if we wanted it to be.
    pub fn ensure_store(&self, root: Option<&Path>) {
        self.request(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        });

        // The request travels through the writer thread and Agent starts Store
        // on another worker, so queuing it is not a readiness guarantee. A
        // direct `cargo run -p koru` used to race straight into App::new and
        // fail while Store was still binding its endpoint. Discovery is the
        // authoritative readiness signal; wait only while a live Agent can be
        // doing useful work, and keep the startup delay bounded.
        if self.current_writer().is_none() {
            return;
        }
        let deadline = std::time::Instant::now() + START_DEADLINE;
        loop {
            let opened = match root {
                Some(root) => koru_api_client::Client::open_at(koru_channel::segment(), root),
                None => koru_api_client::Client::open(koru_channel::segment()),
            };
            match opened {
                Ok(_) => return,
                Err(error)
                    if error.code() != koru_store_core::errors::Code::StorageFailed
                        || std::time::Instant::now() >= deadline =>
                {
                    return;
                }
                Err(_) => std::thread::sleep(RETRY_EVERY),
            }
        }
    }

    /// Ask the Agent to start or adopt this installation's voice worker.
    ///
    /// The voice client performs the readiness poll itself; this is only the
    /// supervision nudge, kept beside [`Self::ensure_store`] so service
    /// process ownership still has one route out of the window.
    pub fn ensure_voice(&self) {
        self.request(Request::EnsureService {
            service: koru_agent_wire::Service::Voice,
        });
    }

    /// Begin the two-minute, all-data device invitation owned by Agent.
    pub fn start_pairing(&self) {
        self.request(Request::StartPairing);
    }

    #[allow(dead_code)]
    pub fn refresh_pairing(&self) {
        self.request(Request::PairingStatus);
    }

    pub fn cancel_pairing(&self) {
        self.request(Request::CancelPairing);
    }

    pub fn accept_link_code(&self, code: String) {
        self.request(Request::AcceptLinkCode {
            code: koru_agent_wire::LinkCode(code),
        });
    }

    pub fn refresh_linked_devices(&self) {
        self.request(Request::LinkedDevices);
    }

    pub fn request_device_history(&self, device_id: String, before: Option<i64>) {
        self.request(Request::DeviceHistory { device_id, before });
    }

    pub fn unlink_device(&self, device_id: String) {
        self.request(Request::UnlinkDevice { device_id });
    }

    /// Rename this device. The refreshed list — including the `this_device`
    /// row — comes back as a `LinkedDevices` response.
    pub fn rename_device(&self, name: String) {
        self.request(Request::RenameDevice { name });
    }

    /// Apply this device's relay preference through the Agent that owns its
    /// network runtime. The refreshed linked-device snapshot carries the
    /// resulting switch and connection state back to the modal.
    pub fn set_relay_enabled(&self, enabled: bool) {
        self.request(Request::SetRelay { enabled });
    }

    /// Why this device cannot sync, if it cannot. See the field.
    pub fn identity_problem(&self) -> Option<String> {
        self.identity_problem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Start over as a new device. A refusal comes back as
    /// [`IdentityReset::Refused`]; success is the Agent stopping, which the
    /// reader turns into [`IdentityReset::Relaunch`] instead of a reconnect.
    pub fn reset_identity(&self) {
        let Some(writer) = self.current_writer() else {
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        *self
            .identity_reset
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = IdentityReset::Requested(id);
        let _ = writer.send(FromShell::Request {
            id,
            body: Request::ResetIdentity,
        });
    }

    /// The reset's outcome once it has one, taken once.
    pub fn take_identity_reset(&self) -> Option<IdentityReset> {
        let mut reset = self
            .identity_reset
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match *reset {
            IdentityReset::Refused(_) | IdentityReset::Relaunch => {
                Some(std::mem::replace(&mut *reset, IdentityReset::Idle))
            }
            IdentityReset::Idle | IdentityReset::Requested(_) => None,
        }
    }

    /// On EOF: a reset in flight means the Agent stopped *for* it, so the
    /// window relaunches onto the new identity rather than reconnecting —
    /// reconnect would refuse the changed Device ID anyway.
    fn relaunch_for_identity_reset(&self) -> bool {
        let mut reset = self
            .identity_reset
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !matches!(*reset, IdentityReset::Requested(_)) {
            return false;
        }
        *reset = IdentityReset::Relaunch;
        drop(reset);
        let _ = wake_the_loop(&self.wake);
        true
    }

    /// Return the latest whole state when it changed. This is deliberately a
    /// snapshot rather than deltas so a view may open halfway through pairing.
    pub fn take_pairing(&self) -> Option<koru_agent_wire::PairingView> {
        self.pairing_dirty.swap(false, Ordering::AcqRel).then(|| {
            self.pairing
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
    }

    /// Every linked device's id and display name, **without draining** the
    /// dirty flag [`Self::take_linked_devices`] reads.
    ///
    /// Peer names are wanted by surfaces that are not the pairing modal — the
    /// context column's Cloud tab turns a peer origin into "from WIN10-rootB"
    /// with this — and a second consuming reader would make the modal miss the
    /// change that woke it.
    pub fn linked_device_names(&self) -> Vec<(String, String)> {
        let held = self
            .linked_devices
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        held.0
            .iter()
            .chain(held.1.iter())
            .map(|device| (device.device_id.clone(), device.display_name.clone()))
            .collect()
    }

    /// The linked devices as this window last heard them, **without**
    /// clearing the change flag. The Link Device dialog consumes the flag;
    /// a run view only wants to put a name beside the device executing a
    /// run, and consuming it here would leave that dialog blank.
    pub fn linked_devices(
        &self,
    ) -> (
        Option<koru_agent_wire::LinkedDeviceView>,
        Vec<koru_agent_wire::LinkedDeviceView>,
    ) {
        let held = self
            .linked_devices
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (held.0.clone(), held.1.clone())
    }

    pub fn take_linked_devices(
        &self,
    ) -> Option<(
        u64,
        Option<koru_agent_wire::LinkedDeviceView>,
        Vec<koru_agent_wire::LinkedDeviceView>,
        Option<koru_agent_wire::RelayView>,
    )> {
        self.linked_devices_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| {
                let generation = self.linked_devices_generation.load(Ordering::Acquire);
                let (this_device, devices, relay) = self
                    .linked_devices
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                (generation, this_device, devices, relay)
            })
    }

    #[cfg(test)]
    pub(crate) fn answer_linked_devices_for_test(
        &self,
        this_device: Option<koru_agent_wire::LinkedDeviceView>,
        devices: Vec<koru_agent_wire::LinkedDeviceView>,
    ) {
        *self
            .linked_devices
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = (this_device, devices, None);
        self.linked_devices_generation.store(
            self.connection_generation.load(Ordering::Acquire),
            Ordering::Release,
        );
        self.linked_devices_dirty.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn connect_for_test(&self) {
        self.connection_generation.fetch_add(1, Ordering::AcqRel);
        self.live.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn disconnect_for_test(&self) {
        self.connection_generation.fetch_add(1, Ordering::AcqRel);
        self.live.store(false, Ordering::Release);
    }

    pub fn take_device_history(
        &self,
    ) -> Option<(
        String,
        Vec<koru_agent_wire::DeviceStateEventView>,
        Option<i64>,
    )> {
        self.device_history_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| {
                self.device_history
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
            })
            .flatten()
    }

    /// The agent's last word on the global-search chord, taken by the asking.
    ///
    /// `Some(Some(label))` is a chord; `Some(None)` is "there is no chord";
    /// `None` is "nothing new since last asked". The taskbar reads this and
    /// rebuilds its *Search* task title only when it changed.
    pub fn take_shortcut(&self) -> Option<Option<String>> {
        self.shortcut_dirty.swap(false, Ordering::AcqRel).then(|| {
            self.shortcut
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        })
    }

    /// Ask the agent to open the global search overlay, as though the hotkey
    /// had been pressed. The taskbar's *Search* task ends here.
    pub fn show_overlay(&self) {
        self.request(Request::ShowOverlay);
    }

    /// Open the machine-wide Koru AI chat window, or bring its existing
    /// instance forward. The agent owns the singleton decision so two Koru
    /// windows cannot race to start two chat processes.
    pub fn open_ai_chat(&self) {
        // `--ai-chat` and a stale jump-list task both land here; a build that
        // hides Koru AI asks for nothing (koru-features).
        if !koru_features::ai() {
            return;
        }
        self.request_ai_chat(Request::OpenAiChat);
    }

    pub fn open_chat_link(&self, chat: String, block: String) {
        // A chat link is document content and still draws; following it opens
        // nothing in a build that hides Koru AI (koru-features).
        if !koru_features::ai() {
            return;
        }
        self.request_ai_chat(Request::OpenChatLink {
            chat,
            block: Some(block),
        });
    }

    /// Validate a LaunchServices URL with the shared grammar, then use the
    /// same Agent requests as Windows URL activation and in-product links.
    pub fn open_external_url(&self, raw: &str) -> Result<(), String> {
        if !self.connected() {
            return Err(koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::AgentUnavailableForLink,
            )
            .into());
        }
        let request = request_for_external_url(raw)?;
        if matches!(request, Request::OpenChatLink { .. }) {
            // The same answer as an in-document chat link: nothing, in a
            // build that hides Koru AI. The Agent refuses it too.
            if !koru_features::ai() {
                return Ok(());
            }
            self.request_ai_chat(request);
        } else {
            self.request(request);
        }
        Ok(())
    }

    /// Pass this window's foreground grant on to the agent, so its overlay can
    /// take focus on a *Search* click. The grant came from the short-lived
    /// taskbar process; the agent owns the overlay, so the overlay — not Koru —
    /// is what should come forward.
    pub fn grant_foreground(&self) {
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
            if let Some(pid) = *self
                .peer_pid
                .lock()
                .unwrap_or_else(|error| error.into_inner())
            {
                // Best effort: without a pid (no agent, or credentials the
                // platform will not report) the overlay still opens, it just may
                // not take focus.
                let _ = unsafe { AllowSetForegroundWindow(pid) };
            }
        }
    }

    /// What this window is showing, so the agent can route a result to it.
    ///
    /// **`Hello` on every call, not `WorkspaceChanged` after the first.** The
    /// two do the same thing in the agent's registry — `hello` records
    /// `(connection, profile, workspace)` and a second one for the same
    /// connection replaces it — and a shell that had to remember which it had
    /// already sent would get it wrong exactly once, on the reconnect after an
    /// agent restart, which is the case nobody tests by hand. The message stays
    /// in the protocol because it says something `Hello` does not: *this is not
    /// my first*. Nothing acts on that yet.
    pub fn hello(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        profile_name: &str,
        workspace_name: &str,
        pinned: &[koru_agent_wire::SearchPin],
    ) {
        let hello = Request::Hello {
            profile: profile.as_uuid().to_string(),
            workspace: workspace.as_uuid().to_string(),
            profile_name: profile_name.to_owned(),
            workspace_name: workspace_name.to_owned(),
            pinned: pinned.to_vec(),
            reopen_claim: self.reopen_claim.clone(),
        };
        *self
            .presence
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(hello.clone());
        self.request(hello);
    }

    /// This window came to the front.
    ///
    /// **Carries no time**, deliberately. The agent stamps the order it
    /// observed from one counter; a timestamp from here would be one of several
    /// clocks, and a window paused in a debugger would report a moment from
    /// before it was paused.
    pub fn active(&self) {
        self.request(Request::Active);
    }

    /// Report the lifecycle of one Agent-routed local dictation. No content,
    /// target identity, device, or error text is admitted to this message.
    pub fn dictation_state(&self, route: u64, state: koru_agent_wire::DictationState) {
        self.request(Request::DictationState { route, state });
    }

    /// Queue one Settings notification operation and return its correlation
    /// id. The typed result is drained asynchronously by
    /// [`take_notification_control_for`](Self::take_notification_control_for).
    pub fn notification_control(
        &self,
        operation: koru_agent_wire::NotificationOperation,
    ) -> Option<u64> {
        let writer = self.current_writer()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending_notification_controls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id);
        if writer
            .send(FromShell::Request {
                id,
                body: Request::NotificationControl { operation },
            })
            .is_err()
        {
            self.pending_notification_controls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            return None;
        }
        Some(id)
    }

    /// Drain the oldest notification answer, uncorrelated. Tests only:
    /// production drains by correlation id
    /// ([`take_notification_control_for`](Self::take_notification_control_for)),
    /// because several windows share one `AgentLink` and an indiscriminate
    /// drain would let one Settings tab consume another's result.
    #[cfg(test)]
    pub fn take_notification_control(
        &self,
    ) -> Option<(u64, koru_agent_wire::NotificationControlResult)> {
        self.notification_controls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
    }

    /// Take only the correlated notification answer this controller owns.
    /// Several windows share one `AgentLink`; draining the front indiscriminately
    /// would let one Settings tab consume another's result.
    pub fn take_notification_control_for(
        &self,
        id: u64,
    ) -> Option<koru_agent_wire::NotificationControlResult> {
        let mut answers = self
            .notification_controls
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let at = answers.iter().position(|(answer, _)| *answer == id)?;
        answers.remove(at).map(|(_, result)| result)
    }

    /// Ask for current, content-free reminder scheduler facts. No subscription
    /// or timer is created; Settings calls this on open and after direct
    /// controls whose result may have changed health.
    pub fn reminder_status(&self) -> Option<u64> {
        let writer = self.current_writer()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending_reminder_status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id);
        if writer
            .send(FromShell::Request {
                id,
                body: Request::ReminderStatus,
            })
            .is_err()
        {
            self.pending_reminder_status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id);
            return None;
        }
        Some(id)
    }

    pub fn take_reminder_status_for(
        &self,
        id: u64,
    ) -> Option<Result<koru_agent_wire::ReminderDiagnostics, ()>> {
        let mut answers = self
            .reminder_status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let at = answers.iter().position(|(answer, _)| *answer == id)?;
        answers.remove(at).map(|(_, result)| result)
    }

    pub fn take_reminder_diagnostics(&self) -> Option<koru_agent_wire::ReminderDiagnostics> {
        self.reminder_diagnostics_dirty
            .swap(false, Ordering::AcqRel)
            .then(|| {
                self.reminder_diagnostics
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            })
            .flatten()
    }

    #[cfg(test)]
    pub(crate) fn answer_reminder_status_for_test(
        &self,
        id: u64,
        result: Result<koru_agent_wire::ReminderDiagnostics, ()>,
    ) {
        self.reminder_status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back((id, result));
    }

    #[cfg(test)]
    pub(crate) fn answer_notification_control_for_test(
        &self,
        id: u64,
        result: koru_agent_wire::NotificationControlResult,
    ) {
        self.notification_controls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back((id, result));
    }

    /// Wake the installed window loop after a non-Agent worker publishes an
    /// answer. This performs no work on the calling thread.
    pub fn wake_loop(&self) {
        let _ = wake_the_loop(&self.wake);
    }

    fn request(&self, body: Request) {
        let Some(writer) = self.current_writer() else {
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // A closed channel means the writer thread is gone, which means the
        // socket is. Nothing to do about it here: the reader has already
        // recorded it and the window keeps working.
        let _ = writer.send(FromShell::Request { id, body });
    }

    fn request_ai_chat(&self, body: Request) {
        let Some(writer) = self.current_writer() else {
            queue_ai_chat_notice(
                &self.notices,
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::AgentNotConnected),
            );
            let _ = wake_the_loop(&self.wake);
            return;
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut pending = self
            .pending_ai_chat
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Check while holding the same lock the reader takes after recording
        // EOF. An ask that raced the disconnect is then either in the set the
        // reader drains or sees `false` here; it cannot land just after that
        // drain and wait forever for an answer from a dead connection.
        if !self.live.load(Ordering::Acquire) {
            drop(pending);
            queue_ai_chat_notice(
                &self.notices,
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::AgentConnectionEnded),
            );
            let _ = wake_the_loop(&self.wake);
            return;
        }
        pending.insert(id);
        drop(pending);
        if writer.send(FromShell::Request { id, body }).is_err() {
            if fail_ai_chat_send(
                &self.pending_ai_chat,
                &self.notices,
                id,
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::AgentConnectionEnded),
            ) {
                let _ = wake_the_loop(&self.wake);
            }
        }
    }

    /// Take a connected stream and start the two threads that own it.
    fn adopt(&self, stream: koru_ipc::Stream) {
        // Read before the split consumes the stream: the pid is the only thing
        // we may later want to hand the foreground grant to (a taskbar *Search*
        // click forwards it to the agent's overlay), and it comes off the
        // transport, never out of a message.
        let peer_pid = koru_ipc::peer_pid(&stream);
        let (read, mut write) = koru_ipc::split(stream);
        let (tx, rx) = std::sync::mpsc::channel::<FromShell>();

        // **The writer owns the write half and nothing else touches it.** The
        // loop enqueues; if the agent stops reading, this thread blocks and the
        // window does not.
        if std::thread::Builder::new()
            .name("koru-agent write".into())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    if koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &message).is_err() {
                        break;
                    }
                    if write.flush().is_err() {
                        break;
                    }
                }
            })
            .is_err()
        {
            return;
        }

        let generation = self.publish_connection(tx.clone(), peer_pid);
        *self
            .trouble
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;

        let inbox = self.inbox.clone();
        let pending_ai_chat = Arc::clone(&self.pending_ai_chat);
        let notification_controls = Arc::clone(&self.notification_controls);
        let pending_notification_controls = Arc::clone(&self.pending_notification_controls);
        let reminder_status = Arc::clone(&self.reminder_status);
        let pending_reminder_status = Arc::clone(&self.pending_reminder_status);
        let reminder_diagnostics = Arc::clone(&self.reminder_diagnostics);
        let reminder_diagnostics_dirty = Arc::clone(&self.reminder_diagnostics_dirty);
        let notices = Arc::clone(&self.notices);
        let wake = Arc::clone(&self.wake);
        let update = Arc::clone(&self.update);
        let update_dirty = Arc::clone(&self.update_dirty);
        let account = Arc::clone(&self.account);
        let account_dirty = Arc::clone(&self.account_dirty);
        let account_results = Arc::clone(&self.account_results);
        let account_admissions = Arc::clone(&self.account_admissions);
        let pending_profile_removals = Arc::clone(&self.pending_profile_removals);
        let profile_removal_results = Arc::clone(&self.profile_removal_results);
        let pending_profile_bindings = Arc::clone(&self.pending_profile_bindings);
        let profile_binding_results = Arc::clone(&self.profile_binding_results);
        let browser_completions = Arc::clone(&self.browser_completions);
        let pairing = Arc::clone(&self.pairing);
        let pairing_dirty = Arc::clone(&self.pairing_dirty);
        let linked_devices = Arc::clone(&self.linked_devices);
        let linked_devices_dirty = Arc::clone(&self.linked_devices_dirty);
        let linked_devices_generation = Arc::clone(&self.linked_devices_generation);
        let device_history = Arc::clone(&self.device_history);
        let device_history_dirty = Arc::clone(&self.device_history_dirty);
        let shortcut = Arc::clone(&self.shortcut);
        let shortcut_dirty = Arc::clone(&self.shortcut_dirty);
        // Browser launching has its own bounded worker. Neither winit nor the
        // Agent reader can be held by a slow OS association handler.
        let browser_writer = Some(tx.clone());
        let browser_next_id = Arc::clone(&self.next_id);
        let browser_results = Arc::clone(&self.account_results);
        let browser_dirty = Arc::clone(&self.account_dirty);
        let browser_wake = Arc::clone(&self.wake);
        let browser_completion = browser_completion_handler(
            browser_writer,
            browser_next_id,
            browser_results,
            Arc::clone(&browser_completions),
            browser_dirty,
            browser_wake,
        );
        // An old OS browser launch can finish after Agent restarts and reuses
        // its account attempt numbers. Only this connection may complete it.
        let browser_completion = connection_browser_completion(
            Arc::clone(&self.connection_generation),
            generation,
            browser_completion,
        );
        let browser_tx =
            start_browser_worker(koru_window::open_uri, Arc::clone(&browser_completion));
        let reconnect_link = self.clone();
        let mut read = read;
        if std::thread::Builder::new()
            .name("koru-agent read".into())
            .spawn(move || {
                loop {
                    let (version, body) = match koru_ipc::frame::read_raw(&mut read) {
                        Ok(frame) => frame,
                        // EOF, or a socket that died. Either way there is
                        // nothing more coming and no heartbeat to miss.
                        Err(_) => break,
                    };
                    if version != FRAME_VERSION {
                        koru_ipc::ipc_debug!("agent: frame version {version} — closing");
                        break;
                    }
                    let Ok(message) = koru_ipc::frame::decode::<FromAgent>(&body) else {
                        koru_ipc::ipc_debug!("agent: a frame this build cannot read — closing");
                        break;
                    };
                    if reconnect_link.connection_generation.load(Ordering::Acquire) != generation {
                        return;
                    }
                    if let FromAgent::Response { id, body } = &message {
                        let mut reset = reconnect_link
                            .identity_reset
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if *reset == IdentityReset::Requested(*id) {
                            *reset = IdentityReset::Refused(match body {
                                Response::Failed { code, .. } => code.clone(),
                                _ => "unexpected_agent_response".into(),
                            });
                            drop(reset);
                            let _ = wake_the_loop(&wake);
                            continue;
                        }
                    }
                    if let FromAgent::Response { id, body } = &message {
                        let (tracked, failed) =
                            handle_ai_chat_response(&pending_ai_chat, &notices, *id, body);
                        if tracked {
                            if failed {
                                let _ = wake_the_loop(&wake);
                            }
                            continue;
                        }
                    }
                    if let FromAgent::Response { id, body } = &message {
                        let profile_id = pending_profile_removals
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(id);
                        if let Some(requested_profile) = profile_id {
                            let result = match body {
                                Response::LocalProfileRemoved {
                                    profile_id,
                                    outcome,
                                } if profile_id == &requested_profile => Ok(*outcome),
                                Response::Failed { code, .. } => Err(code.clone()),
                                _ => Err("unexpected_agent_response".into()),
                            };
                            profile_removal_results
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((requested_profile, result));
                            let _ = wake_the_loop(&wake);
                            continue;
                        }
                    }
                    if let FromAgent::Response { id, body } = &message {
                        let profile_id = pending_profile_bindings
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(id);
                        if let Some(requested_profile) = profile_id {
                            let result = match body {
                                Response::LocalProfileAccountBound {
                                    profile_id,
                                    outcome,
                                } if profile_id == &requested_profile => Ok(*outcome),
                                Response::Failed { code, .. } => Err(code.clone()),
                                _ => Err("unexpected_agent_response".into()),
                            };
                            profile_binding_results
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((requested_profile, result));
                            let _ = wake_the_loop(&wake);
                            continue;
                        }
                    }
                    if let FromAgent::Response { id, body } = &message
                        && account_admissions
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .response(*id, generation, body)
                    {
                        account_dirty.store(true, Ordering::Release);
                        let _ = wake_the_loop(&wake);
                        continue;
                    }
                    match message {
                        FromAgent::Push {
                            body: Push::Account { state },
                        }
                        | FromAgent::Response {
                            body: Response::Account { state },
                            ..
                        } => {
                            *account.lock().unwrap_or_else(|e| e.into_inner()) = state;
                            account_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Push {
                            body:
                                Push::AccountBrowser {
                                    owner_scope,
                                    generation,
                                    attempt,
                                    url,
                                },
                        } => match begin_owned_browser(
                            &browser_completions,
                            owner_scope,
                            generation,
                            attempt,
                        ) {
                            BeginBrowser::Accepted => {
                                if browser_tx.try_send((generation, attempt, url.0)).is_err() {
                                    browser_completion(generation, attempt, false);
                                }
                            }
                            BeginBrowser::Duplicate => {}
                            BeginBrowser::Full => browser_completion(generation, attempt, false),
                        },
                        FromAgent::Push {
                            body: Push::AccountChanged,
                        } => {
                            account_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Push {
                            body:
                                Push::AccountResult {
                                    owner_scope,
                                    generation,
                                    result,
                                },
                        } => {
                            if !accept_owned_agent_account_result(
                                &browser_completions,
                                &account_results,
                                owner_scope,
                                generation,
                                result,
                            ) {
                                continue;
                            }
                            account_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            id,
                            body: Response::NotificationControl { result },
                        } => {
                            pending_notification_controls
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .remove(&id);
                            notification_controls
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((id, result));
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            id,
                            body: Response::ReminderStatus { reminders },
                        } if pending_reminder_status
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&id) =>
                        {
                            reminder_status
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((id, Ok(reminders)));
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            id,
                            body: Response::Failed { .. },
                        } if pending_reminder_status
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&id) =>
                        {
                            reminder_status
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((id, Err(())));
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            id,
                            body: Response::Failed { .. },
                        } if pending_notification_controls
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .remove(&id) =>
                        {
                            notification_controls
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .push_back((
                                    id,
                                    koru_agent_wire::NotificationControlResult::Failed {
                                        code: koru_agent_wire::NotificationFailure::Unavailable,
                                    },
                                ));
                            let _ = wake_the_loop(&wake);
                        }
                        // **Stored, not queued.** A report is what the banner
                        // draws from; only `Installing` is an instruction, and
                        // that one *is* queued — see `FromTheAgent`.
                        FromAgent::Push {
                            body: Push::Update { report },
                        } => {
                            let stage = report.stage;
                            *update.lock().unwrap_or_else(|e| e.into_inner()) = Some(*report);
                            update_dirty.store(true, Ordering::Release);
                            let close_asked = stage == UpdateStage::Installing;
                            if close_asked {
                                inbox
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .push(FromTheAgent::CloseForUpdate);
                            }
                            let event_woken = wake_the_loop(&wake);
                            if close_asked {
                                // The close request exists only as this filed
                                // ask — there is no separate message on the
                                // wire — so its receipt is dated here.
                                // `event_woken=false` means the wake proxy is
                                // not installed yet and the ask waits for an
                                // unrelated wake.
                                tracing::info!(
                                    target: "koru::update",
                                    event_woken,
                                    "close-for-update received"
                                );
                            }
                            koru_ipc::ipc_debug!(
                                "agent: update report {stage:?}, event_woken={event_woken}"
                            );
                        }
                        FromAgent::Push {
                            body: Push::Pairing { view },
                        }
                        | FromAgent::Response {
                            body: koru_agent_wire::Response::Pairing { view },
                            ..
                        } => {
                            *pairing.lock().unwrap_or_else(|error| error.into_inner()) = *view;
                            pairing_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            body:
                                koru_agent_wire::Response::LinkedDevices {
                                    devices,
                                    this_device,
                                    relay,
                                },
                            ..
                        } => {
                            *linked_devices
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = (
                                this_device.map(|device| *device),
                                devices,
                                relay.map(|view| *view),
                            );
                            linked_devices_generation.store(generation, Ordering::Release);
                            linked_devices_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Response {
                            body:
                                koru_agent_wire::Response::DeviceHistory {
                                    device_id,
                                    events,
                                    next_before,
                                },
                            ..
                        } => {
                            *device_history
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) =
                                Some((device_id, events, next_before));
                            device_history_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        // Remembered for the end of the stream, which is
                        // where it matters: see `connection_ended`.
                        // State, like the chord: what `hide_to_tray` asks.
                        FromAgent::Push {
                            body: Push::NotificationArea { present },
                        } => {
                            reconnect_link
                                .notification_area
                                .store(present, Ordering::Release);
                        }
                        FromAgent::Push {
                            body: Push::Quitting,
                        } => {
                            koru_ipc::ipc_debug!("agent: quitting because a person asked");
                            reconnect_link.quit_announced.store(true, Ordering::Release);
                        }
                        // Stored like the pairing view, not queued: a chord is
                        // something the taskbar draws from, not an instruction.
                        FromAgent::Push {
                            body: Push::Shortcut { shortcut: chord },
                        } => {
                            *shortcut.lock().unwrap_or_else(|error| error.into_inner()) = chord;
                            shortcut_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Push {
                            body: Push::Open { target },
                        } => {
                            tracing::info!(
                                target: "koru::notification_activation",
                                profile = target.profile,
                                workspace = target.workspace,
                                document = target.document.as_deref().unwrap_or("none"),
                                node = target.node.as_deref().unwrap_or("none"),
                                "Koru received an Open target from Agent IPC"
                            );
                            if let Some(item) = translate(Push::Open { target }) {
                                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(item);
                                let event_woken = wake_the_loop(&wake);
                                tracing::info!(
                                    target: "koru::notification_activation",
                                    event_woken,
                                    "Koru queued the Agent Open target for its window loop"
                                );
                            } else {
                                tracing::warn!(
                                    target: "koru::notification_activation",
                                    "Koru refused an invalid Agent Open target"
                                );
                            }
                        }
                        FromAgent::Push {
                            body: Push::ReminderSnooze { intent },
                        } => {
                            if let Some(item) = translate(Push::ReminderSnooze { intent }) {
                                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(item);
                                let _ = wake_the_loop(&wake);
                            }
                        }
                        FromAgent::Push {
                            body: Push::ReminderDiagnostics { reminders },
                        } => {
                            *reminder_diagnostics
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(reminders);
                            reminder_diagnostics_dirty.store(true, Ordering::Release);
                            let _ = wake_the_loop(&wake);
                        }
                        FromAgent::Push { body } => {
                            if let Some(item) = translate(body) {
                                let label = match &item {
                                    FromTheAgent::Restore => "restore".to_owned(),
                                    FromTheAgent::OpenReminders => "reminders".to_owned(),
                                    FromTheAgent::SetUpVoice => "set-up-voice".to_owned(),
                                    // **The label says a choice arrived and
                                    // not which**: a device name is a person's
                                    // hardware, and this line reaches a log.
                                    FromTheAgent::SetVoiceInput(device) => match device {
                                        Some(_) => "set-voice-input device".to_owned(),
                                        None => "set-voice-input system-default".to_owned(),
                                    },
                                    FromTheAgent::DictationGesture { .. } => {
                                        "dictation gesture".to_owned()
                                    }
                                    FromTheAgent::AgentDisconnected => {
                                        "agent disconnected".to_owned()
                                    }
                                    FromTheAgent::CloseForUpdate => "close for update".to_owned(),
                                    FromTheAgent::ShowFeedback => "show feedback".to_owned(),
                                    FromTheAgent::ShowSupport => "show support".to_owned(),
                                    FromTheAgent::ShowWhatsNew { version } => {
                                        format!("what's new version={version}")
                                    }
                                    FromTheAgent::ProfileLifecycleDeleted(_) => {
                                        "profile lifecycle deleted".to_owned()
                                    }
                                    // Whether, never which: a session id is
                                    // routing data and this line reaches a
                                    // log.
                                    FromTheAgent::ShowSession(_) => "show-session".to_owned(),
                                    FromTheAgent::NewAgentSession => "new-agent-session".to_owned(),
                                    FromTheAgent::Open(target) => match target.as_ref() {
                                        koru_search_ui::Target::Store {
                                            profile,
                                            workspace,
                                            ..
                                        } => format!(
                                            "open profile={} workspace={}",
                                            profile.to_storage(),
                                            workspace.to_storage()
                                        ),
                                        koru_search_ui::Target::Chat { chat, block, .. } => {
                                            format!("open chat={chat} block={block}")
                                        }
                                    },
                                    FromTheAgent::ReminderSnooze(intent) => format!(
                                        "reminder snooze profile={} workspace={}",
                                        intent.profile.to_storage(),
                                        intent.entity.workspace().to_storage()
                                    ),
                                };
                                inbox.lock().unwrap_or_else(|e| e.into_inner()).push(item);
                                let event_woken = wake_the_loop(&wake);
                                koru_ipc::ipc_debug!(
                                    "agent: queued {label}, event_woken={event_woken}"
                                );
                            }
                        }
                        // **The remaining responses are read and dropped, on
                        // purpose.** Standalone-chat opens are correlated and
                        // handled above; nothing else this window sends has an
                        // answer it acts on:
                        // `EnsureService` is followed by the ordinary store
                        // connect, which is the real test of whether it worked,
                        // and the rest are statements. The id is still on the
                        // wire so that a request which *does* need an answer
                        // can be added without a protocol change.
                        FromAgent::Response { .. } => {}
                    }
                }
                // A previous reader can reach EOF after its replacement has
                // already been adopted. It does not own the new connection's
                // state and must not make it look dead.
                if reconnect_link.connection_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                reconnect_link.connection_ended(generation);
            })
            .is_err()
        {
            self.connection_ended(generation);
            return;
        }

        // A restarted Agent owns a new, empty window registry. Replaying the
        // most recent Hello is what makes native notification Open route back
        // into this already-running process with its exact document target.
        self.sync_connection_state();
        #[cfg(windows)]
        if let Some(peer_pid) = peer_pid {
            watch_agent_process(peer_pid, generation, self.clone());
        }
    }

    fn sync_connection_state(&self) {
        // Zero-Profile onboarding never sends workspace Hello. It still needs
        // the account scope before admitting sign-in, including after reconnect.
        self.account(koru_agent_wire::AccountCommand::Status);
        self.replay_presence();
    }

    /// Setup may block; only this tuple publication shares the UI admission lock.
    fn publish_connection(
        &self,
        writer: std::sync::mpsc::Sender<FromShell>,
        peer_pid: Option<u32>,
    ) -> u64 {
        let mut admissions = self
            .account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let generation = self
            .connection_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        if admissions.replace(generation) {
            self.account_dirty.store(true, Ordering::Release);
        }
        self.quit_announced.store(false, Ordering::Release);
        *self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(writer);
        *self
            .peer_pid
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = peer_pid;
        self.live.store(true, Ordering::Release);
        generation
    }

    fn connection_ended(&self, generation: u64) {
        let mut admissions = self
            .account_admissions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // The reader and the Windows peer-process watcher can discover the
        // same death together. Claim retirement atomically so only one of
        // them can clear shared state or start recovery; the winner advances
        // the generation before a replacement can be adopted.
        if self
            .connection_generation
            .compare_exchange(
                generation,
                generation.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        if admissions.retire(generation) {
            self.account_dirty.store(true, Ordering::Release);
        }
        *self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .peer_pid
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.live.store(false, Ordering::Release);
        self.notification_area.store(false, Ordering::Release);
        drop(admissions);
        let pending_removals = self
            .pending_profile_removals
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .map(|(_, profile)| (profile, Err("agent_disconnected".into())))
            .collect::<Vec<_>>();
        if !pending_removals.is_empty() {
            self.profile_removal_results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .extend(pending_removals);
            let _ = wake_the_loop(&self.wake);
        }
        let pending_bindings = self
            .pending_profile_bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .map(|(_, profile)| (profile, Err("agent_disconnected".into())))
            .collect::<Vec<_>>();
        if !pending_bindings.is_empty() {
            self.profile_binding_results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .extend(pending_bindings);
            let _ = wake_the_loop(&self.wake);
        }
        let browser_failures =
            finish_browser_disconnect(&self.browser_completions, &self.account_results);
        if browser_failures > 0 {
            self.account_dirty.store(true, Ordering::Release);
            let _ = wake_the_loop(&self.wake);
        }
        // The window's own consumers (dictation routing, the Voice row)
        // learn the link ended from the inbox, not from the reconnect.
        self.inbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(FromTheAgent::AgentDisconnected);
        let mut wake = true;
        wake |= fail_pending_notification_controls(
            &self.pending_notification_controls,
            &self.notification_controls,
        ) > 0;
        let abandoned: Vec<_> = self
            .pending_reminder_status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .collect();
        if !abandoned.is_empty() {
            self.reminder_status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .extend(abandoned.into_iter().map(|id| (id, Err(()))));
            wake = true;
        }
        if fail_pending_ai_chat(
            &self.pending_ai_chat,
            &self.notices,
            koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::AgentConnectionEndedBeforeAnswer,
            ),
        ) > 0
        {
            wake = true;
        }
        if wake {
            let _ = wake_the_loop(&self.wake);
        }
        koru_ipc::ipc_debug!("agent: the link ended");
        if self.relaunch_for_identity_reset() {
            koru_ipc::ipc_debug!("agent: stopped for an identity reset; relaunching");
            return;
        }
        if !self.recovers_from_link_end() {
            koru_ipc::ipc_debug!("agent: quit by request; not starting another");
            return;
        }
        self.schedule_reconnect();
    }

    /// Whether an ended link is a crash to recover from, by dialling and — when
    /// nothing answers — starting a new Agent.
    ///
    /// **Not after [`Push::Quitting`].** The tray's *Quit* used to be undone by
    /// exactly this: every open window took the ended link for a crash, found
    /// nothing listening and started the Agent again, which started the Store
    /// again. A window left open after a quit stays without an Agent, as one
    /// started with none does; a hidden one closes itself, because its way
    /// back has gone (`koru_workspace_host`'s tray rule).
    fn recovers_from_link_end(&self) -> bool {
        !self.quit_announced.load(Ordering::Acquire)
    }

    /// Recover one broken link without putting connection work or retries on
    /// the window thread. A supervised child can briefly retain the old pipe
    /// after Agent exits, so recovery gets three bounded attempts. One owner
    /// prevents every cloned view from trying to start its own Agent.
    fn schedule_reconnect(&self) {
        if !self.reconnect_enabled
            || self
                .reconnecting
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let link = self.clone();
        if std::thread::Builder::new()
            .name("koru-agent reconnect".into())
            .spawn(move || {
                let mut result = dial(link.reconnect_root.as_deref());
                for _ in 1..3 {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    result = dial(link.reconnect_root.as_deref());
                }
                link.reconnecting.store(false, Ordering::Release);
                match result {
                    Ok(stream) if link.reconnect_scope_matches(&stream.device_id) => {
                        koru_ipc::ipc_debug!("agent: reconnected after link ended");
                        *link
                            .identity_problem
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = stream.identity_problem;
                        link.adopt(stream.stream);
                        let _ = wake_the_loop(&link.wake);
                    }
                    Ok(_) => {
                        // Views retain the original installation's vault scope.
                        // A different durable identity requires a fresh launch.
                        koru_ipc::ipc_debug!("agent: reconnect refused a changed credential scope");
                    }
                    Err(why) => {
                        koru_ipc::ipc_debug!("agent: reconnect failed — {why}");
                        *link
                            .trouble
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = Some(why);
                        let _ = wake_the_loop(&link.wake);
                    }
                }
            })
            .is_err()
        {
            self.reconnecting.store(false, Ordering::Release);
        }
    }

    fn reconnect_scope_matches(&self, device_id: &str) -> bool {
        self.credentials.as_ref().is_some_and(|expected| {
            credential_context(self.reconnect_root.as_deref(), device_id).as_ref() == Some(expected)
        })
    }
}

fn credential_context(
    root: Option<&std::path::Path>,
    device_id: &str,
) -> Option<koru_oauth_credentials::CredentialContext> {
    let root = root
        .map(std::path::Path::to_path_buf)
        .or_else(koru_channel::channel_root)?;
    let device = koru_device::DeviceId::parse(device_id)?;
    koru_oauth_credentials::CredentialContext::new(koru_channel::segment(), &root, device).ok()
}

/// Windows may keep a named-pipe server handle alive in a supervised child
/// after Agent itself exits, delaying EOF on the reader. The peer process is
/// the actual liveness authority: wait on its kernel handle without polling,
/// then retire this connection generation immediately.
#[cfg(windows)]
fn watch_agent_process(peer_pid: u32, generation: u64, link: AgentLink) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let _ = std::thread::Builder::new()
        .name("koru-agent process watch".into())
        .spawn(move || unsafe {
            let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, peer_pid) else {
                return;
            };
            let ended = WaitForSingleObject(process, INFINITE) == WAIT_OBJECT_0;
            let _ = CloseHandle(process);
            if ended {
                link.connection_ended(generation);
            }
        });
}

fn fail_pending_notification_controls(
    pending: &Mutex<HashSet<u64>>,
    results: &Mutex<VecDeque<(u64, koru_agent_wire::NotificationControlResult)>>,
) -> usize {
    let abandoned: Vec<_> = pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .drain()
        .collect();
    let count = abandoned.len();
    results
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(abandoned.into_iter().map(|id| {
            (
                id,
                koru_agent_wire::NotificationControlResult::Failed {
                    code: koru_agent_wire::NotificationFailure::Unavailable,
                },
            )
        }));
    count
}

fn handle_ai_chat_response(
    pending: &Mutex<HashSet<u64>>,
    notices: &Mutex<VecDeque<koru_workspace_host::ErrorReport>>,
    id: u64,
    body: &Response,
) -> (bool, bool) {
    if !pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&id)
    {
        return (false, false);
    }
    if let Response::Failed { code, message } = body {
        let lang = crate::format::lang();
        let details = match code.as_str() {
            "update_in_progress" => koru_i18n::t(lang, koru_i18n::Key::UpdateInstallInProgress),
            "launch_failed" if message.contains("is not beside this executable") => {
                koru_i18n::t(lang, koru_i18n::Key::AiChatAppMissing)
            }
            _ => koru_i18n::t(lang, koru_i18n::Key::AiChatOpenFailed),
        };
        queue_ai_chat_notice(notices, details);
        (true, true)
    } else {
        (true, false)
    }
}

fn queue_ai_chat_notice(
    notices: &Mutex<VecDeque<koru_workspace_host::ErrorReport>>,
    details: impl Into<String>,
) {
    notices
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(
            koru_workspace_host::ErrorReport::new(
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::AiChatOpenErrorTitle),
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::AiChatOpenErrorBody),
            )
            .with_details(details.into()),
        );
}

/// Complete an AI-chat request whose writer queue has closed.
///
/// The reader can observe EOF and drain the pending set between `send` and this
/// call. Only the side that actually removes the correlation id owns its
/// failure notice; otherwise that race would show the same error twice.
fn fail_ai_chat_send(
    pending: &Mutex<HashSet<u64>>,
    notices: &Mutex<VecDeque<koru_workspace_host::ErrorReport>>,
    id: u64,
    details: &str,
) -> bool {
    if !pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&id)
    {
        return false;
    }
    queue_ai_chat_notice(notices, details);
    true
}

fn fail_pending_ai_chat(
    pending: &Mutex<HashSet<u64>>,
    notices: &Mutex<VecDeque<koru_workspace_host::ErrorReport>>,
    details: &str,
) -> usize {
    let count = pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .drain()
        .count();
    for _ in 0..count {
        queue_ai_chat_notice(notices, details);
    }
    count
}

/// Nudge the event loop, and say whether there was one to nudge.
///
/// **Queue first, wake second**, at every call site: the other order lets
/// `about_to_wait` run between the two, find an empty inbox and sleep past the
/// thing that has just arrived.
fn wake_the_loop(
    wake: &Mutex<Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>>,
) -> bool {
    wake.lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|proxy| {
            proxy
                .send_event(koru_workspace_host::app::Wake::Agent)
                .is_ok()
        })
}

/// A push, in this app's vocabulary.
///
/// A target whose ids will not parse is **refused rather than guessed at**: the
/// same rule the search adapter follows, because `None` already means something
/// (a workspace-level hit, a title match) and cannot double as "unreadable".
fn start_browser_worker(
    opener: impl Fn(&str) -> std::io::Result<()> + Send + 'static,
    completed: Arc<dyn Fn(u64, u64, bool) + Send + Sync>,
) -> std::sync::mpsc::SyncSender<(u64, u64, String)> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<(u64, u64, String)>(1);
    let spawn = std::thread::Builder::new()
        .name("koru-account browser".into())
        .spawn(move || {
            while let Ok((generation, attempt, url)) = receiver.recv() {
                completed(generation, attempt, opener(&url).is_ok());
            }
        });
    if spawn.is_err() {
        // Dropping the receiver makes every enqueue fail and use the same
        // requester-visible failure path as a saturated browser queue.
    }
    sender
}

#[derive(Default)]
struct BrowserCompletions {
    attempts: VecDeque<BrowserAttempt>,
    refused: VecDeque<RefusedBrowser>,
    /// Generations whose deferred browser failure was published after the
    /// writer/reader disconnected. A buffered Agent terminal may still be
    /// decoded; this bounded tombstone keeps that frame from publishing twice
    /// until the old reader reaches EOF.
    fallback_terminals: VecDeque<u64>,
}

struct BrowserAttempt {
    owner_scope: u64,
    generation: u64,
    attempt: u64,
    browser: BrowserStatus,
    agent_terminal: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserStatus {
    Pending,
    Succeeded,
    Failed,
}

struct RefusedBrowser {
    owner_scope: u64,
    generation: u64,
    attempt: u64,
    cancel_requested: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BeginBrowser {
    Accepted,
    Duplicate,
    Full,
}

#[cfg(test)]
fn begin_browser(
    completions: &Mutex<BrowserCompletions>,
    generation: u64,
    attempt: u64,
) -> BeginBrowser {
    begin_owned_browser(completions, 0, generation, attempt)
}

fn begin_owned_browser(
    completions: &Mutex<BrowserCompletions>,
    owner_scope: u64,
    generation: u64,
    attempt: u64,
) -> BeginBrowser {
    let mut completions = completions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if completions
        .attempts
        .iter()
        .any(|pending| pending.generation == generation && pending.attempt == attempt)
        || completions
            .refused
            .iter()
            .any(|refused| refused.generation == generation && refused.attempt == attempt)
        || completions.fallback_terminals.contains(&generation)
    {
        return BeginBrowser::Duplicate;
    }
    if completions.attempts.len() == BROWSER_PENDING_CAPACITY {
        if completions.refused.len() == ACCOUNT_RESULT_CAPACITY {
            completions.refused.pop_front();
        }
        completions.refused.push_back(RefusedBrowser {
            owner_scope,
            generation,
            attempt,
            cancel_requested: false,
        });
        return BeginBrowser::Full;
    }
    completions.attempts.push_back(BrowserAttempt {
        owner_scope,
        generation,
        attempt,
        browser: BrowserStatus::Pending,
        agent_terminal: false,
    });
    BeginBrowser::Accepted
}

fn connection_browser_completion(
    current: Arc<AtomicU64>,
    connection: u64,
    complete: Arc<dyn Fn(u64, u64, bool) + Send + Sync>,
) -> Arc<dyn Fn(u64, u64, bool) + Send + Sync> {
    Arc::new(move |generation, attempt, succeeded| {
        if current.load(Ordering::Acquire) == connection {
            complete(generation, attempt, succeeded);
        }
    })
}

fn browser_completion_handler(
    writer: Option<std::sync::mpsc::Sender<FromShell>>,
    next_id: Arc<AtomicU64>,
    results: Arc<Mutex<VecDeque<ScopedAccountResult>>>,
    browser_completions: Arc<Mutex<BrowserCompletions>>,
    dirty: Arc<AtomicBool>,
    wake: Arc<Mutex<Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>>>,
) -> Arc<dyn Fn(u64, u64, bool) + Send + Sync> {
    Arc::new(move |generation, attempt, succeeded| {
        let should_publish_failure =
            {
                let mut completions = browser_completions
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(refused) = completions
                    .refused
                    .iter_mut()
                    .find(|refused| refused.generation == generation && refused.attempt == attempt)
                {
                    if refused.cancel_requested {
                        false
                    } else {
                        refused.cancel_requested = true;
                        true
                    }
                } else if let Some(index) = completions.attempts.iter().position(|pending| {
                    pending.generation == generation && pending.attempt == attempt
                }) {
                    let pending = &mut completions.attempts[index];
                    if pending.browser != BrowserStatus::Pending {
                        false
                    } else if pending.agent_terminal {
                        completions.attempts.remove(index);
                        false
                    } else if succeeded {
                        pending.browser = BrowserStatus::Succeeded;
                        false
                    } else {
                        pending.browser = BrowserStatus::Failed;
                        true
                    }
                } else {
                    // The exact attempt already observed both terminal sides.
                    false
                }
            };
        if !should_publish_failure {
            return;
        }
        let owner_scope = {
            let completions = browser_completions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            completions
                .attempts
                .iter()
                .find(|pending| pending.generation == generation && pending.attempt == attempt)
                .map(|pending| pending.owner_scope)
                .or_else(|| {
                    completions
                        .refused
                        .iter()
                        .find(|pending| {
                            pending.generation == generation && pending.attempt == attempt
                        })
                        .map(|pending| pending.owner_scope)
                })
        };
        let cancel_sent = writer.as_ref().is_some_and(|writer| {
            writer
                .send(FromShell::Request {
                    id: next_id.fetch_add(1, Ordering::Relaxed),
                    body: Request::Account {
                        expected_owner_scope: owner_scope,
                        command: koru_agent_wire::AccountCommand::Cancel { generation },
                    },
                })
                .is_ok()
        });
        // Browser failure requests cancellation, but is not the operation's
        // terminal fact: the hosted mutation may already have committed. Wait
        // for Agent success/Cancelled while the link is live. A broken writer
        // cannot deliver that answer, so use the same bounded disconnect
        // fallback immediately.
        if !cancel_sent && fail_browser_attempts_on_disconnect(&browser_completions, &results) > 0 {
            dirty.store(true, Ordering::Release);
            let _ = wake_the_loop(&wake);
        }
    })
}

#[cfg(test)]
fn accept_agent_account_result(
    browser_completions: &Mutex<BrowserCompletions>,
    results: &Mutex<VecDeque<ScopedAccountResult>>,
    generation: u64,
    result: koru_agent_wire::AccountOperationResult,
) -> bool {
    accept_owned_agent_account_result(browser_completions, results, 0, generation, result)
}

fn accept_owned_agent_account_result(
    browser_completions: &Mutex<BrowserCompletions>,
    results: &Mutex<VecDeque<ScopedAccountResult>>,
    owner_scope: u64,
    generation: u64,
    result: koru_agent_wire::AccountOperationResult,
) -> bool {
    let mut completions = browser_completions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(index) = completions
        .fallback_terminals
        .iter()
        .position(|fallback| *fallback == generation)
    {
        completions.fallback_terminals.remove(index);
        return false;
    }
    let mut suppress = false;
    let mut duplicate = false;
    for attempt in completions
        .attempts
        .iter_mut()
        .filter(|attempt| attempt.generation == generation)
    {
        duplicate |= attempt.agent_terminal;
        suppress |= attempt.browser == BrowserStatus::Failed;
        attempt.agent_terminal = true;
    }
    completions.attempts.retain(|attempt| {
        attempt.generation != generation || attempt.browser == BrowserStatus::Pending
    });
    completions.refused.retain(|refused| {
        if refused.generation == generation {
            suppress |= refused.cancel_requested;
            false
        } else {
            true
        }
    });
    if duplicate {
        return false;
    }
    let result = if suppress
        && matches!(
            result,
            koru_agent_wire::AccountOperationResult::Cancelled
                | koru_agent_wire::AccountOperationResult::Failed { .. }
        ) {
        koru_agent_wire::AccountOperationResult::Failed {
            code: "browser_unavailable".into(),
        }
    } else {
        result
    };
    drop(completions);
    push_account_result(results, (owner_scope, generation, result));
    true
}

fn fail_browser_attempts_on_disconnect(
    browser_completions: &Mutex<BrowserCompletions>,
    results: &Mutex<VecDeque<ScopedAccountResult>>,
) -> usize {
    let mut completions = browser_completions
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut failed = completions
        .attempts
        .iter()
        .filter(|attempt| attempt.browser == BrowserStatus::Failed)
        .map(|attempt| (attempt.owner_scope, attempt.generation))
        .chain(
            completions
                .refused
                .iter()
                .filter(|refused| refused.cancel_requested)
                .map(|refused| (refused.owner_scope, refused.generation)),
        )
        .collect::<Vec<_>>();
    failed.sort_unstable();
    failed.dedup();
    failed.retain(|(_, generation)| !completions.fallback_terminals.contains(generation));
    for (_, generation) in &failed {
        if completions.fallback_terminals.len() == ACCOUNT_RESULT_CAPACITY {
            completions.fallback_terminals.pop_front();
        }
        completions.fallback_terminals.push_back(*generation);
    }
    completions.attempts.clear();
    completions.refused.clear();
    drop(completions);
    let count = failed.len();
    for (owner_scope, generation) in failed {
        push_account_result(
            results,
            (
                owner_scope,
                generation,
                koru_agent_wire::AccountOperationResult::Failed {
                    code: "browser_unavailable".into(),
                },
            ),
        );
    }
    count
}

/// Finish the old connection after its reader has drained to EOF.
///
/// Writer failure retains fallback tombstones because the old reader may still
/// decode an already-buffered terminal. EOF proves that it cannot; releasing
/// them here lets a restarted Agent reuse its process-local generations.
fn finish_browser_disconnect(
    browser_completions: &Mutex<BrowserCompletions>,
    results: &Mutex<VecDeque<ScopedAccountResult>>,
) -> usize {
    let count = fail_browser_attempts_on_disconnect(browser_completions, results);
    browser_completions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .fallback_terminals
        .clear();
    count
}

fn push_account_result(
    results: &Mutex<VecDeque<ScopedAccountResult>>,
    result: ScopedAccountResult,
) {
    let mut results = results.lock().unwrap_or_else(|error| error.into_inner());
    if results.len() == ACCOUNT_RESULT_CAPACITY {
        results.pop_front();
    }
    results.push_back(result);
}

fn translate(push: Push) -> Option<FromTheAgent> {
    match push {
        Push::Account { .. }
        | Push::AccountBrowser { .. }
        | Push::AccountResult { .. }
        | Push::AccountChanged => None,
        Push::Restore => Some(FromTheAgent::Restore),
        Push::ProfileLifecycleDeleted { profile_id } => {
            ProfileId::parse(&profile_id).map(FromTheAgent::ProfileLifecycleDeleted)
        }
        Push::Voice => Some(FromTheAgent::SetUpVoice),
        Push::VoiceInput { device } => Some(FromTheAgent::SetVoiceInput(device)),
        Push::DictationGesture { route, gesture } => {
            Some(FromTheAgent::DictationGesture { route, gesture })
        }
        // Handled where the report can be stored — the reader matches it out
        // before this is reached, and this arm exists so adding a third push
        // is a compile error here rather than a message silently dropped.
        Push::Update { .. } => None,
        // Stored as whole content-free state by the reader, not queued.
        Push::ReminderDiagnostics { .. } => None,
        Push::OpenReminders => Some(FromTheAgent::OpenReminders),
        // **Queued, unlike a report.** A version is not state a banner reads
        // from; it is the agent asking this window to show the person what the
        // install produced, and it arrives exactly once. That is the same kind
        // of thing `CloseForUpdate` is, which is why both live in this queue and
        // `Push::Update` does not.
        Push::Updated { version } => Some(FromTheAgent::ShowWhatsNew { version }),
        Push::ShowFeedback => Some(FromTheAgent::ShowFeedback),
        Push::ShowSupport => Some(FromTheAgent::ShowSupport),
        Push::Pairing { .. } => None,
        // Stored by the reader (like `Pairing`), not queued — the chord is
        // something the taskbar draws from, never an instruction to act on.
        Push::Shortcut { .. } => None,
        // Remembered by the reader for the end of the stream; nothing for the
        // window to do until then.
        Push::Quitting => None,
        // Stored by the reader; a window reads it when somebody closes one.
        Push::NotificationArea { .. } => None,
        // The tray's Agents rows (Q-E8 P9). `None` is the menu's New; a named
        // session whose id will not parse is refused rather than guessed at —
        // the same rule `Open` follows below.
        Push::ShowSession { session } => match session {
            Some(raw) => match raw.parse::<koru_session_protocol::SessionId>() {
                Ok(session) => Some(FromTheAgent::ShowSession(session)),
                Err(_) => {
                    koru_ipc::ipc_debug!("agent: a session id that will not parse — refused");
                    None
                }
            },
            None => Some(FromTheAgent::NewAgentSession),
        },
        Push::Open { target } => {
            let profile = uuid_id(&target.profile)?;
            let workspace = uuid_id(&target.workspace)?;
            let document = match target.document.as_deref() {
                None => None,
                Some(raw) => Some(uuid_id(raw)?),
            };
            Some(FromTheAgent::Open(Box::new(
                koru_search_ui::Target::Store {
                    profile: koru_store_core::ids::ProfileId::from_uuid(profile),
                    workspace: koru_store_core::ids::WorkspaceId::from_uuid(workspace),
                    document: document.map(koru_store_core::ids::DocumentId::from_uuid),
                    node: target.node,
                    trashed: target.trashed,
                    match_text: target.match_text,
                    disposition: if target.new_window {
                        koru_search_ui::Disposition::NewWindow
                    } else {
                        koru_search_ui::Disposition::CurrentWindow
                    },
                },
            )))
        }
        Push::ReminderSnooze { intent } => {
            let profile = koru_store_core::ProfileId::parse(&intent.profile)?;
            let reminder = koru_store_core::ReminderId::parse(&intent.reminder)?;
            let occurrence = koru_store_core::OccurrenceKey::parse(&intent.occurrence)?;
            let entity = koru_store_core::EntityRef::parse(&intent.entity)?;
            Some(FromTheAgent::ReminderSnooze(ReminderSnoozeAsk {
                delivery: intent.delivery,
                profile,
                reminder,
                occurrence,
                entity,
            }))
        }
    }
}

fn uuid_id(raw: &str) -> Option<uuid::Uuid> {
    match uuid::Uuid::parse_str(raw) {
        Ok(id) => Some(id),
        Err(_) => {
            koru_ipc::ipc_debug!("agent: a target with an id that is not a uuid — refused");
            None
        }
    }
}

fn request_for_external_url(raw: &str) -> Result<Request, String> {
    match koru_link::parse(raw) {
        Ok(koru_link::KoruLink::Document { document, node }) => Ok(Request::OpenLink {
            document: document.to_string(),
            node: node.map(|node| node.to_string()),
        }),
        Ok(koru_link::KoruLink::Chat { chat, block }) => Ok(Request::OpenChatLink {
            chat: chat.to_string(),
            block: block.map(|block| block.to_string()),
        }),
        Ok(koru_link::KoruLink::Node(_)) => {
            Err(koru_i18n::t(crate::format::lang(), koru_i18n::Key::LinkMissingDocument).into())
        }
        Err(_) => Err(koru_i18n::t(crate::format::lang(), koru_i18n::Key::LinkUnsupported).into()),
    }
}

/// Reach an agent: connect, or start one and wait for it to bind.
struct ConnectedAgent {
    stream: koru_ipc::Stream,
    device_id: String,
    identity_problem: Option<String>,
}

/// Where a *Start over as a new device* stands, shared by every clone of the
/// link so the window that asked and the reader that hears agree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityReset {
    Idle,
    /// Sent under this correlation id; the Agent has not answered.
    Requested(u64),
    /// The Agent refused, with its code (`identity_intact`, …).
    Refused(String),
    /// The Agent stopped for the reset; the window must relaunch.
    Relaunch,
}

fn dial(root: Option<&std::path::Path>) -> Result<ConnectedAgent, String> {
    let endpoint = endpoint(root).map_err(|e| e.to_string())?;
    if let Ok(stream) = koru_ipc::connect(&endpoint) {
        return handshake(stream);
    }
    koru_ipc::ipc_debug!("agent: nothing listening — starting one");
    spawn(root)?;

    // **Binding and accepting are two moments**, and a client that arrives
    // between them has to wait rather than conclude there is no agent. The same
    // reasoning `ensure_running` applies to the store.
    let deadline = std::time::Instant::now() + START_DEADLINE;
    loop {
        if let Ok(stream) = koru_ipc::connect(&endpoint) {
            return handshake(stream);
        }
        if std::time::Instant::now() >= deadline {
            return Err("the agent did not start within three seconds".into());
        }
        std::thread::sleep(RETRY_EVERY);
    }
}

/// Where the agent listens.
///
/// **A second declaration of `koru_agent::endpoint`'s rule**, and deliberately
/// not a shared one: reaching the original means linking `koru-agent`, which is
/// a windowed application carrying winit, vello and the search surface. The
/// rule itself is two lines and is stated in both places; that it is stated
/// twice is in [TECHDEBT](../../../docs/contributing/techdebt/README.md) beside the other half of
/// this trade, the detached spawn below.
fn endpoint(root: Option<&std::path::Path>) -> Result<koru_ipc::Endpoint, koru_ipc::IpcError> {
    let segment = koru_channel::segment();
    match root {
        Some(root) => koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, segment, root),
        None => koru_ipc::Endpoint::for_service(koru_ipc::AGENT, segment),
    }
}

/// Start `koru-agent` beside this executable, detached.
///
/// **Beside this exe, never from `PATH`.** A nightly build must not start a
/// stable agent, and the exe's own directory is the only place that answers
/// "the agent that belongs to this build" without a lookup somebody can poison.
/// `--root` is passed through when there is one, or the pair would bind and
/// dial two different names.
fn spawn(root: Option<&std::path::Path>) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("this executable's own path is unreadable: {e}"))?;
    let agent = exe
        .parent()
        .ok_or_else(|| "this executable has no directory".to_string())?
        .join(if cfg!(windows) {
            "koru-agent.exe"
        } else {
            "koru-agent"
        });
    if !agent.exists() {
        return Err(format!("no agent beside this build at {}", agent.display()));
    }
    let mut command = std::process::Command::new(&agent);
    if let Some(root) = root {
        command.arg("--root").arg(root);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("the agent would not start: {e}"))
}

/// Let the agent outlive the window that started it, and give it no console.
#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(_command: &mut std::process::Command) {}

/// Introduce ourselves, and decide whether we can talk at all.
fn handshake(stream: koru_ipc::Stream) -> Result<ConnectedAgent, String> {
    // A handshake that hung would hang the window's start-up, which is the one
    // moment nothing else is happening to notice it.
    koru_ipc::set_timeouts(&stream, Some(std::time::Duration::from_secs(2)));
    // **A borrow, not a split.** `koru_ipc::split` consumes the stream, and the
    // timeouts above have to come off again afterwards — so the handshake runs
    // on `&Stream`, which is `Read` and `Write`, and the split happens only once
    // the link is established. The agent's own `greet` does the same thing from
    // the other side.
    let mut read = &stream;
    let mut write = &stream;

    let hello = ShellHello {
        product: "koru".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    // Sent bare rather than inside `FromShell`: the greeting is what decides
    // whether the request vocabulary applies at all, so it cannot travel in it.
    koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &hello)
        .and_then(|()| write.flush())
        .map_err(|e| format!("the agent would not take a greeting: {e}"))?;

    let (version, body) = koru_ipc::frame::read_raw(&mut read)
        .map_err(|e| format!("the agent said nothing back: {e}"))?;
    if version != FRAME_VERSION {
        // Closed, not refused — see the module note.
        return Err(format!(
            "the agent frames its messages as version {version} and this build reads {FRAME_VERSION}"
        ));
    }
    let greeting: AgentGreeting = koru_ipc::frame::decode(&body)
        .map_err(|e| format!("the agent's greeting could not be read: {e}"))?;
    match greeting {
        AgentGreeting::Welcome(welcome) => {
            let welcome = *welcome;
            koru_ipc::ipc_debug!(
                "agent: welcomed by {} (protocol {}, instance {})",
                welcome.agent_version,
                welcome.protocol_version,
                welcome.instance_id
            );
            // Back to blocking for the life of the link: the reader thread
            // spends nearly all of it parked, and a timeout there would be a
            // disconnect every two seconds.
            koru_ipc::set_timeouts(&stream, None);
            Ok(ConnectedAgent {
                stream,
                device_id: welcome.device_id,
                identity_problem: welcome.sync.sync_error.filter(|_| !welcome.sync.sync_ready),
            })
        }
        // **Not fatal.** A version-skewed agent must not stop somebody editing;
        // what is lost is the shortcut and cross-window routing, which is the
        // state before this existed. Both numbers are named, because
        // "incompatible" sends a person to read two changelogs to find out
        // which end is behind.
        AgentGreeting::Refused {
            what, ours, theirs, ..
        } => Err(format!(
            "the agent refused this window: {what} — it speaks {ours}, this build speaks {theirs}"
        )),
    }
}

#[cfg(test)]
mod tests;
