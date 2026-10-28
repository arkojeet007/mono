// SPDX-License-Identifier: MPL-2.0

//! What reaches the event loop, and the one way anything reaches it.
//!
//! ## One loop; every other thread is a producer
//!
//! Four sources of work, and **exactly one** of them may touch a window:
//!
//! ```text
//! hotkey thread   GetMessageW / WM_HOTKEY       ─┐
//! tray            (created on the winit thread)  ┤
//! ipc listener    blocking accept                ┼─> Waker::wake(AgentEvent)
//! connection      one reader thread each        ─┘         │
//!                                                          ▼
//!                                     winit loop — the only Window holder
//! ```
//!
//! The hotkey needs a thread of its own because `RegisterHotKey(None, ..)`
//! posts `WM_HOTKEY` to the *registering thread's* message queue, and winit does
//! not surface thread messages through `ApplicationHandler`. The listener needs
//! one because `accept` blocks. Each connection needs one because a read blocks.
//!
//! ## Why a `Waker` and not the proxy itself
//!
//! `EventLoopProxy` is winit's, and handing it to a tray callback, a hotkey
//! pump and a socket reader would spread winit through three modules that have
//! no other reason to know it exists. [`Waker`] is the whole of what a producer
//! needs — *raise this, and wake somebody* — and it is what makes those modules
//! testable with a channel instead of an event loop.
//!
//! **A send that fails is not an error.** It means the loop has ended, which is
//! the ordinary way this process stops; a producer thread that panicked on it
//! would turn a clean shutdown into a crash report.
//!
//! ## An event raised before the loop exists is held, not dropped
//!
//! The producers start *before* the loop does, and they have to: the endpoint is
//! bound and answered in [`crate::run`] steps 1 and 3, while the winit
//! `EventLoopProxy` does not exist until the loop is built at step 5. So there is
//! a window — winit's initialisation and a `TextEngine`'s fonts, tens of
//! milliseconds — in which the agent is fully answering the door and has nobody
//! home.
//!
//! That window is not hypothetical, because both callers that matter are
//! *watching* for the door to open and are therefore first through it: the
//! lifecycle gate polls `koru_ipc::connect` in a loop, and a shell that starts
//! its own agent (`apps/koru/src/agent.rs::dial`) spawns it and then retries
//! every few milliseconds. What was lost there was the opening `Hello` — the
//! message that puts a window in the registry — so the agent did not know the
//! window existed and a result for it routed nowhere, until the next workspace
//! switch happened to re-send it.
//!
//! [`Mailbox`] is the whole of the answer: it keeps what is raised until a loop
//! says where to put it, and replays in order. This used to be an
//! `Option<EventLoopProxy>` that silently discarded anything raised while it was
//! `None`, with a comment explaining that doing so was correct — which it would
//! have been, had nothing been running yet.

use std::sync::{Arc, Mutex, PoisonError};

use koru_agent_wire::Request;

use crate::registry::ConnectionId;

/// Action-specific hotkey payloads. Search never carries a virtual key that
/// could be observed, and Dictation never carries a quarantine trigger.
#[derive(Clone, Copy, Debug)]
pub enum HotkeyPress {
    Search {
        trigger: winit::keyboard::KeyCode,
    },
    Dictation {
        key: crate::global_input::VirtualKey,
        foreground: ForegroundPid,
    },
}

