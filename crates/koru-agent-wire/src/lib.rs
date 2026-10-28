// SPDX-License-Identifier: MPL-2.0

//! **Every message the Koru Agent's IPC carries.**
//!
//! ```text
//! shell -> ShellHello  { product, version, protocol_version }
//! agent -> AgentGreeting::Welcome { agent_version, device_id, instance_id, … }
//!        | AgentGreeting::Refused { what, ours, theirs, message }  -> shell closes
//! ...then, both directions, until the socket closes:
//! shell -> FromShell::Request { id, body }
//! agent -> FromAgent::Response { id, body }        — answering a request
//!        | FromAgent::Push(..)                     — unasked for
//! ```
//!
//! ## Two versions live at two layers, and they are not the same number
//!
//! [`PROTOCOL_VERSION`] is **what these messages mean**. The *frame* they
//! travel in — `KORU`, a version byte, a length, a JSON body — is
//! `koru_ipc::frame`, which takes its version as a parameter and holds no
//! constant of its own. That is what lets the store's rendezvous and this
//! stream version independently over identical bytes.
//!
//! The difference matters at exactly one moment, and it is easy to collapse:
//!
//! - an unknown **frame** version means *"this is not something I can read at
//!   all"*, and the only honest answer is to close the connection. A refusal is
//!   itself a frame, and writing one in a format the peer has just said it does
//!   not share is a promise that cannot be kept;
//! - an unknown **protocol** version means *"I can read you, and I do not speak
//!   this"*, which is a sentence — [`AgentGreeting::Refused`], naming both
//!   numbers, because "incompatible" alone sends somebody to read two
//!   changelogs to find out which end is behind.
//!
//! So a reader takes the body as **bytes** first ([`koru_ipc::frame::read_raw`]),
//! decides about the version, and only then gives it a shape. A stream whose
//! semantics have not been agreed must never have its bodies parsed as though
//! they had.
//!
//! ## Ids are strings here
//!
//! Deliberately, and it is what keeps this crate a leaf. Every id is a uuid in
//! its storage spelling; the agent parses one to route on it and a shell parses
//! one to open it, and **each refuses what it cannot parse** rather than
//! passing it on. Typing them would mean depending on `koru-store-core`, which
//! would put a CRDT engine in the graph of anything that wants to send a
//! message — the shape [ADR-105](../../../docs/decisions/ADR-101-125.md#adr-105-ipc--mechanism-is-pushed-down-and-domain-behaviour-stays-with-its-domain)
//! exists to prevent.
//!
//! ## Every request carries a correlation id
//!
//! Because the agent pushes messages nobody asked for. On a stream where
//! `Open(..)` can arrive between a request and its reply, matching replies by
//! arrival order is wrong the first time somebody presses the hotkey during a
//! workspace switch.

use serde::{Deserialize, Serialize};

/// What these messages mean. Bump when a shape changes in a way an older peer
/// cannot read.
///
/// **The only copy.** `koru_ipc::frame` deliberately owns no version constant,
/// so this number and the store's `HANDSHAKE_VERSION` cannot be confused for
/// one another.
///
/// 3 added the update conversation — [`Push::Update`] and
/// [`Request::UpdateChoice`]. A new variant is exactly the shape this constant
/// exists for: these enums are internally tagged, so an older peer meeting an
/// unknown `kind` fails the whole body rather than skipping a field, and the
/// handshake is where that has to be caught.
///
/// 4 added [`Push::Updated`], the one-shot that follows an install.
/// 7 added the device-linking conversation: accepting a link code, listing
/// linked devices, and unlinking one.
/// 8 added [`Request::OpenLink`], which follows a `koru://` URL.
/// 9 made the linked-device list a mesh: [`LinkedDeviceView::introduced_by`]
/// names the directly paired device a transitive one arrived through.
/// 10 added [`Request::RenameDevice`] and the `this_device` row on
/// [`Response::LinkedDevices`].
/// 11 added [`Push::Shortcut`], the chord the agent actually registered, so a
/// shell can say it on a surface the agent does not draw.
/// 12 added [`LinkedDeviceView::build_label`], [`LinkedDeviceView::build_drift`],
/// [`LinkedDeviceView::build_severity`], [`LinkedDeviceView::update_needed_on`]
/// and [`LinkedDeviceView::connection`]: what each device runs, how far from
/// this one, and whether it is reached directly or through a relay.
/// 13 added per-device state-change history and the last successful state
/// change timestamp to [`LinkedDeviceView`].
/// 14 moves that history to paged on-demand responses and adds document
/// display names, operation counts and reconciliation detail.
/// 15 adds terminal-window presence and the distinct Terminal focus-or-launch
/// request (removed in 24). Terminal bytes and snapshots remain on
/// `koru-session-wire`.
/// 15 also adds [`Request::OpenChatLink`] so the Agent can route AI deep links
/// without pretending a chat is a Store document.
/// 16 adds [`Request::OpenAiChat`] so a workspace caption can ask the Agent to
/// launch the standalone host without manufacturing a chat identity.
/// 17 projects the live session list into the Agent menu and lets it ask a
/// Terminal window to select one. Terminal traffic still never crosses here.
/// Version 17 also lets an update-reopened workspace host return the durable
/// launch claim on
/// [`Request::Hello`]. Agent spends reopen intent only for that exact claim;
/// an ordinary or late handshake can never acknowledge a different update.
/// 18 adds [`Service::Voice`], so a workspace host can ask the Agent to make sure the
/// voice worker is running. A new *variant* is exactly what this number exists
/// for: these enums are internally tagged, so an older Agent meeting
/// `"service":"voice"` fails the whole body rather than skipping a field, and
/// the handshake is where that has to be caught rather than at the moment
/// somebody holds a key down.
/// 19 adds [`Push::Voice`], the notification area asking a window to open the
/// voice consent step. The row that sends it was drawn greyed until Plan 2.5
/// existed to answer it (Q-V67).
/// 20 adds [`Push::VoiceInput`], the notification area asking a window to
/// change which microphone dictation listens with. The agent may not write
/// `preferences.conf` — `koru_window::user_preferences` says the shell is its
/// only writer — so the choice is routed rather than applied.
/// On oauth, 21 adds the Agent-owned account state machine. Account requests are valid
/// before workspace `Hello`, so a zero-Profile shell never fabricates ids.
/// 22 identifies each browser handoff attempt independently, preserving the
/// two-stage collision flow while making duplicate pushes idempotent.
/// 23 adds Profile-scoped Auth Identity authorization removal and the
/// device-local empty-Profile removal command with its correlated result.
/// 24 stages Add Google for explicit confirm/cancel, carries canonical Koru
/// Identity ids and the exact signed-in identity only in targeted results, and
/// adds exact local binding CAS.
/// 25 separates automatic enrollment failure and retry state from browser
/// account state and permits cancellation of unfinished enrollment.
/// 26 adds the first-party grant pair, [`Request::RequestFirstPartyGrant`] and
/// [`Request::ValidateFirstPartyGrant`].
/// 27 adds [`Request::OpenWorkspace`], so rooted development launches are
/// Agent-vouched before requesting workspace lifecycle scopes.
/// 28 adds [`AccountCommand::ListProfileChoices`], keeping authorization
/// distinct from the places a Profile's content can currently be found.
/// 29 adds [`Push::ShowSupport`] beside the feedback routing merged earlier.
/// 30 adds durable Profile lifecycle deletion, reconciliation, and cleanup
/// acknowledgements. Lifecycle results contain identifiers and stable state,
/// never Profile names or account PII.
/// 31 adds [`Push::ProfileLifecycleDeleted`] for trusted-peer deletion.
/// 32 adds [`AccountCommand::RestoreProfile`] and its targeted result.
///
/// On reminder, 21 adds [`Request::ReminderActivation`], a bounded content-free envelope
/// forwarded from native process activation to the singleton Agent, and the
/// correlated typed [`Request::NotificationControl`] / [`Response::NotificationControl`]
/// Settings seam. Neither path exposes native adapter types to a shell.
/// 22 adds the acknowledged [`Push::ReminderSnooze`] one-shot. Agent retains
/// it across a cold shell launch or disconnect until the receiving shell has
/// queued it and answers [`Request::AcceptReminderSnooze`].
/// 23 adds content-free reminder scheduler diagnostics to [`Response::Status`].
/// 24 makes those diagnostics directly queryable by Settings and adds the
/// explicit current-device delivery, repair, and authorization controls.
/// 25 adds the content-free delivered/unhandled identities consumed by the
/// reminder attention surface. Authored content and targets remain absent.
/// 26 adds the occurrence's content-free delivery instant so the shell can
/// present when attention fired without deriving it from authored schedule.
/// 27 adds the first-party grant pair, [`Request::RequestFirstPartyGrant`] and
/// [`Request::ValidateFirstPartyGrant`]. The Agent is the only process that
/// launches Koru's own windows, so it is the only one that can say a caller is
/// one; the Gateway asks it rather than believing a self-declared `product`
/// string. The grant carries no roles — a token and an expiry, nothing else —
/// because the scope system stays the authorization vocabulary and a second
/// one would be a second place to look.
/// 22 adds the internal, content-free global-hotkey handoff between Agent and
/// a focused Workspace: [`Push::DictationGesture`] and
/// [`Request::DictationState`]. No transcript or target identity crosses this
/// boundary; the route token only correlates lifecycle acknowledgements.
/// 23 adds [`Push::ShowFeedback`], the notification area asking a window to
/// open Send feedback in a standalone pane.
/// 23 adds [`Push::ShowSupport`], the same ask for Contact support.
/// 24 adds [`Push::ShowSession`], the notification area asking a Koru
/// Workspace window to open the tab for a running terminal session — or, with
/// no session named, to offer starting a new one (sessions live as ordinary
/// Workspace tabs now, Q-E8 P9, so the tray's Agents rows route to koru.exe);
/// and it removes the Terminal presence family — `Request::TerminalHello`,
/// `Request::TerminalSessions`, `Request::ShowTerminal`,
/// `Push::SelectTerminalSession` and `TerminalSessionMenuRow` — because
/// koru-terminal.exe itself is deleted (TERMINATAB W5) and nothing produces
/// them any more. A *removed* variant is exactly the shape this constant
/// exists for, in the other direction: an older Terminal window sending
/// `terminal_hello` to a 24 Agent would fail the whole body, and the handshake
/// is where that has to be caught rather than mid-conversation.
/// 25 combines the session/support schema with global Dictation and Feedback.
/// Neither pre-merge peer can read the complete union.
/// **The reminder block's numbers 21 to 27 are branch-local.** dev spent the same
/// numbers on other shapes while that branch was open — 21 the first-party
/// grant pair, 22 [`Push::ShowFeedback`], 23 [`Push::ShowSupport`], 24
/// [`Push::ShowSession`] plus the removal of the Terminal presence family —
/// and dev's 23 is what the nightlies shipped. The handshake compares for
/// equality, so no number ever meant two things to one running pair, but a
/// reader of this log should not expect a nightly's 23 to carry diagnostics.
/// 28 combines the reminder contract through 27 with dev's feedback, support,
/// and session-tab routing. It adds ShowFeedback, ShowSupport and ShowSession,
/// and removes the retired Terminal presence/request family. Neither a reminder
/// 27 peer nor a dev 24 peer can read this combined contract; reject both at the
/// handshake rather than discovering the mismatch during an action.
/// 29 combines dev's 28 (reminders through 27 plus feedback, support and
/// session-tab routing) with the global-Voice branch's 25 (the Dictation
/// gesture/state handoff above). Neither a 25 peer nor a 28 peer can read
/// the union; the handshake rejects both.
/// 33 combines oauth 32 account/onboarding/lifecycle and Agent-vouched launch
/// with dev 28 reminder diagnostics, notification control and integrated session
/// routing. The standalone Terminal request/presence family stays retired.
/// Both original branch contracts are rejected; frame encoding is unchanged.
/// 34 adds explicit account creation confirmation/cancellation and its pause.
/// 35 adds held account selection and immutable operation-owner scopes on
/// account commands, admissions, browser requests and results.
/// 36 replaces singleton enrollment selection with an account-owned Profile
/// set, per-Profile progress and targeted enrollment retry.
/// 37 combines OAuth account ownership/enrollment (36) with dev global
/// dictation gestures/state (34). Reject either older peer; frames stay at 1.
/// 38 adds targeted, account-owned Cloud subscription inspection.
/// 39 exposes this device's live relay preference and connection in the
/// linked-device snapshot, and lets a shell set that preference explicitly.
/// 40 adds per-Profile Cloud sync: a shell turns one Profile's Workspaces on
/// or off for Koru Cloud and reads every Profile's Cloud sync state.
/// 41 adds `ResetIdentity`: a device whose keys no longer match its identity
/// sets the old identity aside and starts over as a new device.
/// 42 adds [`Push::Quitting`]: the Agent is stopping because a person asked it
/// to, so a window must not start another one when the link ends. A 41 window
/// would restart the Agent its person had just quit; the handshake refuses it.
/// 43 adds [`Push::NotificationArea`], whether the Agent has an icon a hidden
/// window can be brought back from. A window used to assume it from the
/// platform it was built for, which was wrong on Linux (where the icon now
/// exists, when a host does) and wherever the icon failed to appear.
pub const PROTOCOL_VERSION: u32 = 43;

/// The frame version the agent's stream stamps, handed to `koru_ipc::frame`.
///
/// Separate from [`PROTOCOL_VERSION`] on purpose — see the module note. It
/// changes only if the *framing* changes, which is a thing that has never
/// happened and would break every local endpoint at once.
pub const FRAME_VERSION: u8 = 1;

/// The private standalone AI-chat control stream's frame/semantic version.
/// It is deliberately separate from [`PROTOCOL_VERSION`]: the Agent-to-shell
/// conversation and the singleton window's lifecycle can evolve independently.
pub const AI_CHAT_PROTOCOL_VERSION: u8 = 1;

/// A private machine-local request to the standalone AI chat host.
///
/// Chat and block ids remain strings so this leaf does not acquire the AI
/// model. The receiving host validates them before navigating.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiChatRequest {
    Activate,
    OpenChat { chat: String, block: Option<String> },
    Status,
    Exit,
}

/// The standalone host's answer. `pid` is trustworthy to the Agent because it
/// came from the local endpoint owner, rather than from an Agent-shell peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiChatResponse {
    Accepted { pid: u32, focused: bool },
    Failed { code: String, message: String },
}

// ------------------------------------------------------------------ handshake

/// What a shell says first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellHello {
    /// Which program is calling — `koru`, a test harness, a third party. A
    /// label for the agent's logs, never a person and never a device.
    pub product: String,
    pub version: String,
    pub protocol_version: u32,
}

/// What the agent says back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentGreeting {
    Welcome(Box<AgentWelcome>),
    /// Named numbers, both sides.
    Refused {
        /// A stable ASCII token: `protocol` today. Not a sentence.
        what: String,
        /// What the agent speaks.
        ours: u32,
        /// What the shell said it speaks.
        theirs: u32,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWelcome {
    pub agent_version: String,
    pub protocol_version: u32,
    /// This device's id, from `device.json`. **Not an account id** and not
    /// derived from anything — a v4 uuid minted once, per ADR-39.
    pub device_id: String,
    /// New on every agent start, so a shell holding a stale connection can tell
    /// without dialling it. The same reason `StoreHello` carries one.
    pub instance_id: String,
    pub sync: SyncStatus,
}

/// Privacy-safe sync diagnostics. No grants, workspace names or key material.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub endpoint_id: Option<String>,
    pub sync_ready: bool,
    /// A stable internal token such as `identity_unavailable`.
    pub sync_error: Option<String>,
    pub trusted_device_count: usize,
    pub authorization_generation: u64,
}

/// Content-free current scheduler facts suitable for support output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderDiagnostics {
    /// The choice made on this device. Permission and connectivity never
    /// rewrite it.
    pub enabled: bool,
    pub pending: usize,
    pub registered: usize,
    pub failed: usize,
    pub cursor: Option<u64>,
    pub next_deadline_ms: Option<i64>,
    pub last_successful_sync_ms: Option<i64>,
    pub failure_category: Option<String>,
    #[serde(default)]
    pub attention: Vec<ReminderAttention>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderAttention {
    pub profile: String,
    pub reminder: String,
    pub occurrence: String,
    pub fired_at_ms: i64,
}

/// A pinned document a shell is showing, so Search can list it on the landing page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchPin {
    pub title: String,
    pub document: String,
    pub workspace: String,
    /// Whether this pin shows its title in the tab strip. Search matches that.
    #[serde(default)]
    pub show_title: bool,
}

// ------------------------------------------------------------ shell -> agent

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FromShell {
    Request { id: u64, body: Request },
}

/// The only physical keys used by Agent's global Dictation candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationKey {
    D,
    M,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationMode {
    PushToTalk,
    Toggle,
}

/// One content-free step of a routed global gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DictationGesture {
    Press { key: DictationKey },
    Release { key: DictationKey },
    Cancel { key: DictationKey },
}