/// The foreground identity sampled on the hotkey pump at the instant Windows
/// delivered `WM_HOTKEY`. Zero/null/failure is kept distinct and fails closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForegroundPid {
    Known(u32),
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalDictationOwner {
    Agent,
    Workspace(ConnectionId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalDictationGesture {
    Press {
        route: u64,
        owner: LocalDictationOwner,
        key: crate::global_input::VirtualKey,
    },
    Release {
        route: u64,
        owner: LocalDictationOwner,
        key: crate::global_input::VirtualKey,
    },
    Cancel {
        route: u64,
        owner: LocalDictationOwner,
        key: crate::global_input::VirtualKey,
    },
}

/// Something that happened, on its way to the loop.
///
/// Not the wire type. [`Request`] is what a shell *said*; this is what the agent
/// has to *do about it*, and the difference is the connection it arrived on —
/// which the message deliberately does not carry, because a connection is the
/// agent's own idea and a shell has no business naming one.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    /// One of Agent's two named global actions was pressed. Search carries a
    /// physical trigger so the overlay can quarantine the chord that gave it
    /// focus; Dictation carries the registered virtual key so the active-hold
    /// observer reads that key and no other.
    Hotkey(HotkeyPress),
    /// The off-event-thread coordinator moved to a consequential state or
    /// produced one typed refusal. No transcript or target identity crosses
    /// this event boundary.
    GlobalDictationChanged(crate::dictation_coordinator::Snapshot),
    /// A fresh authoritative Global section is waiting on the nonblocking
    /// settings link.
    SettingsChanged,
    /// Device-local Voice consent re-read away from the window thread.
    GlobalConsentRefreshed {
        generation: u64,
        consent: Option<koru_voice_consent::Granted>,
    },
    /// A release-observer-vouched gesture for an existing in-Koru controller.
    /// Raw hold notices stay on the coordinator queue; only this routed action
    /// crosses winit, and it carries no transcript or target.
    LocalDictation(LocalDictationGesture),
    /// The dedicated Koru AI shortcut was pressed.
    AiHotkey,
    /// The blocking singleton status probe requested by `AiHotkey` finished.
    AiChatProbed {
        process: Option<crate::ai_chat::Process>,
    },
    /// An Agent-owned standalone-chat open finished on its worker thread.
    ///
    /// Shell-originated opens answer their correlated request directly. Tray,
    /// hotkey, and Search-result opens have no requesting socket, so their
    /// completion returns here instead; a failure can then be drawn on the
    /// Agent's existing overlay without either process launch or a native
    /// dialog blocking the event loop.
    AiChatOpened { error: Option<String> },
    /// Which chord the hotkey thread actually managed to register, once it
    /// knows — or `None` when every candidate was taken, and when an explicit
    /// `--root` meant none was ever attempted.
    ///
    /// **The answer is not knowable when the menu is built.** Registration
    /// happens on its own thread, races whatever else on the machine wants
    /// `Ctrl+Shift+K`, and falls through a list; the tray is created before any
    /// of that resolves. So the menu draws no accelerator until this arrives
    /// and then says exactly what is true — which is the whole point, because
    /// the previous hardcoded `Ctrl+Shift+K` was confidently wrong for the
    /// second agent on a machine.
    HotkeyRegistered {
        action: crate::hotkey::Action,
        shortcut: Option<String>,
    },
    /// Result of a requested runtime Dictation chord replacement. Registration
    /// is performed before persistence; `active` is always the chord the OS
    /// still owns after that attempt.
    DictationHotkeyPrepared {
        generation: u64,
        requested: String,
        active: Option<String>,
        prepared: bool,
    },
    DictationHotkeySettled {
        generation: u64,
        active: Option<String>,
    },
    /// Whether Ctrl+Shift+I was successfully reserved for Koru AI.
    AiHotkeyRegistered { available: bool },
    /// The notification-area icon became visible or stopped being. Raised by
    /// Linux's StatusNotifierItem service when a watcher (KDE's panel, GNOME's
    /// AppIndicator extension) comes or goes; the windows are told, because a
    /// window may hide behind the icon only while there is one.
    NotificationArea { present: bool },
    /// Something was chosen from the notification area.
    Tray(TrayAsk),
    /// A worker thread finished asking the voice worker what it is doing.
    ///
    /// **`None` means nothing was listening**, which is the ordinary answer and
    /// not a failure: asking is a connect, and a connect that finds nobody is
    /// how this agent discovers there is no worker. Nothing on this path ever
    /// *starts* one — the notification area is drawn when nothing is running,
    /// so the question has to be answerable offline.
    ///
    /// It is an event rather than a return value because the read is IPC with a
    /// timeout and the loop it feeds runs on the winit thread under
    /// `ControlFlow::Wait`. See `Agent::read_voice_worker_off_thread`.
    VoiceWorkerRead {
        worker: Option<crate::tray::VoiceWorker>,
        /// Which capture devices that worker said it can see, and empty when
        /// no worker answered.
        ///
        /// **On the same event as the worker facts** because they come off the
        /// same round trip to the same socket: two events would mean two
        /// dials, and a menu built from half of one answer.
        devices: Vec<koru_voice::InputDevice>,
    },
    /// The Agent-owned search dictation client queued a worker event.
    Dictation,
    /// The supervisor finished checking or starting the Store. `None` means it
    /// is available; `Some` is already person-facing copy for Search.
    StoreAvailability {
        warning: Option<String>,
        running: bool,
    },
    /// Content-free scheduler diagnostics produced off the winit thread.
    ReminderWorkerChanged {
        report: crate::reminders::SchedulerReport,
    },
    /// An activation was checked against the ledger and current Store
    /// authority on the reminder worker. Only routing remains for the loop.
    ReminderOpen {
        target: crate::reminders::ActivationTarget,
    },
    /// A count-only missed-summary activation asks only to restore its profile.
    /// The future global Reminders surface remains a separate design decision.
    ReminderProfileOpen {
        profile: koru_store_core::ids::ProfileId,
    },
    /// A validated snooze gesture awaiting Piece 6's management surface. No
    /// duration is inferred by the native boundary or the Agent.
    ReminderSnooze {
        target: crate::reminders::ActivationTarget,
    },
    NotificationControlAnswered {
        connection: ConnectionId,
        id: u64,
        result: koru_agent_wire::NotificationControlResult,
    },
    /// Store's pairing ALPN produced an incoming join, a completed exchange,
    /// or a failure. The evidence stays in the shared sync-control queue.
    PairingChanged,
    /// Content-free account state nudge; the controller owns the snapshot.
    AccountChanged,
    /// Browser handoff targeted to the shell that initiated this generation.
    AccountBrowser {
        owner_scope: u64,
        requester: ConnectionId,
        generation: u64,
        attempt: u64,
        url: koru_agent_wire::BrowserUrl,
    },
    AccountResult {
        owner_scope: u64,
        requester: ConnectionId,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    },
    /// The blocking Store call for a correlated Profile catalog marker finished.
    LocalProfileRemoved {
        requester: (ConnectionId, u64),
        profile_id: String,
        result: Result<koru_agent_wire::LocalProfileRemovalOutcome, (String, String)>,
    },
    /// The blocking Store Profile account compare-and-set finished.
    LocalProfileAccountBound {
        requester: (ConnectionId, u64),
        profile_id: String,
        result: Result<koru_agent_wire::LocalProfileAccountBindingOutcome, (String, String)>,
    },
    /// A shell connected and completed the handshake.
    ShellArrived {
        connection: ConnectionId,
        /// From the transport, never from the message. See `koru_ipc::peer_pid`.
        pid: Option<u32>,
    },
    /// A shell said something.
    ShellSaid {
        connection: ConnectionId,
        id: u64,
        body: Box<Request>,
        /// When the reader thread took the frame off the pipe. One process,
        /// one clock — the loop subtracts it to say how long a request sat
        /// between being read and being acted on, which is the half of a slow
        /// consent the routing log could not see.
        read_at: std::time::Instant,
    },
    /// A shell's socket ended — clean close, crash, or kill, all the same.
    ShellGone { connection: ConnectionId },
    // The three below are the update worker's, and they arrive the same way
    // every other variant does: raised on a worker thread through the [`Waker`]
    // and delivered on the loop's own thread. **Nothing here blocks the loop** —
    // checking, downloading, verifying and a modal dialog all happen on threads
    // of their own, and what reaches this enum is only the news that they moved.
    /// The update worker moved. The loop drains the controller and
    /// re-broadcasts.
    ///
    /// **It carries nothing**, deliberately: the controller is the state and a
    /// snapshot copied into an event would be one that had already moved on by
    /// the time the loop read it. This is a nudge, not a report.
    UpdateChanged,
    /// Somebody answered an update offer from a dialog or the tray rather than
    /// over a socket.
    ///
    /// The socket path arrives as a [`Request::UpdateChoice`] on a connection;
    /// this is the same answer with no connection behind it, because the person
    /// who gave it has no Koru window open. One vocabulary, two ways in.
    UpdateChoiceMade {
        choice: koru_agent_wire::UpdateChoice,
    },
    /// A dialog offer finished, so another may be shown.
    UpdateOfferHandled,
    /// The session host atomically compared an install consent and froze (or
    /// refused to freeze) new session creation. The request reply is delayed
    /// until this event so the winit thread never performs IPC.
    SessionShutdownPrepared {
        requester: Option<(ConnectionId, u64)>,
        result: crate::session_host::PrepareResult,
    },
    SessionCatalogRefreshed {
        build_number: u64,
        result: Result<crate::session_host::CatalogFact, String>,
    },
    /// Session-tree teardown completed on its lifecycle worker, carrying what
    /// the stop actually did — graceful, forced, nothing to stop, or timed
    /// out. A quit reports it; an update handoff fails closed on a timeout.
    SessionHostStopped {
        outcome: koru_agent_worker::supervisor::StopOutcome,
    },
    AgentQuitDecided {
        confirmed: bool,
        catalog: Result<crate::session_host::CatalogFact, String>,
    },
    /// A `koru://` link's document has been placed and is ready to route.
    ///
    /// Boxed because it is by far the largest variant, and every other event
    /// would otherwise pay for its size.
    LinkResolved {
        target: Box<koru_agent_wire::Target>,
    },
    /// The agents-menu watcher read a fresh session-host catalog. The loop
    /// only stores it; the tray re-projects on the wake that delivered it.
    /// Boxed for the reason [`AgentEvent::LinkResolved`] is: every other event
    /// would otherwise pay for the largest variant's size.
    AgentsCatalog {
        catalog: Box<crate::agents_menu::AgentsCatalog>,
    },
}

/// What the notification area can ask for.
///
/// **`Clone` rather than `Copy` since [`TrayAsk::ChooseVoiceInput`] arrived**:
/// a microphone is chosen by *name*, and a name is a `String`. Everything else
/// here is still a bare token, so the cost is a clone at the two call sites
/// that needed one rather than anywhere on a hot path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TrayAsk {
    /// Bring the shells back.
    Restore,
    /// Bring the selected open workspace's Koru window to the front.
    OpenWorkspace(ConnectionId),
    /// The icon's left button (Windows): bring back the workspace window the
    /// person is using, or open the Search overlay when no window is open.
    /// The gesture travels, not the meaning — the loop reads the registry when
    /// it answers, the same rule as every ask whose target is "now".
    IconClick,
    /// Open the overlay, the same as the hotkey.
    Search,
    /// Open the profile-global reminder destination.
    Reminders,
    /// Focus or launch Koru Workspace and offer starting a new agent session
    /// there — the Agents menu's **New** row (Q-E8 P9). Which agent to start
    /// is the person's choice, so the window opens its chooser rather than
    /// this ask naming a profile.
    NewAgent,
    /// Focus or launch Koru Workspace and open this running session's tab —
    /// one of the Agents menu's session rows.
    ShowAgentSession(koru_session_protocol::SessionId),
    /// Open or foreground the standalone Koru AI chat window.
    AiChat,
    /// Open the root this agent is running from in the platform file browser.
    OpenLocation,
    /// Open machine-local Koru settings (window geometry and Agent data).
    OpenLocalDataLocation,
    /// Open roaming settings shared with Koru.
    OpenSharedLocation,
    /// Recheck or start the Store and refresh the status row.
    RefreshStore,
    /// Open the running API Gateway's generated public documentation.
    OpenGatewayDocs,
    /// Open the Store's physical directory.
    OpenStoreLocation,
    /// Write and open the Store's current diagnostic report.
    OpenStoreStats,
    /// Choose a destination and ask the live Store for a consistent backup.
    BackupStore,
    /// Say which build this is, and open its release notes.
    ///
    /// **The tray is the only surface an agent alone has**, so "which build am
    /// I running" has to be answerable from it — several channels can be
    /// installed side by side and two of them can be running at once.
    About,
    /// Become the build that opens `koru://` links.
    ///
    /// **Explicit, and last-one-wins.** A scheme has one owner per user while
    /// every channel installs side by side, so this is a thing a person asks
    /// for rather than something a build claims on startup.
    RegisterUrlScheme,
    /// Stop opening `koru://` links. Offered only when this build is the one
    /// registered: removing somebody else's is not ours to do.
    UnregisterUrlScheme,
    /// Act on the **Voice input** row, whatever it currently says.
    ///
    /// **One ask for four labels**, and the loop re-reads the state rather than
    /// the menu carrying it: the row is built once and a model can arrive or be
    /// removed by another channel's process while it sits there, so an ask that
    /// named the state would act on what the menu said the last time it was
    /// refreshed. The same reason `RegisterUrlScheme`'s handler asks
    /// `url_scheme_state()` again instead of trusting the row it came from.
    Voice,
    /// Open the persistent Koru Voice surface without starting dictation.
    /// This remains distinct from [`Voice`](Self::Voice), whose installed and
    /// enabled meaning is Start dictation.
    VoiceSettings,
    /// **Drop the model and keep dictating.** The weights are several hundred
    /// megabytes and the idle deadline has not fired yet; this frees them now.
    ///
    /// **Separate from [`VoiceStop`](Self::VoiceStop) on purpose.** The two
    /// look alike on a menu and are not alike at all: this one leaves voice
    /// working and makes the next dictation pay the model load again — about
    /// eleven seconds, measured — while stopping ends the process. One ask
    /// covering both would mean the loop deciding which somebody meant, which
    /// is a decision the person already made by choosing a row.
    VoiceRelease,
    /// **End the voice worker.** Nothing is running afterwards and the next
    /// dictation starts one.
    VoiceStop,
    /// **Listen with this microphone.**
    ///
    /// Unlike [`Voice`](Self::Voice), this one *does* carry what was pressed,
    /// and the difference is real: which row somebody chose is a fact about
    /// the gesture and cannot be re-derived afterwards, whereas what the
    /// *Voice* row means depends on a cache that moves under the menu.
    ///
    /// The agent does not apply it. `preferences.conf` has one writer -- the
    /// shell -- so this becomes a `Push::VoiceInput` to a window.
    ChooseVoiceInput(koru_voice::InputChoice),
    /// Flip the relay preference (iroh-relay decision №6): device-local,
    /// applied to this channel's runtime immediately, on/off only — the
    /// relay list itself is never edited from a menu.
    ///
    /// **No boolean is carried, deliberately.** The loop reads
    /// `AgentNetwork::relay_enabled` fresh and flips it — the same rule
    /// that keeps [`RegisterUrlScheme`](Self::RegisterUrlScheme) from
    /// acting on what the row said when the menu was built.
    ToggleRelay,
    /// Look for a newer build now, whatever the automatic schedule says.
    CheckForUpdates,
    /// Write to us.
    ///
    /// **Offered on every build, including one nobody published** — unlike
    /// [`About`](Self::About), whose row is greyed when there is no release page
    /// to open. There is always a mailbox, and a developer running a `cargo`
    /// build has as much to say as anybody.
    SendFeedback,
    Quit,
}