/// The receiving local controller's correlated lifecycle answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DictationState {
    Refused,
    Listening { mode: DictationMode },
    Finalising,
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Ask the Agent's bounded account worker. Valid before [`Request::Hello`].
    Account {
        command: AccountCommand,
        expected_owner_scope: Option<u64>,
    },
    /// Mark one empty Profile removed in this installation's Store catalog.
    /// The Store keeps its row, CRDT, and deleted Workspace tombstones for
    /// explicit recovery; the caller still owns Profile-keyed settings cleanup.
    RemoveLocalProfile {
        profile_id: String,
    },
    /// Atomically bind this local Profile to an exact Koru identity. The
    /// expected identity makes erased rebinds explicit; a mismatch never writes.
    BindLocalProfileAccount {
        profile_id: String,
        expected_identity_id: Option<String>,
        new_identity_id: String,
    },
    /// Make sure a service this device depends on is running.
    ///
    /// **The agent supervises rather than proxies.** The answer is only that it
    /// is up; the shell then connects to it itself and mints its own session
    /// credential, which is per connection and could not have been handed over.
    EnsureService {
        service: Service,
    },
    /// I am a shell, and this is what I am showing.
    ///
    /// **No pid.** The agent reads that off the transport, because a pid a peer
    /// reports is a claim — and a wrong one hands `AllowSetForegroundWindow` to
    /// a process nobody chose.
    Hello {
        profile: String,
        workspace: String,
        profile_name: String,
        workspace_name: String,
        #[serde(default)]
        pinned: Vec<SearchPin>,
        /// The opaque v4 claim Agent persisted before launching this window.
        /// Absent on every ordinary launch and retained across workspace
        /// changes only so a reconnect after an Agent crash can finish the
        /// same durable acknowledgement.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reopen_claim: Option<String>,
    },
    /// I am showing something else now. Updates the registry entry this
    /// connection already has; never adds a second.
    WorkspaceChanged {
        profile: String,
        workspace: String,
        profile_name: String,
        workspace_name: String,
        #[serde(default)]
        pinned: Vec<SearchPin>,
    },
    /// I was just brought to the front.
    ///
    /// **Carries no time.** The agent stamps the order it observed, from one
    /// monotonic counter — several machines' clocks drift, a laptop resumes
    /// with the wrong one, and a shell paused in a debugger reports a time from
    /// before it was paused.
    Active,
    /// Answer a routed global dictation gesture. Accepted only from the
    /// transport-authenticated Workspace connection that owns `route`.
    DictationState {
        route: u64,
        state: DictationState,
    },
    /// Open the global search overlay, as though the hotkey had been pressed.
    ShowOverlay,
    /// Put a Koru workspace window in front, launching the sibling executable
    /// when none is connected.
    ///
    /// Unlike a caller spawning `koru.exe` itself, this path records the child
    /// as an Agent launch. The window can therefore prove it is first-party
    /// when it asks the Gateway for the separately scoped workspace lifecycle
    /// permissions.
    OpenWorkspace {
        /// Optional profile selector passed through exactly as Koru's own
        /// `--profile` value. Agent does not interpret a name, id, or path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// Development runner only: keep stdout/stderr on Agent's console so
        /// Koru's diagnostic counters remain visible under `run-all`.
        #[serde(default)]
        inherit_stdio: bool,
    },
    /// Route this result to whichever window should show it — which may be the
    /// one asking, another, or one that has to be started first.
    ///
    /// **Not "open this here".** A hit found by a shell's own in-app search can
    /// name a workspace that shell does not have open, and deciding which
    /// window it belongs in is the agent's job: only it knows what every window
    /// is showing and which was focused last. A shell that routed for itself
    /// would open a second window onto a workspace already on screen.
    ///
    /// Answered `Ok` once the decision is made, which is **not** the same as
    /// delivered — a target for a workspace nothing is showing is held until a
    /// window says `Hello` for it. Delivery arrives as [`Push::Open`], on
    /// whichever connection won.
    Route {
        target: Target,
    },
    /// Follow a `koru://` link: a stable document id, and optionally one block.
    ///
    /// **Not [`Route`], and the difference is load-bearing.** A `Target` names
    /// a profile and a workspace, and a caller holding a URL has neither — a
    /// link deliberately says nothing about where a document currently lives
    /// (ADR-127). Guessing them client-side is precisely the mistake `Route`'s
    /// note warns about, so the agent resolves the location through the Store
    /// catalog and then routes it like any other target.
    ///
    /// Ids travel as strings for the same reason every other id here does:
    /// nothing on this wire links the node model.
    ///
    /// Answered `Ok` once accepted for resolution, which is **not** delivered.
    OpenLink {
        document: String,
        node: Option<String>,
    },
    /// Follow a canonical AI chat link. Chat and block identities remain
    /// strings on this process boundary so the Agent does not link AI models.
    OpenChatLink {
        chat: String,
        block: Option<String>,
    },
    /// Open the standalone AI chat at its ordinary landing state.
    ///
    /// Kept distinct from [`OpenChatLink`](Self::OpenChatLink): opening the app
    /// must not mint or guess a durable chat merely to satisfy a process-launch
    /// request.
    OpenAiChat,
    /// The person answered the update banner. See [`UpdateChoice`].
    ///
    /// **The consent is shown here and acted on there.** The agent owns
    /// updating this installation — it is the process that survives a restart
    /// and the one that can drive the swap helper — but a person is looking at
    /// a window, not at a tray icon, so the two consent steps are drawn by a
    /// shell and arrive as this.
    UpdateChoice {
        choice: UpdateChoice,
    },
    /// What is this agent, and what is it holding? Answered with
    /// [`Response::Status`].
    ///
    /// **A question, not a subscription.** Anything that wanted to watch would
    /// need pushes, and there is nothing here worth waking a process for.
    Status,
    StartPairing,
    AcceptLinkCode {
        code: LinkCode,
    },
    PairingStatus,
    CancelPairing,
    LinkedDevices,
    DeviceHistory {
        device_id: String,
        before: Option<i64>,
    },
    UnlinkDevice {
        device_id: String,
    },
    /// Rename *this* device — never a peer's. The new name is a self-signed
    /// record that gossips to the mesh like any other trust record, so every
    /// linked device picks it up; it is also what the next pairing offers.
    /// Answered with the refreshed [`Response::LinkedDevices`].
    RenameDevice {
        name: String,
    },
    /// Set this device's relay preference. The Agent applies it to the live
    /// network and persists it for the next launch, then answers with the same
    /// whole linked-device snapshot as [`Request::LinkedDevices`].
    SetRelay {
        enabled: bool,
    },
    /// An opaque notification gesture. The native boundary constrains its
    /// decoded shape to schema + registration id + action; authored content or
    /// a Store navigation target must never be added to this transport value.
    ReminderActivation {
        envelope: String,
    },
    /// One explicit notification-settings operation. Test copy is localized
    /// by the requesting UI, bounded by Agent, and never persisted.
    NotificationControl {
        operation: NotificationOperation,
    },
    /// Content-free reminder scheduler facts for this device. This is a
    /// question rather than a subscription: Settings refreshes after an
    /// explicit action or when it opens, never from a polling timer.
    ReminderStatus,
    /// The shell has placed one routed snooze-management ask in its event-loop
    /// queue. Until this acknowledgement arrives Agent retains the ask, so a
    /// shell that disconnects during a cold launch cannot consume it silently.
    AcceptReminderSnooze {
        delivery: String,
    },
    /// **Ask for proof that this caller is one of Koru's own windows.**
    ///
    /// The Agent is the process that launches them, so it is the only one that
    /// can say so; it answers this for a connection whose transport-read pid is
    /// one it started, and refuses everyone else. What comes back is a
    /// [`FirstPartyGrant`] to present to the Gateway at connect time.
    ///
    /// It carries nothing. The Agent must not be told who is asking — that is
    /// the whole point — so there is no field a caller could get wrong or lie
    /// about.
    RequestFirstPartyGrant,
    /// **Is this grant one you minted, and is it still good?**
    ///
    /// The Gateway's half. A grant is spent by being validated: a token that
    /// answers `valid` once answers `false` afterwards, so a grant that leaks
    /// after admission buys nothing. The answer names no caller and no scope —
    /// the Gateway already knows which scopes were asked for, and the scope
    /// system stays the authorization vocabulary.
    ValidateFirstPartyGrant {
        grant: FirstPartyGrant,
    },
    /// Start over as a new device: set this device's identity aside, keep its
    /// content, mint a new Device ID from the keys it still holds (or new keys
    /// if it holds none), and stop. **Only when the identity is broken** — a
    /// device whose keys match its identity is refused with `identity_intact`,
    /// because a working identity is linked devices' trust and must not be
    /// discarded by a click. Success has no reply, as with [`Self::Shutdown`]:
    /// the Agent stops, and the shell relaunches onto the new identity. A
    /// refusal answers [`Response::Failed`].
    ResetIdentity,
    /// Stop. There is no reply, because the socket closing *is* the reply — an
    /// agent that acknowledged a shutdown and then had to still be running to
    /// send it would be answering a question about a moment that had passed.
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotificationOperation {
    Inspect,
    /// The only operation allowed to prompt for notification permission.
    RequestAuthorization,
    OpenSettings,
    /// Change this Agent's local delivery choice. It never changes a profile
    /// reminder rule and cannot address another device.
    SetDeliveryEnabled {
        enabled: bool,
    },
    /// Re-read authority and repair this device's native registrations.
    Repair,
    SendTest {
        tone: NotificationTone,
        title: String,
        body: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationTone {
    System,
    KoruDefault,
    Soft,
    Chime,
    Pulse,
    Bright,
    Calm,
    Drop,
    Knock,
    Signal,
    Urgent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAuthorization {
    NotDetermined,
    Allowed,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotificationControlResult {
    Capability {
        authorization: NotificationAuthorization,
        actions: bool,
        stable_replacement: bool,
        withdrawal: bool,
        custom_sound: bool,
        activation_while_stopped: bool,
    },
    Authorization {
        authorization: NotificationAuthorization,
    },
    Done,
    Failed {
        code: NotificationFailure,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationFailure {
    Unsupported,
    PermissionDenied,
    AssetMissing,
    InvalidRequest,
    Platform,
    Busy,
    Timeout,
    Unavailable,
}

/// A short-lived proof that the Agent launched the process holding it.
///
/// **Opaque token and an expiry, and deliberately nothing else.** No caller
/// name, no product, no roles: a grant that carried permissions would be a
/// second authorization vocabulary beside the scopes, and two vocabularies is
/// two places to look when something is allowed that should not be. What this
/// says is "the supervisor launched whoever asked for this", and the Gateway
/// decides what that is worth.
///
/// The token is redacted from `Debug` for the reason [`LinkCode`] is: request
/// tracing and test failures routinely format whole wire messages.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstPartyGrant {
    token: String,
    pub expires_at_millis: i64,
}

impl FirstPartyGrant {
    pub fn new(token: String, expires_at_millis: i64) -> Self {
        Self {
            token,
            expires_at_millis,
        }
    }

    pub fn expose(&self) -> &str {
        &self.token
    }
}

impl std::fmt::Debug for FirstPartyGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirstPartyGrant")
            .field("token", &"[REDACTED]")
            .field("expires_at_millis", &self.expires_at_millis)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    ApiGateway,
    /// Compatibility spelling retained for older clients.
    /// Agent ensures the gateway and never starts Store directly.
    Store,
    /// The voice worker.
    ///
    /// **Lazy by design.** The Agent does not start one at launch; it starts
    /// one on the first `EnsureService { service: Voice }`, which arrives when
    /// a person first dictates. An installation with voice switched off pays
    /// nothing — no process, no model load, no device.
    ///
    /// **A `--root` Agent does not refuse it**, unlike the machine-wide
    /// shortcut, the Run-key entry and `koru://` registration. Those reach
    /// outside the installation; a rooted worker is what makes voice testable
    /// in isolation, and its endpoint is qualified by the root so it cannot be
    /// mistaken for the one somebody is dictating through.
    Voice,
}

// ------------------------------------------------------------ agent -> shell

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FromAgent {
    /// Answering a request. `id` echoes the one it answers.
    Response { id: u64, body: Response },
    /// Unasked for — which is why a response needs an id at all.
    Push { body: Push },
}

/// Stable outcome of a device-local Profile removal request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProfileRemovalOutcome {
    Removed,
    AlreadyRemoved,
}

/// Stable outcome of a durable local Profile account compare-and-set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProfileAccountBindingOutcome {
    Bound,
    AlreadyBound,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok,
    /// Result of [`Request::OpenWorkspace`]. `None` means an existing window
    /// was restored; a pid means Agent spawned and recorded the child.
    WorkspaceOpened {
        pid: Option<u32>,
    },
    Account {
        state: AccountState,
    },
    /// The bounded worker accepted this operation. Completion is delivered as
    /// a requester-targeted [`Push::AccountResult`] with the same generation.
    AccountQueued {
        owner_scope: u64,
        generation: u64,
    },
    /// Correlated answer that the Store catalog marker is durable. This is not
    /// acknowledgement that workspace-host Profile-keyed settings are cleared.
    LocalProfileRemoved {
        profile_id: String,
        outcome: LocalProfileRemovalOutcome,
    },
    /// Correlated answer that the exact identity binding is durable.
    LocalProfileAccountBound {
        profile_id: String,
        outcome: LocalProfileAccountBindingOutcome,
    },
    NotificationControl {
        result: NotificationControlResult,
    },
    ReminderStatus {
        reminders: ReminderDiagnostics,
    },
    /// What a running agent is and what it is holding. Counts rather than
    /// contents: **a shell's profile and workspace are a person's, and a status
    /// line printed into a terminal or pasted into a bug report is not the
    /// place for them.** How many there are answers the question somebody
    /// actually has, which is whether their windows are registered.
    Status {
        agent_version: String,
        /// This device's id. Not an account id — [ADR-39](../../../docs/decisions/README.md).
        device_id: String,
        /// New on every agent start, so two `status` calls can be told apart
        /// across a restart.
        instance_id: String,
        /// Connected shells that completed the handshake.
        shells: usize,
        /// Targets held for a workspace that has not appeared yet.
        pending_launches: usize,
        /// Whether this launch participates in machine-wide global input.
        /// Ordinary Agents and interactive rooted `--tray` Agents report true;
        /// headless roots report false.
        owns_the_machine: bool,
        sync: SyncStatus,
        reminders: ReminderDiagnostics,
    },
    Pairing {
        view: Box<PairingView>,
    },
    LinkedDevices {
        devices: Vec<LinkedDeviceView>,
        /// This device itself, so the list can lead with it and offer
        /// rename. `None` only when sync identity is unavailable.
        #[serde(default)]
        this_device: Option<Box<LinkedDeviceView>>,
        /// This device's relay switch and live connection. `None` only when
        /// the Agent's device network is unavailable.
        #[serde(default)]
        relay: Option<Box<RelayView>>,
    },
    DeviceHistory {
        device_id: String,
        events: Vec<DeviceStateEventView>,
        next_before: Option<i64>,
    },
    /// A freshly minted grant, for a caller the Agent launched.
    FirstPartyGrant {
        grant: FirstPartyGrant,
    },
    /// Whether a presented grant was one this Agent minted and had not already
    /// spent. `false` is the whole answer: a grant that is unknown, expired or
    /// already used is refused, and saying which of the three would tell a
    /// guessing caller which guesses are getting warmer.
    FirstPartyGrantValidity {
        valid: bool,
    },
    /// A stable ASCII code and a message that is developer text, never
    /// something to show a person. The rule the store's API already follows.
    Failed {
        code: String,
        message: String,
    },
}

/// A short-lived device-linking secret. It serializes as the underlying text
/// but never reveals it through `Debug`, because both request tracing and test
/// failures routinely format whole wire messages.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LinkCode(pub String);

impl std::fmt::Debug for LinkCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkCode([REDACTED])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedDeviceView {
    pub device_id: String,
    pub display_name: String,
    pub linked_at_millis: i64,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub last_seen_millis: Option<i64>,
    /// The display name of the directly paired device this one arrived
    /// through, when there is no local pairing with it — "via B". `None` for
    /// a directly linked device. A transitive device cannot be unlinked here;
    /// it disappears when the introducing relationship is revoked.
    #[serde(default)]
    pub introduced_by: Option<String>,
    /// What the device says it runs — `version · channel · branch · OS`, as
    /// its latest self-signed build record puts it. `None` for a device that
    /// has never said (a build older than this field).
    #[serde(default)]
    pub build_label: Option<String>,
    /// How far the device's build is from this one's, in semver's meaning of
    /// the three numbers: `"same"`, `"patch"`, `"minor"`, `"major"`,
    /// `"incompatible"` (different peer protocol numbers — the two do not
    /// sync) or `"unknown"`. `None` when either side has not said. The agent
    /// decides, since it holds both descriptions.
    #[serde(default)]
    pub build_drift: Option<String>,
    /// How loudly to show it: `"warning"` (a minor apart), `"danger"` (a
    /// major apart, or incompatible), `None` for nothing to mark.
    #[serde(default)]
    pub build_severity: Option<String>,
    /// Set only when incompatible: which side has to update — `"peer"` (the
    /// device on the lower protocol number is that one) or `"this_device"`.
    #[serde(default)]
    pub update_needed_on: Option<String>,
    /// How the live connection reaches the device: `"direct"` or `"relay"`
    /// (through an iroh relay server). `None` while offline, and for a
    /// connection whose path is not yet known.
    #[serde(default)]
    pub connection: Option<String>,
    /// Last retained remote catalog/profile/workspace/document/blob change
    /// accepted from this device.
    /// Kept empty in device-list responses; history travels through the paged
    /// `DeviceHistory` response.
    #[serde(default)]
    pub last_sync_millis: Option<i64>,
    #[serde(default)]
    pub history: Vec<DeviceStateEventView>,
}

/// This device's relay preference and observed connection state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayView {
    pub enabled: bool,
    /// One of `off`, `connecting`, `connected`, or `disconnected`.
    pub state: String,
    /// The active/configured relay host. Absent while relays are off.
    pub host: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStateEventView {
    pub event_id: Option<i64>,
    pub at_millis: i64,
    pub kind: String,
    pub profile_id: Option<String>,
    pub workspace_id: Option<String>,
    pub document_id: Option<String>,
    pub document_name: Option<String>,
    pub blob_hash: Option<String>,
    pub detail: Option<String>,
    pub items: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Push {
    /// Complete privacy-safe state, sent on each material account transition.
    Account {
        state: AccountState,
    },
    /// A browser handoff. The shell opens it on its browser worker; Agent never
    /// invokes a platform browser and never logs this value.
    AccountBrowser {
        owner_scope: u64,
        generation: u64,
        /// Monotonic process-local identity for one browser handoff. Collision
        /// reauthentication has two attempts in one account generation.
        attempt: u64,
        url: BrowserUrl,
    },
    /// Completion for one account operation, delivered only to its requester.
    AccountResult {
        owner_scope: u64,
        generation: u64,
        result: AccountOperationResult,
    },
    /// Content-free completion nudge for consumers that do not retain state.
    AccountChanged,
    /// One exact Profile became hosted-deleted through a trusted peer. The
    /// Profile id is not PII and is the minimum routing fact a shell needs to
    /// stop presenting or opening its local content and resume recoverable
    /// cleanup. Names and account metadata never travel in this broadcast.
    ProfileLifecycleDeleted {
        profile_id: String,
    },
    /// Show this result. See [`Target`].
    Open {
        target: Target,
    },
    /// Open the validated reminder entity and begin its semantic snooze flow.
    ///
    /// The delivery token is device-local mechanism. The remaining fields are
    /// stable identifiers resolved by Agent from its content-free ledger and
    /// revalidated by the shell; no title or authored content crosses here.
    ReminderSnooze {
        intent: ReminderSnoozeIntent,
    },
    /// Whole, content-free scheduler state for reminder chrome. Stored by the
    /// shell as a cache; it is not a one-shot instruction.
    ReminderDiagnostics {
        reminders: ReminderDiagnostics,
    },
    /// Open the profile-global reminder destination in the addressed shell.
    OpenReminders,
    /// Come back from the notification area.
    Restore,
    /// **Set voice input up.** The notification area's *Download model…* row
    /// was chosen, and the agent has nowhere to ask the question.
    ///
    /// It carries nothing, and that is a decision rather than an omission. The
    /// consent step names a download size and a licence, and both come off a
    /// *signed* catalog entry — putting them on this message would mean the
    /// sentence somebody reads had travelled through a process that never
    /// verified it, and would leave a second copy of the offer in flight while
    /// the first was being chosen. The agent's contribution is the gesture.
    ///
    /// **The download runs in the window, not in the agent.** See
    /// `koru_workspace_host::voice_install`: the consent step is a modal panel and the
    /// progress toast is a toast, an agent has neither, and a window whose
    /// agent will not start still has to be able to set voice up.
    Voice,
    /// **Listen with this microphone.** The notification area's *Input*
    /// submenu was used, and the agent may not write the preference itself.
    ///
    /// `None` is *follow the operating system*. A name is a device's name
    /// verbatim, never a position in a list — `koru_voice::input` has the
    /// argument, and the short version is that an index silently means a
    /// different microphone after a reboot.
    ///
    /// **It travels as a `String` rather than as a `koru_voice::InputChoice`**,
    /// because this crate carries ids as strings on purpose: `koru-agent` has
    /// no node model and no voice vocabulary, and must not acquire one to pass
    /// a name along. The shell parses it.
    ///
    /// **The agent proposes and the shell writes.** `preferences.conf` has one
    /// writer — `koru_window::user_preferences`' own note is explicit about
    /// it — so a tray choice is a gesture routed to a window rather than a
    /// second process editing a file the first one rewrites wholesale.
    VoiceInput {
        device: Option<String>,
    },
    /// Deliver Agent's registered Dictation chord to the focused Workspace's
    /// existing local controller. The token correlates content-free state
    /// replies; no transcript, target, or editable content crosses this wire.
    DictationGesture {
        route: u64,
        gesture: DictationGesture,
    },
    /// Bring Koru Workspace forward and show a terminal session as a tab.
    ///
    /// `Some` names a running session by its id — the receiving window focuses
    /// the tab already showing it, or opens a tab attached to it (never a
    /// launch). `None` is the tray's **New**: the window opens its new-tab
    /// chooser, because which agent to start is the person's choice and the
    /// notification area has nowhere to ask it.
    ShowSession {
        session: Option<String>,
    },
    /// Everything the agent knows about updating this installation, whole, on
    /// every change. See [`UpdateReport`].
    ///
    /// **Whole rather than a delta.** A shell can connect halfway through a
    /// download, and a report that only said what had moved would leave it with
    /// no way to draw the banner until the next byte arrived. Holding the last
    /// one and nothing else also means a shell has no update state machine to
    /// get out of step with the agent's.
    ///
    /// Boxed for the same reason [`AgentGreeting::Welcome`] is: `Push` is
    /// matched by value at every call site, and an enum is as large as its
    /// largest variant — so an unboxed report would make `Restore`, the
    /// two-byte message, cost as much to move as the release notes.
    Update {
        report: Box<UpdateReport>,
    },
    /// **An install finished, and this is the build it produced.** Show the
    /// person what changed.
    ///
    /// **An event, and the only reason it is not a stage on [`UpdateReport`].**
    /// A report is *state* — pushed whole on every change, and again to every
    /// window that says hello, precisely so a window that arrived late is not
    /// left ignorant. Both of those are exactly wrong for this: a stage saying
    /// "installed" would re-fire every time the state was re-sent, so the tab
    /// would reopen on every reconnect and every check for the rest of the
    /// build's life. This happens once, is sent to one window, and is forgotten
    /// by both sides.
    ///
    /// It carries the version rather than letting the shell read its own
    /// `build_info()`, which would be the same string on the happy path. They
    /// stop being the same string the moment a shell from one install is
    /// talking to an agent from another, and the agent is the one that knows
    /// which build was installed.
    Updated {
        version: String,
    },
    /// Open Send feedback in a window of its own. One-shot, like [`Self::Updated`].
    ShowFeedback,
    /// Open Contact support — the singleton support window. One-shot.
    ShowSupport,
    Pairing {
        view: Box<PairingView>,
    },
    /// Which global-search chord this agent actually registered, so a surface
    /// the agent does not draw (a shell's taskbar Jump List) can say it.
    ///
    /// **Pushed, never re-derived by the shell.** `RegisterHotKey` is
    /// machine-wide and the agent is its owner by construction; a shell that
    /// tried to work the chord out for itself would both lie (it cannot know
    /// which candidate won) and risk stealing it. The shape matches
    /// `AgentEvent::HotkeyRegistered`, the in-process twin that feeds the
    /// tray: `Some` is a person-facing label such as `Ctrl+Shift+K`, and `None`
    /// means there is no chord — every candidate was taken, or the agent has an
    /// explicit `--root`.
    ///
    /// **Re-pushed on every `Hello`, like [`Update`] and [`Pairing`].** The
    /// answer is known once, shortly after the agent starts, and a window that
    /// connects later would otherwise never learn it.
    Shortcut {
        shortcut: Option<String>,
    },
    /// **The Agent is stopping because a person asked it to** — the tray's
    /// *Quit*, or `koru-agent stop`. Sent to every window just before the
    /// stop begins, so it reaches them ahead of the end of the stream.
    ///
    /// A window that reads it does not start a replacement Agent when the link
    /// ends. Without it a window cannot tell a quit from a crash, and it treats
    /// every ended link as a crash: it dials, finds nothing listening, and
    /// starts a new Agent — so *Quit* brought back the very process it had
    /// just stopped. A crash still sends nothing, and a window still restarts
    /// an Agent that died.
    ///
    /// Not sent when the Agent stops for an identity reset, after which a
    /// window relaunches onto the new identity, nor by an install handoff,
    /// which closes the windows itself before the Agent goes.
    Quitting,
    /// **Whether this Agent has a notification-area icon right now** — the
    /// way back to a window hidden behind it.
    ///
    /// A window hides to the tray only when this was last `true`. It was a
    /// compile-time constant in the window (Windows and macOS yes, Linux no),
    /// which was wrong as soon as Linux had an icon and wrong already wherever
    /// the icon failed to appear. Only the Agent knows, and on Linux the
    /// answer moves: a StatusNotifierItem host (KDE's panel, GNOME's
    /// AppIndicator extension) can appear after the Agent starts, or go away.
    ///
    /// **Sent on every change, and on `Hello` when it is `true`.** A window
    /// starts at `false` and returns to it whenever its link ends, so `false`
    /// on `Hello` would say nothing it does not already assume.
    NotificationArea {
        present: bool,
    },
}

/// Commands accepted by the account worker. String ids remain strings on this
/// leaf IPC crate and are validated by the owning account/Profile domains.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountCommand {
    SwitchAccount {
        account_id: String,
    },
    AddAccount {
        locale: String,
    },
    ConfirmAccountCreation,
    CancelAccountCreation,
    Status,
    StartGoogle {
        locale: String,
    },
    /// Sign in with Apple in the system browser. Everything after the browser
    /// returns -- creation, Profile choice, the exchange -- is Google's path.
    StartApple {
        locale: String,
    },
    /// Sign in with an email address and a password.
    ///
    /// Begin an email sign-up or sign-in **inside the app**.
    ///
    /// The welcome screen's one submit: an address and a password.
    ///
    /// **Which journey this is, is the worker's to work out, not the
    /// screen's.** A match signs in. Anything else -- no such account, or the
    /// wrong password -- mails a code to that address and parks at
    /// `AwaitingEmailCode`, so the screen cannot be used to ask whether an
    /// address is known. What it was is said one screen later, to somebody
    /// who has proved they read the mailbox.
    ContinueWithEmail {
        locale: String,
        address: String,
        password: String,
    },
    /// The six digits, typed on the surface that started the flow.
    ///
    /// Answers with `AwaitingEmailPassword` when the address is proved, and
    /// stays at `AwaitingEmailCode` with a failure code when it is not.
    SubmitEmailCode {
        code: String,
    },
    /// The password for a new account, which completes the sign-up and
    /// exchanges the grant this device was bound to.
    SubmitEmailPassword {
        password: String,
    },
    /// Send the code again, to the address the journey already holds.
    ///
    /// Carries nothing: naming an address here would be a second way to
    /// point the mailer at one, and the flow already knows which it is. The
    /// cooldown is the server's.
    ResendEmailCode,
    Cancel {
        generation: u64,
    },
    Retry,
    RetryEnrollment {
        profile_id: Option<String>,
    },
    CancelEnrollment,
    /// Return the exact Auth Identity behind the current session. This is a
    /// correlated result because its verified email is PII.
    InspectAccountSession,
    InspectCloudSubscription {
        request_id: u64,
    },
    /// Turn Koru Cloud on or off for every Workspace this device holds for
    /// one Profile of the current account, and remember the choice for
    /// Workspaces created later. Answered by `CloudSync` for the same id.
    SetProfileCloudSync {
        request_id: u64,
        profile_id: String,
        enabled: bool,
    },
    /// Read every local Profile's Cloud sync state. Answered by `CloudSync`.
    InspectCloudSync {
        request_id: u64,
    },
    ListProfiles,
    /// Return every Profile authorized for the current account together with
    /// the device availability onboarding needs to explain each next step.
    /// Manage Profiles continues to use `ListProfiles` for its plain catalog.
    ListProfileChoices,
    CreateProfile {
        profile_id: String,
        owner_display_name: String,
        name: String,
        locale: Option<String>,
    },
    /// Take an existing local Profile into the signed-in account: the hosted
    /// record is created under this identity and provisioned for this device,
    /// and the caller then binds the local Profile to the identity the result
    /// names. The snapshots are what other devices see the Profile called
    /// before they hold a copy of it.
    AttachProfile {
        profile_id: String,
        #[serde(default)]
        name_snapshot: Option<String>,
        #[serde(default)]
        locale_snapshot: Option<String>,
    },
    AddGoogle {
        profile_id: String,
        locale: String,
    },
    ConfirmAddGoogle {
        candidate_id: String,
        profile_id: String,
    },
    CancelAddGoogle {
        candidate_id: String,
        profile_id: String,
    },
    RebindProfile {
        profile_id: String,
        erased_identity_id: String,
    },
    RemoveProfileAuthorization {
        profile_id: String,
        auth_identity_id: String,
        confirm_profile_offline_only: bool,
    },
    DeleteProfile {
        profile_id: String,
        expected_lifecycle_revision: u64,
    },
    RestoreProfile {
        profile_id: String,
        expected_lifecycle_revision: u64,
        name_snapshot: String,
        locale_snapshot: Option<String>,
    },
    ReconcileProfileLifecycles,
    InspectProfileLifecycle {
        profile_id: String,
    },
    AcknowledgeProfileCleanup {
        profile_id: String,
        stage: ProfileCleanupStage,
    },
    SelectProfiles {
        profile_ids: Vec<String>,
    },
    Logout,
}

impl std::fmt::Debug for AccountCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SwitchAccount { .. } => "AccountCommand::SwitchAccount(<redacted>)",
            Self::AddAccount { .. } => "AccountCommand::AddAccount(<redacted>)",
            Self::ConfirmAccountCreation => "AccountCommand::ConfirmAccountCreation",
            Self::CancelAccountCreation => "AccountCommand::CancelAccountCreation",
            Self::Status => "AccountCommand::Status",
            Self::StartGoogle { .. } => "AccountCommand::StartGoogle(<redacted>)",
            Self::StartApple { .. } => "AccountCommand::StartApple(<redacted>)",
            // The address and the password are the whole of these.
            Self::ContinueWithEmail { .. } => "AccountCommand::ContinueWithEmail(<redacted>)",
            Self::SubmitEmailCode { .. } => "AccountCommand::SubmitEmailCode(<redacted>)",
            Self::SubmitEmailPassword { .. } => "AccountCommand::SubmitEmailPassword(<redacted>)",
            Self::ResendEmailCode => "AccountCommand::ResendEmailCode",
            Self::Cancel { .. } => "AccountCommand::Cancel",
            Self::Retry => "AccountCommand::Retry",
            Self::RetryEnrollment { .. } => "AccountCommand::RetryEnrollment(<redacted>)",
            Self::CancelEnrollment => "AccountCommand::CancelEnrollment",
            Self::InspectAccountSession => "AccountCommand::InspectAccountSession",
            Self::InspectCloudSubscription { .. } => "AccountCommand::InspectCloudSubscription",
            Self::SetProfileCloudSync { .. } => "AccountCommand::SetProfileCloudSync(<redacted>)",
            Self::InspectCloudSync { .. } => "AccountCommand::InspectCloudSync",
            Self::ListProfiles => "AccountCommand::ListProfiles",
            Self::ListProfileChoices => "AccountCommand::ListProfileChoices",
            Self::CreateProfile { .. } => "AccountCommand::CreateProfile(<redacted>)",
            Self::AttachProfile { .. } => "AccountCommand::AttachProfile(<redacted>)",
            Self::AddGoogle { .. } => "AccountCommand::AddGoogle(<redacted>)",
            Self::ConfirmAddGoogle { .. } => "AccountCommand::ConfirmAddGoogle(<redacted>)",
            Self::CancelAddGoogle { .. } => "AccountCommand::CancelAddGoogle(<redacted>)",
            Self::RebindProfile { .. } => "AccountCommand::RebindProfile(<redacted>)",
            Self::RemoveProfileAuthorization { .. } => {
                "AccountCommand::RemoveProfileAuthorization(<redacted>)"
            }
            Self::DeleteProfile { .. } => "AccountCommand::DeleteProfile(<redacted>)",
            Self::RestoreProfile { .. } => "AccountCommand::RestoreProfile(<redacted>)",
            Self::ReconcileProfileLifecycles => "AccountCommand::ReconcileProfileLifecycles",
            Self::InspectProfileLifecycle { .. } => {
                "AccountCommand::InspectProfileLifecycle(<redacted>)"
            }
            Self::AcknowledgeProfileCleanup { .. } => {
                "AccountCommand::AcknowledgeProfileCleanup(<redacted>)"
            }
            Self::SelectProfiles { .. } => "AccountCommand::SelectProfiles(<redacted>)",
            Self::Logout => "AccountCommand::Logout",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStage {
    AwaitingAccountCreation,
    /// **This build has no Koru Cloud** (`koru_env::cloud_configured`): there
    /// is nothing to sign in to, so no account capability exists and nothing
    /// is retried. Not [`Self::Unavailable`], which is a Cloud that exists and
    /// could not be reached; a surface says *isn't set up in this build*.
    ///
    /// Only a build with no origin sends it, and its desktop is the same
    /// build, so no older reader meets the spelling: an official build always
    /// bakes an origin.
    NotConfigured,
    Unavailable,
    SignedOut,
    Restoring,
    WaitingForBrowser,
    /// The code screen is up and the worker is holding the flow open.
    AwaitingEmailCode,
    /// The address is proved; the app is asking for a password to set.
    AwaitingEmailPassword,
    Exchanging,
    SignedIn,
    Refreshing,
    Cancelled,
    Failed,
}

/// Content-light whole state. It deliberately carries no email, provider
/// subject, token, Profile name, OAuth state, grant, PKCE value, or full URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountState {
    pub active_scope: u64,
    pub active_account_id: Option<String>,
    pub held_accounts: Vec<AccountHeldAccount>,
    pub generation: u64,
    pub stage: AccountStage,
    pub capability: bool,
    pub profile_count: usize,
    pub queue_depth: usize,
    pub enrollment: EnrollmentState,
    pub enrollments: Vec<AccountProfileEnrollment>,
    /// Device choices for the active account, with live Cloud connection progress.
    /// No stage here claims that all content is durably covered by Cloud.
    #[serde(default)]
    pub cloud_profiles: Vec<AccountProfileCloud>,
    #[serde(default)]
    pub enrollment_failure_code: Option<String>,
    #[serde(default)]
    pub enrollment_retry_at_millis: Option<i64>,
    pub retry_at_millis: Option<i64>,
    pub failure_code: Option<String>,
}

/// Nonsecret process-local routing and durable account discovery. Display names
/// and emails remain targeted operation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountHeldAccount {
    pub scope_id: u64,
    pub account_id: String,
    pub auth_identity_id: Option<String>,
    pub stage: AccountStage,
    pub credential_ready: bool,
    pub failure_code: Option<String>,
    pub active_generations: Vec<u64>,
}

/// Nonsecret progress for one selected Profile, attributed by the containing
/// account scope and (for command results) its immutable operation generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfileEnrollment {
    pub profile_id: String,
    pub state: EnrollmentState,
    pub failure_code: Option<String>,
    pub retry_at_millis: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfileCloud {
    pub profile_id: String,
    pub stage: CloudConnectionStage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudConnectionStage {
    Connecting,
    Syncing,
    Retrying,
    WaitingForConsent,
}

/// A Profile row is operation data, not broadcast account state. Names and
/// locale snapshots may contain PII, so this type only travels in the targeted
/// completion for the shell that requested it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAuthIdentityRow {
    pub auth_identity_id: String,
    pub provider: String,
    pub verified_email: Option<String>,
}

impl std::fmt::Debug for AccountAuthIdentityRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccountAuthIdentityRow(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfileRow {
    pub koru_identity_id: String,
    pub profile_id: String,
    /// **The Profile's own identity document where this device holds one**, and
    /// the account's `discovery_name_snapshot` only where it does not.
    ///
    /// The account's copy is written when a Profile is created and when a
    /// deleted one is restored, and nothing updates it after that — there is no
    /// rename route, by decision, because a server holding an accurate name for
    /// every Profile is a liability under encryption rather than a field worth
    /// maintaining. Preferring it showed a Profile's creation-time name for
    /// ever. It survives as the one thing a device with an account session and
    /// no synced documents can put in a list on first run.
    ///
    /// The overlay happens once, in `koru-account-runtime`, which is the only
    /// place holding both the account listing and the local Store; a surface
    /// draws this field and does not choose between two sources itself.
    pub name: Option<String>,
    /// The identity document's locale where this device holds one, otherwise the
    /// account's snapshot. See [`AccountProfileRow::name`].
    pub locale: Option<String>,
    #[serde(default)]
    pub auth_identities: Vec<AccountAuthIdentityRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddGoogleCancellationStatus {
    Cancelled,
    AlreadyCancelled,
    AlreadyConfirmed,
}

impl std::fmt::Debug for AccountProfileRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountProfileRow")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

/// One hosted Profile and the independently derived places from which this
/// installation may obtain its content. Authorization never implies content
/// availability, so onboarding must receive both facts without filtering the
/// Profile catalog.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfileChoice {
    pub profile: AccountProfileRow,
    pub available_on_this_device: bool,
    pub available_on_another_device: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountProfileLifecycleState {
    Active,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCleanupStage {
    HostedPending,
    HostedCommitted,
    WorkspacesRemoved,
    ProfileRemoved,
    SettingsCleared,
    NavigationSettled,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfileLifecycleRow {
    pub profile_id: String,
    pub lifecycle_state: AccountProfileLifecycleState,
    pub lifecycle_revision: u64,
    pub operation_id: Option<String>,
    pub cleanup_stage: Option<ProfileCleanupStage>,
    pub failure_code: Option<String>,
}

impl std::fmt::Debug for AccountProfileLifecycleRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountProfileLifecycleRow")
            .field("lifecycle_state", &self.lifecycle_state)
            .field("lifecycle_revision", &self.lifecycle_revision)
            .field("cleanup_stage", &self.cleanup_stage)
            .field("failure_code", &self.failure_code)
            .finish()
    }
}

impl std::fmt::Debug for AccountProfileChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountProfileChoice")
            .field("profile", &"<redacted>")
            .field("available_on_this_device", &self.available_on_this_device)
            .field(
                "available_on_another_device",
                &self.available_on_another_device,
            )
            .finish()
    }
}