impl AgentEvent {
    /// The variant's name and nothing else — for the stall watch, which must
    /// say what the loop was inside without ever logging what it carried.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hotkey(..) => "Hotkey",
            Self::GlobalDictationChanged(_) => "GlobalDictationChanged",
            Self::SettingsChanged => "SettingsChanged",
            Self::GlobalConsentRefreshed { .. } => "GlobalConsentRefreshed",
            Self::LocalDictation(..) => "LocalDictation",
            Self::AiHotkey => "AiHotkey",
            Self::AiChatProbed { .. } => "AiChatProbed",
            Self::AiChatOpened { .. } => "AiChatOpened",
            Self::HotkeyRegistered { .. } => "HotkeyRegistered",
            Self::DictationHotkeyPrepared { .. } => "DictationHotkeyPrepared",
            Self::DictationHotkeySettled { .. } => "DictationHotkeySettled",
            Self::AiHotkeyRegistered { .. } => "AiHotkeyRegistered",
            Self::NotificationArea { .. } => "NotificationArea",
            Self::Tray(_) => "Tray",
            Self::VoiceWorkerRead { .. } => "VoiceWorkerRead",
            Self::Dictation => "Dictation",
            Self::StoreAvailability { .. } => "StoreAvailability",
            Self::ReminderWorkerChanged { .. } => "ReminderWorkerChanged",
            Self::ReminderOpen { .. } => "ReminderOpen",
            Self::ReminderProfileOpen { .. } => "ReminderProfileOpen",
            Self::ReminderSnooze { .. } => "ReminderSnooze",
            Self::NotificationControlAnswered { .. } => "NotificationControlAnswered",
            Self::PairingChanged => "PairingChanged",
            Self::AccountChanged => "AccountChanged",
            Self::AccountBrowser { .. } => "AccountBrowser",
            Self::AccountResult { .. } => "AccountResult",
            Self::LocalProfileRemoved { .. } => "LocalProfileRemoved",
            Self::LocalProfileAccountBound { .. } => "LocalProfileAccountBound",
            Self::ShellArrived { .. } => "ShellArrived",
            Self::ShellSaid { .. } => "ShellSaid",
            Self::ShellGone { .. } => "ShellGone",
            Self::UpdateChanged => "UpdateChanged",
            Self::UpdateChoiceMade { .. } => "UpdateChoiceMade",
            Self::UpdateOfferHandled => "UpdateOfferHandled",
            Self::SessionShutdownPrepared { .. } => "SessionShutdownPrepared",
            Self::SessionCatalogRefreshed { .. } => "SessionCatalogRefreshed",
            Self::SessionHostStopped { .. } => "SessionHostStopped",
            Self::AgentQuitDecided { .. } => "AgentQuitDecided",
            Self::LinkResolved { .. } => "LinkResolved",
            Self::AgentsCatalog { .. } => "AgentsCatalog",
        }
    }
}