/// One Profile's Cloud sync state on this device. `enabled` is the
/// remembered choice; `workspaces` counts the Workspaces this device holds
/// for the Profile and `synced_workspaces` how many of them Koru Cloud has
/// accepted, so a partially applied choice is visible rather than hidden.
/// `error` is the stable code of the last refusal while applying the choice,
/// never a message with content in it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudSyncProfile {
    pub profile_id: String,
    pub enabled: bool,
    pub workspaces: u32,
    pub synced_workspaces: u32,
    pub error: Option<String>,
}

impl std::fmt::Debug for CloudSyncProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudSyncProfile(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountOperationResult {
    /// Card-only decision. No Stripe identifier, email, credential or URL.
    CloudSubscription {
        request_id: u64,
        current: Option<bool>,
    },
    /// Every local Profile's Cloud sync state, after a read or a change. A
    /// Profile absent from the list has no Cloud sync state on this device.
    CloudSync {
        request_id: u64,
        profiles: Vec<CloudSyncProfile>,
    },
    AccountSelected {
        scope_id: u64,
        account_id: Option<String>,
    },
    AccountCreationRequired,
    SignedIn,
    /// Targeted session metadata. Never copy either field into AccountState,
    /// logs, events, status, or diagnostics.
    SignedInIdentity {
        provider: String,
        verified_email: String,
    },
    Profiles {
        profiles: Vec<AccountProfileRow>,
    },
    ProfileChoices {
        display_name: String,
        choices: Vec<AccountProfileChoice>,
    },
    Profile {
        profile: AccountProfileRow,
    },
    ProfileAuthorizationRemoved {
        profile_id: String,
        profile_offline_only: bool,
        already_removed: bool,
    },
    ProfileDeleted {
        lifecycle: AccountProfileLifecycleRow,
        exact_retry: bool,
    },
    ProfileRestored {
        lifecycle: AccountProfileLifecycleRow,
        exact_retry: bool,
    },
    ProfileLifecycle {
        lifecycle: Option<AccountProfileLifecycleRow>,
    },
    ProfileLifecycleReconciliation {
        profiles: Vec<AccountProfileLifecycleRow>,
    },
    AddGoogleCandidate {
        candidate_id: String,
        profile_id: String,
        verified_email: String,
        expires_at_millis: i64,
    },
    GoogleAuthorizationAdded {
        koru_identity_id: String,
        profile_id: String,
        already_authorized: bool,
    },
    AddGoogleCancelled {
        profile_id: String,
        status: AddGoogleCancellationStatus,
    },
    ProfilesSelected {
        profiles: Vec<AccountProfileEnrollment>,
    },
    SignedOut,
    Cancelled,
    Failed {
        code: String,
    },
}

impl std::fmt::Debug for AccountOperationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::CloudSubscription { .. } => "CloudSubscription(<redacted>)",
            Self::CloudSync { .. } => "CloudSync(<redacted>)",
            Self::AccountSelected { .. } => "AccountSelected(<redacted>)",
            Self::AccountCreationRequired => "AccountCreationRequired",
            Self::SignedIn => "SignedIn",
            Self::SignedInIdentity { .. } => "SignedInIdentity(<redacted>)",
            Self::Profiles { .. } => "Profiles(<redacted>)",
            Self::ProfileChoices { .. } => "ProfileChoices(<redacted>)",
            Self::Profile { .. } => "Profile(<redacted>)",
            Self::ProfileAuthorizationRemoved { .. } => "ProfileAuthorizationRemoved(<redacted>)",
            Self::ProfileDeleted { .. } => "ProfileDeleted(<redacted>)",
            Self::ProfileRestored { .. } => "ProfileRestored(<redacted>)",
            Self::ProfileLifecycle { .. } => "ProfileLifecycle(<redacted>)",
            Self::ProfileLifecycleReconciliation { .. } => {
                "ProfileLifecycleReconciliation(<redacted>)"
            }
            Self::AddGoogleCandidate { .. } => "AddGoogleCandidate(<redacted>)",
            Self::GoogleAuthorizationAdded { .. } => "GoogleAuthorizationAdded(<redacted>)",
            Self::AddGoogleCancelled { .. } => "AddGoogleCancelled(<redacted>)",
            Self::ProfilesSelected { .. } => "ProfilesSelected(<redacted>)",
            Self::SignedOut => "SignedOut",
            Self::Cancelled => "Cancelled",
            Self::Failed { .. } => "Failed",
        };
        f.write_str(kind)
    }
}

impl Default for AccountState {
    fn default() -> Self {
        Self {
            active_scope: 0,
            active_account_id: None,
            held_accounts: Vec::new(),
            generation: 0,
            stage: AccountStage::Unavailable,
            capability: false,
            profile_count: 0,
            queue_depth: 0,
            enrollment: EnrollmentState::Idle,
            enrollments: Vec::new(),
            cloud_profiles: Vec::new(),
            enrollment_failure_code: None,
            enrollment_retry_at_millis: None,
            retry_at_millis: None,
            failure_code: None,
        }
    }
}

impl AccountState {
    /// The whole state of a build with no Koru Cloud: [`AccountStage::NotConfigured`],
    /// no capability, and no failure — nothing went wrong.
    pub fn not_configured() -> Self {
        Self {
            stage: AccountStage::NotConfigured,
            ..Self::default()
        }
    }

    /// What an Agent with no account capability answers: not configured when
    /// the build has no Koru Cloud, and otherwise the default *unavailable*.
    pub fn without_capability(cloud_configured: bool) -> Self {
        if cloud_configured {
            Self::default()
        } else {
            Self::not_configured()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentState {
    #[default]
    Idle,
    Waiting,
    Enrolling,
    Complete,
    Failed,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrowserUrl(pub String);

impl std::fmt::Debug for BrowserUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserUrl(REDACTED)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderSnoozeIntent {
    pub delivery: String,
    pub profile: String,
    pub reminder: String,
    pub occurrence: String,
    pub entity: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    Idle,
    Displaying,
    Connecting,
    Confirming,
    Preparing,
    Finishing,
    Succeeded,
    Expired,
    Cancelled,
    Failed,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingView {
    pub state: PairingState,
    pub qr_payload: Option<String>,
    pub expires_in_seconds: u16,
    pub peer_name: Option<String>,
    pub error_code: Option<String>,
}

impl std::fmt::Debug for PairingView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingView")
            .field("state", &self.state)
            .field(
                "qr_payload",
                &self.qr_payload.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in_seconds", &self.expires_in_seconds)
            .field("peer_name", &self.peer_name)
            .field("error_code", &self.error_code)
            .finish()
    }
}

/// A search result the person chose, on the wire.
///
/// Field-for-field `koru_search_ui::Target`, with the ids as strings — see the
/// module note. The conversion is at the edges, and a host that cannot parse an
/// id refuses the navigation rather than guessing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub profile: String,
    pub workspace: String,
    /// `None` is a workspace-level match, which has nothing more to open.
    pub document: Option<String>,
    /// `None` is a hit on a document's **title**, which is mirrored from the
    /// manifest listing rather than read from a node in anybody's tree.
    pub node: Option<String>,
    /// Whether the document is in the bin. **Not a "trash root"** — the store's
    /// model has no such node, and what a host keys its bin rows by is the
    /// host's business.
    pub trashed: bool,
    /// Authored evidence, for placing a caret or a transient flash. Never used
    /// to find anything.
    pub match_text: Option<String>,
    pub new_window: bool,
}

// ------------------------------------------------------------------ updating

/// Everything the agent knows about updating this installation, flat enough to
/// draw a banner from and nothing more.
///
/// ## This crate does not link `koru-update`, and that is the whole design
///
/// `koru-agent-wire` is a leaf whose only dependency is serde. The module note
/// says why ids are strings here — typing them would put a CRDT engine in the
/// graph of anything that wants to send a message — and the same argument lands
/// harder on this type: `koru-update` brings reqwest, minisign-verify and zip
/// with it, so a program that only wanted to *draw the banner* would acquire an
/// HTTP client, a signature verifier and an archive extractor in order to do
/// it. That is the shape [ADR-105](../../../docs/decisions/ADR-101-125.md#adr-105-ipc--mechanism-is-pushed-down-and-domain-behaviour-stays-with-its-domain)
/// exists to prevent, said about a second domain.
///
/// So this is strings and numbers, and the conversion happens at both edges:
/// the agent projects a `koru_update::UpdateStatus` onto it, and an app reads it
/// into whatever its banner wants. Neither end holds the other's types.
///
/// ## And it is not an `UpdateManifest` either
///
/// The obvious shortcut is to forward the manifest, which already has the
/// version, the build number and the release notes in it. It also has the
/// package URL, its size, its SHA-256 and its **signature** — the four fields
/// that decide what gets executed on this machine. A banner needs none of them,
/// and this channel crosses a process boundary, so a narrower type is a
/// narrower blast radius rather than tidiness. What is downloaded and what is
/// verified stay entirely on the agent's side of the socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub stage: UpdateStage,
    /// The build being offered — empty and `0` when there is none, which is
    /// what most reports say. Two stages bend that slightly and neither is a
    /// bug: [`UpdateStage::InstalledNewer`] names the build the channel has
    /// (this installation is *ahead* of it), and [`UpdateStage::Deferred`]
    /// names the one the person put off.
    pub version: String,
    /// See [`UpdateReport::version`].
    pub build_number: u64,
    /// The release notes' title, straight from the manifest. **This is the
    /// "what's new" a person reads before consenting**, so it is present from
    /// [`UpdateStage::Available`] onwards rather than appearing at install
    /// time.
    pub title: String,
    /// The release notes' summary lines, straight from the manifest. A banner
    /// shows the first few; how many is the banner's business, and truncating
    /// here would mean a wider window could not show more.
    pub summary: Vec<String>,
    /// What *View all changes* opens. `None` when the release named no page —
    /// the button is then not drawn, rather than drawn and inert.
    pub notes_url: Option<String>,
    /// Bytes so far, and the package's size. Both `0` outside
    /// [`UpdateStage::Downloading`].
    pub received: u64,
    /// See [`UpdateReport::received`].
    pub total: u64,
    /// Whether the agent can perform the swap here — in practice, whether the
    /// installation directory is writable.
    ///
    /// **The app must not work this out for itself.** It used to probe the
    /// filesystem beside its own executable, which answers a question about
    /// *that* process's directory and its privileges; the agent is the process
    /// that will do the replacing, so it is the only one whose answer is about
    /// the operation that will actually be attempted. When this is `false` the
    /// only honest button is *Download manually*, and [`Self::download_url`] is
    /// what it opens.
    pub can_install: bool,
    /// Where a person goes when the agent cannot install for them. `None`
    /// unless there is a build to point at.
    pub download_url: Option<String>,
    /// A failure's person-facing sentence, mirroring `UpdateFailure::message`.
    /// `None` unless the stage is [`UpdateStage::Failed`].
    pub message: Option<String>,
    /// The same failure's developer text, mirroring `UpdateFailure::detail` —
    /// for a log or a bug report, never for the banner. The rule
    /// [`Response::Failed`] already follows, said about a push.
    pub detail: Option<String>,
    /// Which channel this installation follows, in its storage spelling
    /// (`stable`, `beta`, `nightly`, …). A fact the Welcome screen shows, and a
    /// string rather than an enum for the reason in the type's note — the enum
    /// lives in `koru-channel` and this crate links nothing.
    pub channel: String,
    /// Whether the agent checks on its own. Shown beside the channel, and
    /// changed with [`UpdateChoice::SetAutomatic`].
    pub automatic_checks: bool,
    /// Number of active terminal processes in the private session host when
    /// this report was produced. Content and output never cross this wire.
    #[serde(default)]
    pub active_session_count: u32,
    /// Compare-and-consent epoch for the active session set. `0` means the
    /// session host is unavailable, in which case install is refused safely.
    #[serde(default)]
    pub session_generation: u64,
}

/// Where in the update conversation this installation is —
/// `koru_update::UpdateStatus` with its payloads lifted into
/// [`UpdateReport`]'s fields.
///
/// Flat on purpose. The status enum carries a whole manifest in two of its arms
/// and a `PathBuf` in one, and a wire type that mirrored that shape would have
/// to carry the package path — a filesystem location in the agent's world,
/// meaningless in the shell's and no business of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStage {
    /// Nothing has been asked yet.
    Idle,
    Checking,
    UpToDate,
    /// What is installed is newer than what the channel offers — a developer
    /// build, or a channel switched downwards. Not an error, and dismissed with
    /// [`UpdateChoice::DismissNewer`].
    InstalledNewer,
    /// A build was found and **nothing has been downloaded**. This is where the
    /// first consent is asked for.
    Available,
    Downloading,
    Verifying,
    /// Downloaded and verified, waiting on the second consent.
    Ready,
    Installing,
    /// The person said *Later* about this build; it will not be offered again
    /// until a newer one arrives.
    Deferred,
    Failed,
}

/// What the person pressed.
///
/// ## Two consent steps, and the agent enforces the order
///
/// A build is found and the person consents to **download** it; the download
/// finishes and they consent to **install** it. Never one step, never silently.
/// That is a product rule, and a rule only a drawing convention enforces is one
/// somebody removes by drawing a different banner — so the agent checks the
/// stage before it acts: [`Self::Download`] is refused unless the stage is
/// [`UpdateStage::Available`], and [`Self::Install`] unless it is
/// [`UpdateStage::Ready`]. A shell cannot skip a step even by sending the
/// message for the step it wants, which is what makes the two steps a property
/// of the system rather than of one banner's code.
///
/// ## The three codes
///
/// Answered [`Response::Ok`], or [`Response::Failed`] with one of these — ASCII
/// and stable, because they are branched on:
///
/// - `no_update` — there is nothing being offered to act on;
/// - `wrong_stage` — there is, and it is not at the step this asks for. The
///   refusal above arrives as this one;
/// - `not_permitted` — this agent will not touch the installation. An agent
///   started with an explicit `--root` takes nothing machine-wide, by the same
///   decision that makes it refuse the global shortcut, so it refuses to
///   replace a binary it does not own.
///
/// `Ok` means the agent has taken the choice, not that the work is done. What
/// happened next arrives as the next [`Push::Update`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateChoice {
    /// Yes, fetch it. The first consent.
    Download,
    /// Yes, install it and restart. The second.
    Install { session_generation: u64 },
    /// Not this build. Defers it until a newer one appears.
    Later,
    /// Stop the download that is running. Not the same as `Later`: the build is
    /// still offered afterwards.
    Cancel,
    /// Try the failed step again.
    Retry,
    /// Check now, whatever [`UpdateReport::automatic_checks`] says.
    CheckNow,
    /// Put away the "your build is newer" notice.
    DismissNewer,
    /// Check on your own, or do not.
    SetAutomatic { on: bool },
    /// A window was asked to close for an install and did not.
    ///
    /// **Not [`Self::Later`], which is what this used to be sent as.** Later
    /// means *not this build* and defers it until a newer one appears; a person
    /// who answered "no" to *there are unsaved changes, close Koru?* has said
    /// nothing about the build at all. Sent as itself, it ends the handoff and
    /// leaves the offer exactly where it was, so the banner is still there when
    /// they are ready — which is what `Handoff::refused` was always documented
    /// to do and, until now, had no caller to do it.
    CloseRefused,
}

#[cfg(test)]
mod tests;