/// How a producer thread reaches the loop.
///
/// Cloneable and `Send`, because there is one per producer and they outlive
/// each other in no particular order.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn(AgentEvent) + Send + Sync>);

impl Waker {
    pub fn new(raise: impl Fn(AgentEvent) + Send + Sync + 'static) -> Self {
        Self(Arc::new(raise))
    }

    /// Raise an event. **Never fails**: a loop that has ended is the ordinary
    /// way this process stops, and a producer that panicked on it would turn a
    /// clean shutdown into a crash report.
    pub fn wake(&self, event: AgentEvent) {
        (self.0)(event);
    }

    /// A waker that collects instead of waking, for testing a producer without
    /// an event loop.
    #[cfg(test)]
    pub fn collecting() -> (Self, Arc<std::sync::Mutex<Vec<AgentEvent>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        (
            Self::new(move |event| sink.lock().unwrap().push(event)),
            seen,
        )
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

/// Where a producer's event goes, before and after there is a loop to take it.
///
/// See the module note for why the "before" half exists at all. **It names no
/// winit**, which is the same property [`Waker`] has and for the same reason:
/// what a producer needs is *raise this*, and the loop is the only thing that
/// should know how one is delivered.
pub struct Mailbox {
    state: Mutex<Delivery>,
}

enum Delivery {
    /// No loop yet. Kept in the order raised.
    Held(Vec<AgentEvent>),
    Live(Box<dyn Fn(AgentEvent) + Send>),
}

impl Mailbox {
    /// **Unbounded while held, deliberately.** What bounds it is process
    /// start-up: the loop is built a few tens of milliseconds later, and a
    /// process whose loop never starts is one that is exiting. A cap would be a
    /// second way to lose an event, which is the failure this type exists to
    /// remove.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Delivery::Held(Vec::new())),
        })
    }

    /// Hand this to a producer. Every one of them holds a [`Waker`] and none of
    /// them knows whether a loop exists yet.
    pub fn waker(self: &Arc<Self>) -> Waker {
        let mailbox = Arc::clone(self);
        Waker::new(move |event| mailbox.raise(event))
    }

    fn raise(&self, event: AgentEvent) {
        // A poisoned mutex means some producer panicked. **Recovered rather than
        // skipped**: dropping the event is precisely the bug this type exists to
        // fix, and one panicked thread is no reason for the next `Hello` to
        // vanish.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *state {
            Delivery::Held(waiting) => waiting.push(event),
            Delivery::Live(deliver) => deliver(event),
        }
    }

    /// A loop exists now; here is how to reach it.
    ///
    /// Whatever was held is delivered **in order, under the lock**, so an event
    /// raised by another thread part-way through the replay queues behind it
    /// rather than overtaking it. Ordering is not cosmetic here: `ShellArrived`
    /// carries the pid that `Hello` is later matched against.
    pub fn attach(&self, deliver: impl Fn(AgentEvent) + Send + 'static) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let held = std::mem::replace(&mut *state, Delivery::Live(Box::new(deliver)));
        let (Delivery::Held(waiting), Delivery::Live(deliver)) = (held, &*state) else {
            // Attached twice. Nothing was being held, so there is nothing to
            // replay and nothing to report.
            return;
        };
        for event in waiting {
            deliver(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ConnectionId;

    fn gone(id: u64) -> AgentEvent {
        AgentEvent::ShellGone {
            connection: ConnectionId(id),
        }
    }

    fn ids(events: &[AgentEvent]) -> Vec<u64> {
        events
            .iter()
            .map(|event| match event {
                AgentEvent::ShellGone { connection } => connection.0,
                other => panic!("not one of ours: {other:?}"),
            })
            .collect()
    }

    /// **The regression.** A producer runs before the loop exists — the
    /// endpoint is answered two steps earlier — and what it raised has to
    /// survive until there is somewhere to put it.
    #[test]
    fn what_is_raised_before_a_loop_exists_is_delivered_when_one_arrives() {
        let mailbox = Mailbox::new();
        let waker = mailbox.waker();
        waker.wake(gone(1));
        waker.wake(gone(2));

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        mailbox.attach(move |event| sink.lock().unwrap().push(event));

        assert_eq!(
            ids(&seen.lock().unwrap()),
            [1, 2],
            "an event raised before the loop was lost"
        );
    }

    /// And in the order it was raised. `ShellArrived` carries the pid that a
    /// later `Hello` is matched against, so a replay that reordered them would
    /// hand the foreground capability to nobody.
    #[test]
    fn a_replay_keeps_the_order_things_were_raised_in() {
        let mailbox = Mailbox::new();
        let waker = mailbox.waker();
        for id in 1..=5 {
            waker.wake(gone(id));
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        mailbox.attach(move |event| sink.lock().unwrap().push(event));

        assert_eq!(ids(&seen.lock().unwrap()), [1, 2, 3, 4, 5]);
    }

    /// Once a loop is attached, nothing is held: an event goes straight through.
    #[test]
    fn once_a_loop_is_attached_an_event_goes_straight_to_it() {
        let mailbox = Mailbox::new();
        let waker = mailbox.waker();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        mailbox.attach(move |event| sink.lock().unwrap().push(event));

        waker.wake(gone(7));
        assert_eq!(ids(&seen.lock().unwrap()), [7]);
    }
}
