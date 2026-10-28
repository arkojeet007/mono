// SPDX-License-Identifier: MPL-2.0

//! The agent, as the shared window loop sees it.
//!
//! This is the **one** thread that owns the registry, the router and the
//! window. Everything else raises an [`AgentEvent`] and gets out of the way.
//!
//! ## It is windowless most of the time
//!
//! `wants_window` is `false` until the hotkey or the tray asks for the overlay,
//! and `koru_window::run` creates and drops the surface to match. A
//! tray-resident agent with nothing on screen sits at zero CPU — `ControlFlow::
//! Wait`, and nothing here polls.
//!
//! **The `Renderer` outlives the window**, which is what makes reopening the
//! overlay free: `Renderer::new` compiles every pipeline vello has, 7.6 s in a
//! debug build, and paying that on every hotkey press would make the feature
//! unusable. `koru-window` keeps renderers per *device* for exactly this.
//!
//! ## Closing the overlay does not end the agent
//!
//! `close_requested` puts the overlay away; `should_exit` is only ever set by
//! *Quit*. That is why the trait requires an answer to closing rather than
//! assuming one — a transient overlay closes, while Agent does not exit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use koru_account_core::KoruIdentityId;
use koru_agent_wire::{FromAgent, Push, Request, Response, Service};
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use koru_store_core::ProfileAccountBinding;
use koru_store_core::ids::{ProfileId, WorkspaceId};
use koru_window::{WindowApp, WindowRequest};
use vello::Scene;
use winit::dpi::PhysicalSize;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{ResizeDirection, WindowAttributes};

use crate::connection::Connection;
use crate::event::{AgentEvent, TrayAsk};
use crate::registry::{ConnectionId, Registry};
use crate::route::{Routed, Router};
use crate::routing_log::RoutingLog;
use crate::{Paths, service};
use koru_window::SearchGeometry;

fn profile_account_binding(value: &str) -> Option<ProfileAccountBinding> {
    KoruIdentityId::from_str(value)
        .ok()
        .and_then(|identity| ProfileAccountBinding::from_identity_uuid(identity.as_uuid()).ok())
}

struct DeferredOpen {
    target: koru_search_ui::Target,
    stage: DeferredStage,
    deadline: Instant,
}

#[derive(Clone)]
enum VoiceIntent {
    Enable,
    Disable,
    Startup(bool),
    Shortcut(String, u64),
}

struct VoiceWrite {
    intent: VoiceIntent,
    receipt: koru_api_client::ConfirmedWrite<koru_settings::GlobalSettings>,
}

#[derive(Clone, Copy)]
enum VoiceStartStage {
    Closing,
    Prepare,
    Starting,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VoiceAttemptOrigin {
    Surface,
    Tray,
    Hotkey,
}

#[derive(Clone, Copy)]
enum WindowTransition {
    ClosingSearch,
    ReadySearch,
    ClosingVoice,
    ReadyVoice,
}

struct PendingReminderSnooze {
    profile: ProfileId,
    workspace: WorkspaceId,
    intent: koru_agent_wire::ReminderSnoozeIntent,
    assigned: Option<ConnectionId>,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum DeferredStage {
    /// The current `settle` still owns the always-on-top search HWND. Advance
    /// once, let the shared loop drop it, then route on the next loop turn.
    ClosingOverlay,
    Ready,
}

/// Requests that can admit new work or launch a process after update
/// quiescence has begun. Status, close/refusal, and shutdown traffic must still
/// pass so the handoff can finish or abort cleanly.
fn starts_work_while_quiescing(request: &Request) -> bool {
    match request {
        Request::Account { command, .. } => !matches!(
            command,
            koru_agent_wire::AccountCommand::Status
                | koru_agent_wire::AccountCommand::Cancel { .. }
        ),
        _ => matches!(
            request,
            Request::EnsureService { .. }
                | Request::RemoveLocalProfile { .. }
                | Request::BindLocalProfileAccount { .. }
                | Request::ShowOverlay
                | Request::OpenWorkspace { .. }
                | Request::Route { .. }
                | Request::OpenLink { .. }
                | Request::OpenChatLink { .. }
                | Request::OpenAiChat
                | Request::ReminderActivation { .. }
                | Request::NotificationControl { .. }
                | Request::StartPairing
                | Request::AcceptLinkCode { .. }
                // Rewrites the identity the handoff's replacement Agent opens.
                | Request::ResetIdentity
        ),
    }
}

/// Requests whose successful path may create a process. The updater's
/// token-bearing Agent can run inside the exclusive restart window, but no
/// ordinary shell/chat may cross it before that Agent joins the shared side.
fn starts_process_during_restart(request: &Request) -> bool {
    matches!(
        request,
        Request::EnsureService { .. }
            | Request::RemoveLocalProfile { .. }
            | Request::BindLocalProfileAccount { .. }
            | Request::OpenWorkspace { .. }
            | Request::Route { .. }
            | Request::OpenLink { .. }
            | Request::OpenChatLink { .. }
            | Request::OpenAiChat
            | Request::ReminderActivation { .. }
            | Request::NotificationControl { .. }
    )
}

trait GlobalDictationSink: Send {
    fn configure(&self, policy: crate::dictation_coordinator::Policy);
    fn prepare_target(&self);
    fn start_prepared(&self);
    fn discard_prepared(&self);
    fn cancel(&self, reason: crate::global_input::CancelReason);
    fn finish(&self);
    fn press(
        &self,
        local: Option<crate::event::LocalDictationOwner>,
        key: crate::global_input::VirtualKey,
    );
    fn local_state(
        &self,
        owner: crate::event::LocalDictationOwner,
        route: u64,
        state: crate::dictation_coordinator::LocalState,
    );
    fn shell_gone(&self, connection: ConnectionId);
}

impl GlobalDictationSink for crate::dictation_coordinator::Handle {
    fn configure(&self, policy: crate::dictation_coordinator::Policy) {
        crate::dictation_coordinator::Handle::configure(self, policy);
    }
    fn prepare_target(&self) {
        crate::dictation_coordinator::Handle::prepare_target(self);
    }
    fn start_prepared(&self) {
        crate::dictation_coordinator::Handle::start_prepared(self);
    }
    fn discard_prepared(&self) {
        crate::dictation_coordinator::Handle::discard_prepared(self);
    }
    fn cancel(&self, reason: crate::global_input::CancelReason) {
        crate::dictation_coordinator::Handle::cancel(self, reason);
    }
    fn finish(&self) {
        crate::dictation_coordinator::Handle::finish(self);
    }
    fn press(
        &self,
        local: Option<crate::event::LocalDictationOwner>,
        key: crate::global_input::VirtualKey,
    ) {
        crate::dictation_coordinator::Handle::press(self, local, key);
    }

    fn local_state(
        &self,
        owner: crate::event::LocalDictationOwner,
        route: u64,
        state: crate::dictation_coordinator::LocalState,
    ) {
        crate::dictation_coordinator::Handle::local_state(self, owner, route, state);
    }

    fn shell_gone(&self, connection: ConnectionId) {
        crate::dictation_coordinator::Handle::shell_gone(self, connection);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DictationDestination {
    Global,
    Local(crate::event::LocalDictationOwner),
    Refuse,
}

fn dictation_destination(
    foreground: crate::event::ForegroundPid,
    own_pid: u32,
    search_open: bool,
    registry: &Registry,
) -> DictationDestination {
    let crate::event::ForegroundPid::Known(pid) = foreground else {
        return DictationDestination::Refuse;
    };
    if pid == own_pid {
        return if search_open {
            DictationDestination::Local(crate::event::LocalDictationOwner::Agent)
        } else {
            DictationDestination::Refuse
        };
    }
    match registry.workspace_for_pid(pid) {
        crate::registry::WorkspacePidMatch::Unique(connection) => {
            DictationDestination::Local(crate::event::LocalDictationOwner::Workspace(connection))
        }
        crate::registry::WorkspacePidMatch::Ambiguous => DictationDestination::Refuse,
        crate::registry::WorkspacePidMatch::None => DictationDestination::Global,
    }
}

fn search_routed_press(
    current_route: Option<u64>,
    proposed_route: u64,
    stage: koru_dictation::Stage,
    active_key: Option<PhysicalKey>,
    proposed_key: PhysicalKey,
) -> koru_dictation::RoutedPress {
    koru_dictation::routed_press(
        current_route,
        proposed_route,
        stage,
        active_key == Some(proposed_key),
    )
}

fn workspace_state_authenticated(
    registry: &Registry,
    active: Option<(ConnectionId, u64)>,
    connection: ConnectionId,
    route: u64,
) -> bool {
    active == Some((connection, route)) && registry.shell(connection).is_some()
}

fn notification_control_request(
    operation: koru_agent_wire::NotificationOperation,
) -> Result<crate::reminders::NotificationControl, (&'static str, &'static str)> {
    use crate::reminders::NotificationControl;
    use koru_agent_wire::NotificationOperation as Wire;
    Ok(match operation {
        Wire::Inspect => NotificationControl::Inspect,
        Wire::RequestAuthorization => NotificationControl::RequestAuthorization,
        Wire::OpenSettings => NotificationControl::OpenSettings,
        Wire::SetDeliveryEnabled { enabled } => NotificationControl::SetDeliveryEnabled { enabled },
        Wire::Repair => NotificationControl::Repair,
        Wire::SendTest { tone, title, body } => {
            if title.is_empty()
                || title.chars().count() > 256
                || body
                    .as_ref()
                    .is_some_and(|body| body.chars().count() > 1_024)
            {
                return Err((
                    "invalid_notification_test",
                    "notification test copy is outside its bounds",
                ));
            }
            NotificationControl::SendTest {
                tone: notification_tone(tone),
                content: koru_native_notifications::Content { title, body },
            }
        }
    })
}

fn notification_tone(
    tone: koru_agent_wire::NotificationTone,
) -> koru_store_core::reminders::ToneId {
    use koru_agent_wire::NotificationTone as Wire;
    use koru_store_core::reminders::ToneId;
    match tone {
        Wire::System => ToneId::System,
        Wire::KoruDefault => ToneId::KoruDefault,
        Wire::Soft => ToneId::Soft,
        Wire::Chime => ToneId::Chime,
        Wire::Pulse => ToneId::Pulse,
        Wire::Bright => ToneId::Bright,
        Wire::Calm => ToneId::Calm,
        Wire::Drop => ToneId::Drop,
        Wire::Knock => ToneId::Knock,
        Wire::Signal => ToneId::Signal,
        Wire::Urgent => ToneId::Urgent,
    }
}

fn notification_control_result(
    result: crate::reminders::NotificationControlResponse,
) -> koru_agent_wire::NotificationControlResult {
    use crate::reminders::NotificationControlResponse as Worker;
    use koru_agent_wire::NotificationControlResult as Wire;
    match result {
        Worker::Capability(capability) => Wire::Capability {
            authorization: notification_authorization(capability.authorization),
            actions: capability.actions,
            stable_replacement: capability.stable_replacement,
            withdrawal: capability.withdrawal,
            custom_sound: capability.custom_sound,
            activation_while_stopped: capability.activation_while_stopped,
        },
        Worker::Authorization(authorization) => Wire::Authorization {
            authorization: notification_authorization(authorization),
        },
        Worker::Done => Wire::Done,
        Worker::Failed(code) => Wire::Failed {
            code: match code {
                koru_native_notifications::FailureCode::Unsupported => {
                    koru_agent_wire::NotificationFailure::Unsupported
                }
                koru_native_notifications::FailureCode::PermissionDenied => {
                    koru_agent_wire::NotificationFailure::PermissionDenied
                }
                koru_native_notifications::FailureCode::AssetMissing => {
                    koru_agent_wire::NotificationFailure::AssetMissing
                }
                koru_native_notifications::FailureCode::InvalidRequest => {
                    koru_agent_wire::NotificationFailure::InvalidRequest
                }
                koru_native_notifications::FailureCode::Platform => {
                    koru_agent_wire::NotificationFailure::Platform
                }
            },
        },
    }
}

fn notification_authorization(
    authorization: koru_native_notifications::Authorization,
) -> koru_agent_wire::NotificationAuthorization {
    match authorization {
        koru_native_notifications::Authorization::NotDetermined => {
            koru_agent_wire::NotificationAuthorization::NotDetermined
        }
        koru_native_notifications::Authorization::Allowed => {
            koru_agent_wire::NotificationAuthorization::Allowed
        }
        koru_native_notifications::Authorization::Denied => {
            koru_agent_wire::NotificationAuthorization::Denied
        }
        koru_native_notifications::Authorization::Unavailable => {
            koru_agent_wire::NotificationAuthorization::Unavailable
        }
    }
}

/// A calm drag affordance above the query. The whole band is draggable; the
/// centered pill is the visible promise shown in the design.
const DRAG_RAIL_H: f32 = 12.0;
const DRAG_HANDLE_W: f32 = 52.0;
const DRAG_HANDLE_H: f32 = 4.0;
const SEARCH_WIDTH: f32 = 760.0;
const SEARCH_DEFAULT_CONTENT_H: f32 = 480.0;

fn search_min_height() -> f32 {
    koru_window::desktop_preferences::GLOBAL_SEARCH_MIN_HEIGHT as f32 + DRAG_RAIL_H
}

fn search_max_height() -> f32 {
    koru_window::desktop_preferences::GLOBAL_SEARCH_MAX_HEIGHT as f32 + DRAG_RAIL_H
}

/// The dedicated Search window has a fixed width. Corners still count as a
/// generous top/bottom target, but resolve to a vertical resize direction.
fn vertical_resize_direction(
    width: f32,
    height: f32,
    x: f32,
    y: f32,
    maximised: bool,
) -> Option<ResizeDirection> {
    match koru_window::resize::edge(width, height, x, y, maximised) {
        Some(ResizeDirection::NorthWest | ResizeDirection::North | ResizeDirection::NorthEast) => {
            Some(ResizeDirection::North)
        }
        Some(ResizeDirection::SouthWest | ResizeDirection::South | ResizeDirection::SouthEast) => {
            Some(ResizeDirection::South)
        }
        Some(ResizeDirection::West | ResizeDirection::East) | None => None,
    }
}

/// Outermost top strip is always North resize — including over the pill.
/// People aim at the window edge to grow taller; the pill is for moving and
/// lives *below* this band so the two gestures do not fight.
const TOP_RESIZE_BAND: f32 = 8.0;

/// Where the drag-handle pill sits in the top rail.
fn drag_handle_x(width: f32) -> f32 {
    ((width - DRAG_HANDLE_W) * 0.5).max(0.0)
}

fn on_top_resize_edge(y: f32) -> bool {
    y >= 0.0 && y < TOP_RESIZE_BAND
}

fn on_drag_handle(width: f32, x: f32, y: f32) -> bool {
    // Edge wins over the pill so the top-centre of the window still resizes.
    if on_top_resize_edge(y) || y < 0.0 || y >= DRAG_RAIL_H || x < 0.0 || x > width {
        return false;
    }
    let left = drag_handle_x(width);
    // A few pixels past the painted pill — the grip is only 4 px tall and the
    // hit target has to be kinder than the ink.
    let pad = 8.0;
    x >= left - pad && x <= left + DRAG_HANDLE_W + pad
}

fn search_window_height(geometry: &SearchGeometry) -> f32 {
    geometry
        .height()
        .unwrap_or(SEARCH_DEFAULT_CONTENT_H + DRAG_RAIL_H)
        .clamp(search_min_height(), search_max_height())
}

/// Whether the quit's gateway stop has finished.
///
/// **The winit thread still never waits for it** — the stop runs on its own
/// lifecycle thread and the loop leaves the moment the person asked it to. What
/// this exists for is the moment *after* the loop has ended, where there is
/// nobody left to answer and the only thing between the Store and a kill is
/// whether this process is still alive to let it finish. See
/// [`finish_quit_stop`].
static QUIT_STOP: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>> =
    std::sync::Mutex::new(None);

/// How long the process waits, after its loop has ended, for a quit's gateway
/// stop to finish.
///
/// Past the supervisor's ten-second stop deadline on purpose, so both endings
/// fit: the child exits on its own, or it is killed and reaped. In the ordinary
/// case this costs milliseconds — with the event tails closed rather than
/// waited on, a gateway asked to stop is gone almost at once.
const QUIT_STOP_PATIENCE: std::time::Duration = std::time::Duration::from_secs(12);

/// **Let the Store finish, once there is nobody left to answer.**
///
/// Called by `run` after the event loop returns and before this process ends.
/// Without it the quit's stop thread is racing the process exit, and the Job
/// Object wins: the gateway child — and the Store inside it — is killed a few
/// milliseconds into a shutdown it had agreed to. That is not a lost
/// acknowledgement, because an `Ack` is already committed, but it is a
/// database ended by force on every ordinary quit, and it made the stop's
/// truthful outcome unobservable because nothing was left alive to record it.
///
/// Bounded, and the Job Object is still the fallback: a stop that has not
/// finished by [`QUIT_STOP_PATIENCE`] is left behind rather than allowed to
/// hold the process open.
pub fn finish_quit_stop() {
    let waiting = QUIT_STOP.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(waiting) = waiting else { return };
    if waiting.recv_timeout(QUIT_STOP_PATIENCE).is_err() {
        tracing::warn!(
            target: "koru::gateway_host",
            "the gateway had not finished stopping when the Agent left; its job object ends it"
        );
    }
}

/// Where a blocking install-handoff worker leaves its answer for the loop.
///
/// One shared slot rather than a flag, because each of these steps has an
/// outcome the handoff has to branch on — a chat that would not close, a
/// gateway that did not confirm its Store shutdown — and a bare "done" would
/// let the install proceed past a refusal. `None` means the worker has not
/// answered yet; the loop takes the answer exactly once.
type HandoffSlot<T> = std::sync::Arc<std::sync::Mutex<Option<Result<T, String>>>>;

pub struct Agent {
    paths: Paths,
    registry: Registry,
    router: Router,
    routing_log: RoutingLog,
    /// The live connections, by the id the registry keys on. Holding one keeps
    /// its writer thread alive; dropping it closes the socket.
    connections: Vec<Connection>,
    /// The transport-authenticated process id waiting for each connection's
    /// `Hello`. The accept event arrives first; retaining it here is what lets
    /// the registry later authorize that exact shell to come to the front.
    peer_pids: HashMap<ConnectionId, Option<u32>>,
    /// Who this device is, and which run of the agent this is. Held for
    /// [`Request::Status`] to answer with; minted in [`crate::run`], because
    /// the device file is read before the loop exists.
    device_id: String,
    instance_id: String,
    sync_status: koru_agent_wire::SyncStatus,
    /// Private identity stays in Agent memory; only public status crosses IPC.
    sync_identity: Option<koru_device::StoredLocalDevice>,
    sync_control: Option<crate::service::SyncControl>,
    account: Option<crate::account::AccountControl>,
    /// Whether this build has a Koru Cloud (`koru_env::cloud_configured`),
    /// told by `run` before the loop. It decides what an Agent with no
    /// account capability answers: *not configured* rather than the
    /// unavailable state a Cloud that exists and failed to start would give.
    cloud_configured: bool,
    account_generations: HashMap<ConnectionId, u64>,
    /// Hosted-deleted Profiles whose recoverable local cleanup is unfinished.
    /// Loaded before the event loop and replayed on every shell `Hello`.
    pending_profile_cleanup: std::collections::HashSet<String>,
    pairing: Option<koru_pairing::InvitationSession>,
    joining: Option<koru_agent_wire::PairingView>,
    /// The Global settings section, kept current off the event thread. Read for
    /// the caret the overlay draws; the agent never writes a preference.
    settings: koru_api_client::GlobalLink,
    global_consent: Option<koru_voice_consent::Granted>,
    consent_refresh_generation: u64,
    voice_window: crate::voice_window::VoiceWindowHost,
    voice_write: Option<VoiceWrite>,
    voice_retry: Option<VoiceIntent>,
    voice_forced_off: bool,
    voice_problem: Option<koru_voice_ui::Problem>,
    voice_start: Option<VoiceStartStage>,
    voice_open_waiting: bool,
    voice_attempt: Option<VoiceAttemptOrigin>,
    window_transition: Option<WindowTransition>,
    tray_voice_start: bool,
    voice_shortcut_prepare: Option<(u64, String)>,
    overlay: Option<koru_search_ui::SearchSurface>,
    search_geometry: SearchGeometry,
    deferred_open: Option<DeferredOpen>,
    /// An Agents-menu ask made while no Workspace window was connected —
    /// `Some(Some(id))` is a session to show, `Some(None)` is New. Spent by
    /// the next Workspace `Hello`, the `feedback_owed` shape: the window the
    /// ask is addressed to is the one the launch below is producing.
    pending_workspace_session: Option<Option<koru_session_protocol::SessionId>>,
    /// The last session-host catalog the agents-menu watcher read — what the
    /// tray's Agents submenu is projected from. Stored whole so the wake path
    /// only reads; every IPC read happened on the watcher's thread.
    agents_catalog: crate::agents_menu::AgentsCatalog,
    /// The watcher's kick: "the catalog may have moved — read it again".
    /// `None` in direct unit construction, where no watcher runs.
    agents_menu_kick: Option<std::sync::mpsc::SyncSender<()>>,
    /// How a Store supervisor worker wakes the window loop. Absent only in
    /// direct unit-test construction, where starting services is forbidden.
    event_waker: Option<crate::event::Waker>,
    /// Last status reported by the asynchronous Store supervisor.
    store_running: Option<bool>,
    /// Last content-free scheduler state. Native/store work never runs on the
    /// loop; this is only the small result a tray/global surface can inspect.
    reminder_report: crate::reminders::SchedulerReport,
    /// Validated one-shot asks retained until the receiving shell confirms it
    /// queued them. This bridges a notification activation over a cold shell
    /// launch without putting its target on argv or inventing a duration.
    reminder_snoozes: VecDeque<PendingReminderSnooze>,
    /// Profile-specific missed-summary activations awaiting a cold shell.
    reminder_opens_owed: HashSet<ProfileId>,
    /// A tray ask awaiting whichever shell Restore brings back first.
    reminder_tray_open_owed: bool,
    reminder_activation_ingress: Option<crate::reminders::ReminderActivationIngress>,
    reminder_control_ingress: Option<crate::reminders::ReminderControlIngress>,
    /// The updater's one-time token, retained until the matching gateway,
    /// private bridge, and required Store worker have all answered ready.
    update_health_token: Option<String>,
    /// Availability can be reported more than once. Health acknowledgement and
    /// reopen form one startup transition, never repeated by later probes.
    startup_transition_complete: bool,
    /// A health-token restart must wait for the updater to release its
    /// exclusive restart gate before reopening ordinary shells/chat. This is a
    /// maintenance deadline, not a blocking wait on the event loop.
    startup_gate_retry_at: Option<Instant>,
    /// Which chord the hotkey thread actually registered, once it says. Held
    /// here rather than pushed straight at the tray because the tray may not
    /// exist yet when the answer arrives — registration starts before the
    /// window loop does.
    shortcut: crate::tray::Shortcut,
    dictation_shortcut: crate::tray::Shortcut,
    /// Whether the notification-area icon is up, as the windows were last told
    /// ([`Push::NotificationArea`]). Set by the host once the tray exists and
    /// by the tray's own service when its host comes or goes.
    notification_area: bool,
    hotkey_control: Option<crate::hotkey::Control>,
    /// True for ordinary Agents and for a rooted interactive `--tray` launch.
    /// False for headless roots so automated gates claim no OS singleton.
    machine_input_enabled: bool,
    hotkey_generation: u64,
    hotkey_preparing: Option<(u64, String)>,
    /// A prepared chord remains the OS-effective binding until the pump
    /// confirms Commit or Rollback. Retain the requested label so the Voice
    /// surface never calls the old durable chord current in that interval.
    hotkey_settling: Option<(u64, String)>,
    ai_shortcut: bool,
    /// Global dictation lives on its own worker. `None` in unit tests and for
    /// launches that did not opt into machine-wide input.
    global_dictation: Option<Box<dyn GlobalDictationSink>>,
    global_dictation_snapshot: crate::dictation_coordinator::Snapshot,
    /// Correlation for a registered chord currently owned by Search's
    /// existing in-process dictation controller.
    agent_dictation_route: Option<u64>,
    /// Last lifecycle value sent for that route. The ordinary maintenance
    /// drain must not turn a steady Listening state into IPC/winit churn.
    agent_dictation_last_state: Option<crate::dictation_coordinator::LocalState>,
    /// Exact authenticated Workspace connection and Agent-minted token which
    /// may answer the current routed local dictation.
    workspace_dictation_route: Option<(ConnectionId, u64)>,
    /// What the tray's dictation submenu should say.
    ///
    /// **Held rather than re-derived per wake.** Deciding it reads the model
    /// cache off the disk *and* dials a socket, and the loop that would ask is
    /// the winit thread under `ControlFlow::Wait`: a file walk and an IPC round
    /// trip on every agent event are exactly the sort of thing this process is
    /// not allowed to do. It is settled once at construction and re-read when
    /// something voice-shaped happens — a row pressed, a worker warmed, a
    /// release or a stop landing.
    ///
    /// **So it is a reading with a known staleness**, which is the same bargain
    /// the Store status row and the `koru://` row already take: a poll for a
    /// menu nobody has open would be worse than a line somebody has to reopen
    /// the menu to refresh. `tray-icon` offers no "menu is about to open" hook
    /// to do better with.
    voice: crate::tray::VoiceMenu,
    /// Dictation hosted by the Agent's own global-search window. The main Koru
    /// shell has a different controller because it is a different process;
    /// both dial the same root-qualified worker.
    dictation: koru_dictation::Dictation,
    /// The physical key whose release finishes the current push-to-talk hold.
    dictation_key: Option<winit::keyboard::PhysicalKey>,
    /// **A consent step the notification area asked for while no window
    /// existed.** Spent by the next `Hello` — see
    /// [`Self::push_voice_setup_to`], and `push_whats_new_to` beside it for the
    /// same shape and the same reason.
    voice_setup_owed: bool,
    /// Send feedback asked from the tray while no window existed.
    feedback_owed: bool,
    /// **A microphone chosen in the notification area while no window
    /// existed.** Spent by the next `Hello`, like the offer above it. `Some`
    /// carries what to ask for; the inner `None` is *follow the operating
    /// system*, which is why this is an `Option<Option<_>>` rather than a
    /// nullable name -- *nothing owed* and *the system default is owed* are
    /// two different states.
    voice_input_owed: Option<Option<String>>,
    /// Checking, downloading and installing. `None` on a `--root` agent, on a
    /// build with no update feed, and under test — see [`Self::set_event_waker`],
    /// where all three gates are.
    updates: Option<koru_update::UpdateController>,
    /// Local signed manifest to re-offer if the updater returns to this build.
    rehearsal_manifest: Option<std::path::PathBuf>,
    /// The upgrade funnel's dedupe: which offered build was already announced
    /// as `app.update.available` / `.downloaded`. The same offer repeating —
    /// every daily check, every reconnect — is not news.
    update_funnel_available: Option<u64>,
    update_funnel_downloaded: Option<u64>,
    /// Whether the installation directory accepts writes, probed **once** when
    /// updates start rather than per report. `update_report` runs on the event
    /// loop for every push, and the probe is a file created on the install's
    /// disk — which the loop must not wait on (docs/features/auto-update/BUGS.md
    /// issue 1: this exact call, with an fsync it no longer does, held the
    /// loop for 70–157 s). Writability cannot change mid-session short of an
    /// ACL edit under a running install, and `launch_updater` still probes
    /// live at install time, so a stale `true` is caught before anything moves.
    install_writable: bool,
    /// This agent's share of the installation, held so
    /// [`koru_update::InstanceLease::other_instances`] can count the windows it
    /// did *not* start. Dropped before the updater takes its exclusive lock.
    update_lease: Option<koru_update::InstanceLease>,
    /// How far through bringing the stack down an install has got.
    handoff: crate::update::Handoff,
    /// An aborted handoff may already have closed windows/chat/gateway. Wait
    /// for the gateway/Store readiness event before putting that exact subset
    /// back while this same Agent remains alive.
    handoff_recovering: bool,
    /// The windows to put back afterwards, read before they were asked to close.
    reopen: crate::update::Reopen,
    /// Serialize durable reopen mutations off the event loop. Several shells
    /// can reconnect together after an Agent crash; independent read/modify/
    /// writes would resurrect each other's acknowledged entries.
    reopen_io: std::sync::Arc<std::sync::Mutex<()>>,
    /// Exact durable claims returned by live update-reopened shells. Kept by
    /// connection so replay after an Agent-only crash can acknowledge only the
    /// process that won that claim, never an ordinary window on the same
    /// profile/workspace.
    reopen_claims: HashMap<ConnectionId, String>,
    /// A cheap gate that keeps ordinary `Hello` requests from starting a disk
    /// worker when no update transition exists.
    reopen_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Repeated readiness events schedule at most one replay worker.
    reopen_replay_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Result of the blocking Chat exit-and-wait worker.
    chat_stopping: Option<HandoffSlot<bool>>,
    /// Set by the worker that stopped the store, read by the handoff. A slot
    /// rather than an event, because `UpdateChanged` is raised by the update
    /// worker too and could otherwise walk the handoff past this step — and it
    /// carries the *outcome*, because a gateway that did not confirm its Store
    /// shutdown must stop the install rather than only be logged about.
    store_stopping: Option<HandoffSlot<String>>,
    /// Content-free session lifecycle control. All calls happen on workers;
    /// the event loop only reads the last catalog fact.
    session_lifecycle: Option<crate::session_host::Lifecycle>,
    session_catalog: crate::session_host::CatalogFact,
    session_prepare_pending: bool,
    session_catalog_refresh_pending: bool,
    session_catalog_ready_build: Option<u64>,
    session_stopping: bool,
    quit_confirmation_pending: bool,
    quit_after_session_stop: bool,
    /// Whether an update dialog is on screen. Two modals are two competing
    /// decisions even though each is only a worker thread.
    update_dialog_open: bool,
    /// The last report every window was told, so an unchanged one is not sent
    /// again.
    ///
    /// **`UpdateChanged` is a wake, not a change.** The update worker raises it
    /// whenever it looks at anything, and `broadcast_update` used to push
    /// regardless — so a banner sitting at `Ready`, with nothing moving, was
    /// re-sent about **58 times a second** for as long as it was on screen. One
    /// session's log carries 14,315 of them, 3,423 in a single 59-second
    /// stretch, and that is most of what `routing.log` is. An IPC frame and a
    /// log line per frame is the thing PRINCIPLES names outright: it changes
    /// what it measures and buries the two lines that mattered — which is
    /// exactly what happened to the install that failed underneath them.
    ///
    /// One report rather than one per connection, because `broadcast_update`
    /// sends one report to all of them; a window that connects later is caught
    /// by `push_update_to`, which is a different path and always sends.
    last_broadcast_report: Option<koru_agent_wire::UpdateReport>,
    /// Physical keys the loop must swallow until they are released — see
    /// [`Self::quarantine_the_chord`]. Drained by the loop, not by us.
    quarantine: Vec<winit::keyboard::PhysicalKey>,
    /// Ignore `Focused(false)` until this instant. Opening a skip-taskbar
    /// always-on-top panel can lose focus in the same turn as it appears; the
    /// click-away dismiss must not treat that as "click away".
    ignore_blur_until: Option<Instant>,
    exit: bool,
}

/// Which of the two control verbs a tray row asked for.
///
/// A type rather than a `bool`, because *release* and *stop* are not two
/// settings of one switch: one of them leaves voice working and costs the next
/// dictation a model load, the other ends the process. A `bool` parameter at a
/// call site would read as `true` and say nothing about which.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum VoiceVerb {
    Release,
    Stop,
}

/// **Ask the worker on `root` what it is doing, and keep the three facts a menu
/// may show.**
///
/// `None` when nothing is listening, and `None` when the conversation failed:
/// both mean *this menu has nothing true to say about a worker*, and the
/// specific failure belongs in the log rather than on a row.
///
/// The report carries a good deal more — ring overruns, a sample rate, stage
/// timings, sessions this run, the capture device's category — and none of it
/// crosses this function. That is the point of narrowing here rather than
/// storing the report and choosing at the menu: a field that is not held cannot
/// be drawn by somebody adding a line six months from now.
fn voice_worker_facts(root: &std::path::Path) -> Option<crate::tray::VoiceWorker> {
    facts_from(koru_voice_client::ask::status(root))
}

/// **Which capture devices a running worker can see.**
///
/// Empty when nothing is listening, and that is the ordinary answer on a
/// machine nobody has dictated on today -- not a failure and not *this machine
/// has no microphone*. `VoiceClient::devices` refuses rather than starting a
/// worker: drawing a menu is not somebody asking to dictate.
///
/// **The agent does not enumerate for itself**: `cpal` is opened in one
/// process in this workspace, the voice worker, and the whole privacy story
/// rests on it. The agent does not even link it. Asking
/// over the socket is also the *same* answer the window's settings page gets,
/// which is what keeps the two surfaces from disagreeing.
fn voice_devices(root: &std::path::Path) -> Vec<koru_voice::InputDevice> {
    koru_voice_client::VoiceClient::at(Some(root.to_path_buf()))
        .as_product("koru-agent")
        .devices()
        .unwrap_or_default()
}

/// The same narrowing, applied to an answer somebody else asked for.
fn facts_from(asked: koru_voice_client::ask::Asked) -> Option<crate::tray::VoiceWorker> {
    match asked {
        koru_voice_client::ask::Asked::Answered(report) => Some(crate::tray::VoiceWorker {
            state: report.state,
            model_loaded: report.model_loaded,
            // A token on the wire, an `ErrorCode` here: the tray shows a
            // sentence and `ErrorCode::sentence_key` is what chooses it, so a
            // code this build has never heard of is dropped rather than
            // printed. `parse` answers `None` for exactly that.
            last_error: report
                .last_error
                .as_deref()
                .and_then(koru_voice::ErrorCode::parse),
        }),
        koru_voice_client::ask::Asked::NotRunning => None,
        koru_voice_client::ask::Asked::Failed(why) => {
            koru_ipc::ipc_debug!("voice: the worker would not answer: {why}");
            None
        }
    }
}

/// **What the tray's dictation submenu should say**, asked once at start and
/// again whenever something voice-shaped happens.
///
/// **Never under test.** Deciding it walks the model cache, and on a
/// developer's machine that is a real directory outside any temporary root --
/// the `no-reader` lesson in its filesystem form. Nothing is installed on a
/// test root, and that is what a suite is told.
///
/// The worker half is deliberately absent here: `voice_state` reads the disk
/// and nothing else, and the socket is asked separately and off this thread.
/// See [`Agent::read_voice_worker_off_thread`].
fn read_voice_state(
    root: Option<&std::path::Path>,
    input: koru_voice::InputChoice,
) -> crate::tray::VoiceMenu {
    if cfg!(test) {
        return crate::tray::VoiceMenu {
            row: crate::tray::VoiceRow::NotInstalled { megabytes: None },
            installed: Vec::new(),
            worker: None,
            // **Nothing is read under test**, which is the point of this whole
            // branch: walking the cache would touch the developer's own model
            // directory, and enumerating would dial their worker.
            devices: Vec::new(),
            input: koru_voice::InputChoice::SystemDefault,
        };
    }
    crate::tray::voice_state(root, input)
}

/// Whether Agent should start the idle worker solely to make its native input
/// submenu complete. There is nothing useful to warm before models exist, and
/// a switched-off installation must never be dialled at all.
fn warms_voice_input_picker(state: &crate::tray::VoiceMenu) -> bool {
    matches!(&state.row, crate::tray::VoiceRow::Installed)
}

fn system_theme_result(
    appearance: koru_window::Appearance,
    current_dark: bool,
    system_dark: bool,
) -> Option<bool> {
    appearance
        .follows_system()
        .then_some(system_dark)
        .filter(|dark| *dark != current_dark)
}

fn voice_enable_patch(
    first_enable: bool,
    start_on_login_available: bool,
) -> koru_settings::GlobalPatch {
    koru_settings::GlobalPatch {
        global_dictation_enabled: Some(true),
        start_on_login: (first_enable && start_on_login_available).then_some(true),
        ..Default::default()
    }
}

impl Agent {
    pub fn new(paths: Paths, device_id: String, instance_id: String) -> Self {
        // In-memory Agent fixtures must not probe the production reopen file.
        let reopen_pending = !cfg!(test) && crate::update::Reopen::has_record(&paths);
        let routing_log = if cfg!(test) {
            RoutingLog::disabled()
        } else {
            RoutingLog::open(paths.routing_log_file())
        };
        let search_geometry = if cfg!(test) {
            SearchGeometry::disabled()
        } else {
            SearchGeometry::open(paths.window_geometry_file())
        };
        // **Which profile the overlay opens in outlives this process.** The
        // agent is quit at shut-down and replaced by every update, and without
        // this the surface fell back to whatever the catalog listed first each
        // time it came back. Left unset under test, so a suite writes nothing:
        // see `koru_search_ui::persist_choices_in`.
        if !cfg!(test) {
            koru_search_ui::persist_choices_in(paths.search_state_file());
        }
        // **The settings authority, opened once and read on every overlay.**
        // The agent used to parse `preferences.conf` here for its caret, on
        // the event thread, every time the overlay opened. It is a separate
        // process from the shell and cannot see the shell's globals, so the
        // authority is what the two now agree through. Disabled under test:
        // a suite must not reach a Gateway (ADR-266, and TESTING.md's rule
        // about never asserting that a connection fails).
        let settings = if cfg!(test) {
            koru_api_client::GlobalLink::disabled()
        } else {
            koru_api_client::GlobalLink::global(
                crate::segment().to_owned(),
                paths.explicit_root().map(std::path::Path::to_path_buf),
            )
        };
        if !cfg!(test) {
            koru_window::Appearance::from_section(settings.authoritative()).apply();
        }
        let voice = read_voice_state(
            paths.explicit_root(),
            koru_dictation::Preferences::from_section(settings.authoritative()).input,
        );
        let mut dictation =
            koru_dictation::Dictation::at(Some(crate::service::voice_service_root(&paths)));
        let global_consent = if !cfg!(test) {
            koru_dictation::Preferences::from_section(settings.authoritative())
                .apply_to(&mut dictation);
            let consent = paths
                .voice_consent_file()
                .map(|path| koru_voice_consent::read(&path))
                .and_then(|consent| consent.granted().cloned());
            dictation.set_consent(consent.clone());
            consent
        } else {
            None
        };
        routing_log.event(format_args!(
            "agent started protocol={} channel={}",
            koru_agent_wire::PROTOCOL_VERSION,
            crate::segment()
        ));
        Self {
            paths,
            voice_setup_owed: false,
            feedback_owed: false,
            voice_input_owed: None,
            registry: Registry::new(),
            router: Router::new(),
            routing_log,
            connections: Vec::new(),
            peer_pids: HashMap::new(),
            device_id,
            instance_id,
            sync_status: koru_agent_wire::SyncStatus::default(),
            sync_identity: None,
            sync_control: None,
            account: None,
            cloud_configured: true,
            account_generations: HashMap::new(),
            pending_profile_cleanup: std::collections::HashSet::new(),
            pairing: None,
            joining: None,
            settings,
            global_consent,
            consent_refresh_generation: 0,
            voice_window: crate::voice_window::VoiceWindowHost::default(),
            voice_write: None,
            voice_retry: None,
            voice_forced_off: false,
            voice_problem: None,
            voice_start: None,
            voice_open_waiting: false,
            voice_attempt: None,
            window_transition: None,
            tray_voice_start: false,
            voice_shortcut_prepare: None,
            overlay: None,
            search_geometry,
            deferred_open: None,
            pending_workspace_session: None,
            agents_catalog: crate::agents_menu::AgentsCatalog::default(),
            agents_menu_kick: None,
            event_waker: None,
            store_running: None,
            reminder_report: crate::reminders::SchedulerReport::default(),
            reminder_snoozes: VecDeque::new(),
            reminder_opens_owed: HashSet::new(),
            reminder_tray_open_owed: false,
            reminder_activation_ingress: None,
            reminder_control_ingress: None,
            update_health_token: None,
            startup_transition_complete: false,
            startup_gate_retry_at: None,
            shortcut: crate::tray::Shortcut::Unknown,
            dictation_shortcut: crate::tray::Shortcut::Unknown,
            notification_area: false,
            hotkey_control: None,
            machine_input_enabled: false,
            hotkey_generation: 0,
            hotkey_preparing: None,
            hotkey_settling: None,
            ai_shortcut: false,
            global_dictation: None,
            global_dictation_snapshot: crate::dictation_coordinator::Snapshot::default(),
            agent_dictation_route: None,
            agent_dictation_last_state: None,
            workspace_dictation_route: None,
            voice,
            dictation,
            dictation_key: None,
            updates: None,
            rehearsal_manifest: None,
            install_writable: false,
            update_lease: None,
            handoff: crate::update::Handoff::Idle,
            handoff_recovering: false,
            reopen: crate::update::Reopen::default(),
            reopen_io: std::sync::Arc::new(std::sync::Mutex::new(())),
            reopen_claims: HashMap::new(),
            reopen_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(reopen_pending)),
            reopen_replay_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            chat_stopping: None,
            store_stopping: None,
            session_lifecycle: None,
            session_catalog: crate::session_host::CatalogFact {
                generation: 0,
                active_count: 0,
            },
            session_prepare_pending: false,
            session_catalog_refresh_pending: false,
            session_catalog_ready_build: None,
            session_stopping: false,
            quit_confirmation_pending: false,
            quit_after_session_stop: false,
            update_dialog_open: false,
            update_funnel_available: None,
            update_funnel_downloaded: None,
            last_broadcast_report: None,
            quarantine: Vec::new(),
            ignore_blur_until: None,
            exit: false,
        }
    }

    /// Install the agents-menu watcher's kick — see [`crate::agents_menu`].
    pub fn set_agents_menu_kick(&mut self, kick: std::sync::mpsc::SyncSender<()>) {
        self.agents_menu_kick = Some(kick);
    }

    /// Say the session catalog may have moved. `try_send`, coalesced: two
    /// kicks owe the same single re-read, and no watcher is no menu to stale.
    fn kick_agents_menu(&self) {
        if let Some(kick) = &self.agents_menu_kick {
            crate::agents_menu::nudge(kick);
        }
    }

    pub fn set_session_lifecycle(
        &mut self,
        lifecycle: crate::session_host::Lifecycle,
        catalog: crate::session_host::CatalogFact,
    ) {
        self.session_lifecycle = Some(lifecycle);
        self.session_catalog = catalog;
    }

    pub fn set_account_control(&mut self, control: crate::account::AccountControl) {
        self.account = Some(control);
    }

    /// Record whether this build has a Koru Cloud. Without one there is never
    /// an account capability, and every account request is answered with
    /// [`koru_agent_wire::AccountState::not_configured`] — locally, with no
    /// network attempt.
    pub fn set_cloud_configured(&mut self, configured: bool) {
        self.cloud_configured = configured;
    }

    /// Re-apply every Profile's remembered Cloud sync choice.
    ///
    /// `InspectCloudSync` *is* the reconcile: it reports the remembered state
    /// and, for a Profile that is switched on, activates any Workspace that
    /// has appeared since the switch was answered. Running it on the Cloud
    /// heartbeat is what makes a Workspace created while the settings surface
    /// is closed reach the Cloud within the minute, instead of waiting for
    /// somebody to open the switch again.
    ///
    /// It is the Agent's own question, not a connection's: requester `0` is the
    /// worker's internal requester and `request_id: 0` marks the answer as
    /// unsolicited. It stands aside whenever anything is already waiting for
    /// admission — a background nudge on a timer must never spend the slot a
    /// person's next command needs, and skipping a pass costs at most the next
    /// thirty seconds.
    pub(crate) fn reconcile_cloud_sync(&self) -> bool {
        let Some(control) = &self.account else {
            return false;
        };
        if control.state().stage != koru_agent_wire::AccountStage::SignedIn
            || control.queued_commands() > 0
        {
            return false;
        }
        control
            .enqueue(
                0,
                koru_agent_wire::AccountCommand::InspectCloudSync { request_id: 0 },
            )
            .is_ok()
    }

    pub(crate) fn set_update_health_token(&mut self, token: Option<String>) {
        self.update_health_token = token;
    }

    pub(crate) fn set_update_lease(&mut self, lease: Option<koru_update::InstanceLease>) {
        self.update_lease = lease;
    }

    pub(crate) fn set_rehearsal_updates(
        &mut self,
        controller: koru_update::UpdateController,
        manifest: std::path::PathBuf,
    ) {
        self.updates = Some(controller);
        self.rehearsal_manifest = Some(manifest);
    }

    pub fn set_sync_identity(
        &mut self,
        identity: koru_device::StoredLocalDevice,
        trusted_device_count: usize,
        authorization_generation: u64,
    ) {
        self.sync_status = koru_agent_wire::SyncStatus {
            endpoint_id: Some(identity.descriptor.endpoint_id.to_storage()),
            sync_ready: true,
            sync_error: None,
            trusted_device_count,
            authorization_generation,
        };
        self.sync_identity = Some(identity);
        if let Err(error) = self.advertise_build(&crate::build_stamp::payload()) {
            // A device that cannot describe itself still syncs; the list
            // shows no build for it, which is the honest state.
            self.routing_log
                .event(format_args!("build advertise failed error={error}"));
        }
    }

    /// Sign and store this device's build description, unless the latest one
    /// it signed already says exactly this — so a launch on an unchanged
    /// build writes nothing. Otherwise it is a rename's path: the revision
    /// bump carries it to every peer, and a build alters nothing the data
    /// plane checks, so no connection is rebuilt for it.
    pub fn advertise_build(&mut self, payload: &koru_device::BuildPayload) -> Result<(), String> {
        use koru_device::TrustRepository as _;
        let Some(identity) = self.sync_identity.as_ref() else {
            return Err("sync identity is not available".into());
        };
        let mut repository =
            crate::identity::SqliteTrustRepository::open(&self.paths.identity_db())
                .map_err(|error| error.to_string())?;
        let view = repository.trust_view().map_err(|error| error.to_string())?;
        let device = identity.descriptor.device_id;
        if view.build_is_current(device, payload) {
            return Ok(());
        }
        let record = koru_device::DeviceBuildV1::sign(
            koru_device::RecordId::new(),
            device,
            payload,
            wall_time_millis(),
            &identity.signing_key,
        );
        let verified =
            koru_device::verify_gossip_record(&view, koru_device::TrustRecord::DeviceBuild(record))
                .map_err(|error| error.to_string())?;
        repository
            .apply(verified)
            .map_err(|error| error.to_string())?;
        self.routing_log.event(format_args!(
            "build advertised build={} os_version={} commit={}",
            payload.label(),
            payload.os_version.as_deref().unwrap_or("-"),
            payload.commit.as_deref().unwrap_or("-"),
        ));
        self.refresh_trust_status()
    }

    pub fn set_sync_unavailable(&mut self, code: &str) {
        self.sync_status.sync_ready = false;
        self.sync_status.sync_error = Some(code.to_owned());
        self.sync_identity = None;
    }

    pub fn set_sync_control(&mut self, control: crate::service::SyncControl) {
        self.sync_control = Some(control);
    }

    pub fn sync_identity(&self) -> Option<&koru_device::StoredLocalDevice> {
        self.sync_identity.as_ref()
    }

    fn pairing_response(&self) -> Response {
        use koru_agent_wire::{PairingState, PairingView};
        if let Some(view) = &self.joining {
            return Response::Pairing {
                view: Box::new(view.clone()),
            };
        }
        let Some(pairing) = &self.pairing else {
            return Response::Pairing {
                view: Box::new(PairingView {
                    state: PairingState::Idle,
                    qr_payload: None,
                    expires_in_seconds: 0,
                    peer_name: None,
                    error_code: None,
                }),
            };
        };
        let state = match pairing.stage(std::time::Instant::now()) {
            koru_pairing::PairingStage::Idle => PairingState::Idle,
            koru_pairing::PairingStage::Displaying => PairingState::Displaying,
            koru_pairing::PairingStage::Connecting => PairingState::Connecting,
            koru_pairing::PairingStage::Confirming => PairingState::Confirming,
            koru_pairing::PairingStage::Preparing => PairingState::Preparing,
            koru_pairing::PairingStage::Finishing => PairingState::Finishing,
            koru_pairing::PairingStage::Succeeded => PairingState::Succeeded,
            koru_pairing::PairingStage::Expired => PairingState::Expired,
            koru_pairing::PairingStage::Cancelled => PairingState::Cancelled,
            koru_pairing::PairingStage::Failed => PairingState::Failed,
        };
        Response::Pairing {
            view: Box::new(PairingView {
                state,
                qr_payload: matches!(state, PairingState::Displaying)
                    .then(|| pairing.invitation.encode_link_url().ok())
                    .flatten(),
                expires_in_seconds: if matches!(state, PairingState::Displaying) {
                    pairing.invitation.expires_in_seconds
                } else {
                    0
                },
                peer_name: None,
                error_code: None,
            }),
        }
    }

    fn linked_devices_response(&self) -> Response {
        let repository =
            match crate::identity::SqliteTrustRepository::open(&self.paths.identity_db()) {
                Ok(repository) => repository,
                Err(error) => {
                    return Response::Failed {
                        code: error.code.into(),
                        message: error.detail,
                    };
                }
            };
        use koru_device::TrustRepository as _;
        let view = match repository.trust_view() {
            Ok(view) => view,
            Err(error) => {
                return Response::Failed {
                    code: error.code.into(),
                    message: error.detail,
                };
            }
        };
        Response::LinkedDevices {
            devices: linked_devices(
                &view,
                self.sync_control
                    .as_ref()
                    .map(crate::service::SyncControl::status)
                    .as_ref(),
            ),
            this_device: this_device(&view).map(Box::new),
            relay: relay_view().map(Box::new),
        }
    }

    fn device_history_response(&self, device_id: &str, before: Option<i64>) -> Response {
        let empty = || Response::DeviceHistory {
            device_id: device_id.to_owned(),
            events: Vec::new(),
            next_before: None,
        };
        let Some(peer) = koru_device::DeviceId::parse(device_id) else {
            return empty();
        };
        let Some(control) = &self.sync_control else {
            return empty();
        };
        match control.state_history(peer, before) {
            Ok(koru_sync_control::ControlResponse::StateHistory {
                events,
                next_before,
                ..
            }) => Response::DeviceHistory {
                device_id: device_id.to_owned(),
                events: events.into_iter().map(device_state_event).collect(),
                next_before,
            },
            Ok(koru_sync_control::ControlResponse::Rejected { .. }) | Err(_) => empty(),
            Ok(_) => empty(),
        }
    }

    /// Rename this device: a self-signed record that gossips mesh-wide, plus
    /// the local descriptor so the next pairing offers the current name.
    fn rename_device(&mut self, value: &str) -> Response {
        use koru_device::TrustRepository as _;
        let name = value.trim();
        if name.is_empty() {
            return Response::Failed {
                code: "empty_name".into(),
                message: "a device name cannot be empty".into(),
            };
        }
        if name.chars().count() > koru_device::MAX_DEVICE_NAME_CHARS {
            return Response::Failed {
                code: "name_too_long".into(),
                message: format!(
                    "a device name is at most {} characters",
                    koru_device::MAX_DEVICE_NAME_CHARS
                ),
            };
        }
        let Some(identity) = self.sync_identity.as_ref() else {
            return Response::Failed {
                code: "identity_unavailable".into(),
                message: "sync identity is not available".into(),
            };
        };
        let mut repository =
            match crate::identity::SqliteTrustRepository::open(&self.paths.identity_db()) {
                Ok(repository) => repository,
                Err(error) => {
                    return Response::Failed {
                        code: error.code.into(),
                        message: error.detail,
                    };
                }
            };
        let record = koru_device::DeviceNameV1::sign(
            koru_device::RecordId::new(),
            identity.descriptor.device_id,
            name.to_owned(),
            wall_time_millis(),
            &identity.signing_key,
        );
        let result = (|| -> Result<(), String> {
            let view = repository.trust_view().map_err(|error| error.to_string())?;
            let verified = koru_device::verify_gossip_record(
                &view,
                koru_device::TrustRecord::DeviceName(record),
            )
            .map_err(|error| error.to_string())?;
            repository
                .apply(verified)
                .map_err(|error| error.to_string())?;
            repository
                .set_local_display_name(name)
                .map_err(|error| error.to_string())?;
            // The revision bump alone carries the rename: the sync-control
            // loop gossips on revision change, and a name alters nothing the
            // data plane checks, so no connection is rebuilt for it.
            self.refresh_trust_status()
        })();
        match result {
            Ok(()) => self.linked_devices_response(),
            Err(error) => Response::Failed {
                code: "rename_device".into(),
                message: error,
            },
        }
    }

    fn broadcast_pairing(&mut self) {
        let Response::Pairing { view } = self.pairing_response() else {
            return;
        };
        let ids: Vec<_> = self.connections.iter().map(Connection::id).collect();
        for id in ids {
            self.push(id, Push::Pairing { view: view.clone() });
        }
    }

    fn broadcast_profile_lifecycle_deleted(&mut self, profile_id: String) {
        self.pending_profile_cleanup.insert(profile_id.clone());
        let ids: Vec<_> = self.connections.iter().map(Connection::id).collect();
        for id in ids {
            self.push(
                id,
                Push::ProfileLifecycleDeleted {
                    profile_id: profile_id.clone(),
                },
            );
        }
    }

    pub(crate) fn set_pending_profile_cleanup(&mut self, profiles: Vec<String>) {
        self.pending_profile_cleanup = profiles.into_iter().collect();
    }

    fn push_pending_profile_cleanup_to(&mut self, connection: ConnectionId) {
        let mut profiles = self
            .pending_profile_cleanup
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        profiles.sort();
        for profile_id in profiles {
            self.push(connection, Push::ProfileLifecycleDeleted { profile_id });
        }
    }

    fn refresh_trust_status(&mut self) -> Result<(), String> {
        use koru_device::TrustRepository as _;
        let repository = crate::identity::SqliteTrustRepository::open(&self.paths.identity_db())
            .map_err(|error| error.to_string())?;
        let view = repository.trust_view().map_err(|error| error.to_string())?;
        self.sync_status.trusted_device_count = linked_devices(&view, None).len();
        self.sync_status.authorization_generation = view.effective_revision;
        Ok(())
    }

    fn commit_pairing(&mut self, prepared: &koru_pairing::PreparedPairingV1) -> Result<(), String> {
        let local = self
            .sync_identity
            .as_ref()
            .ok_or_else(|| "sync identity is unavailable".to_owned())?
            .descriptor
            .device_id;
        let mut repository =
            crate::identity::SqliteTrustRepository::open(&self.paths.identity_db())
                .map_err(|error| error.to_string())?;
        repository
            .commit_pairing(prepared, local)
            .map_err(|error| error.to_string())?;
        self.refresh_trust_status()
    }

    fn settle_pairing_events(&mut self) {
        let Some(control) = self.sync_control.clone() else {
            return;
        };
        for event in control.take_pairing_events() {
            match event {
                koru_sync_control::PairingEvent::JoinRequested { attempt_id, join } => {
                    let result = (|| {
                        let identity = self
                            .sync_identity
                            .as_ref()
                            .ok_or_else(|| "identity_unavailable".to_owned())?;
                        let session = self
                            .pairing
                            .as_mut()
                            .ok_or_else(|| "invitation_unavailable".to_owned())?;
                        session
                            .claim(
                                &join.secret,
                                join.transcript.joiner.device_id,
                                std::time::Instant::now(),
                            )
                            .map_err(|error| format!("{error:?}"))?;
                        session.preparing();
                        join.complete(&session.invitation, &identity.signing_key)
                            .map_err(|error| format!("{error:?}"))
                    })();
                    match result {
                        Ok(prepared) => {
                            if self.commit_pairing(&prepared).is_ok() {
                                control.complete_pairing(attempt_id, prepared);
                                if let Some(session) = &mut self.pairing {
                                    session.succeed();
                                }
                            } else if let Some(session) = &mut self.pairing {
                                session.fail();
                            }
                        }
                        Err(_) => {
                            if let Some(session) = &mut self.pairing {
                                session.fail();
                            }
                        }
                    }
                }
                koru_sync_control::PairingEvent::Completed { prepared } => {
                    let peer_name = prepared.transcript.inviter.display_name.clone();
                    let (state, error_code) = if self.commit_pairing(&prepared).is_ok() {
                        (koru_agent_wire::PairingState::Succeeded, None)
                    } else {
                        (
                            koru_agent_wire::PairingState::Failed,
                            Some("pairing_commit".into()),
                        )
                    };
                    self.joining = Some(koru_agent_wire::PairingView {
                        state,
                        qr_payload: None,
                        expires_in_seconds: 0,
                        peer_name: Some(peer_name),
                        error_code,
                    });
                }
                koru_sync_control::PairingEvent::Failed { code } => {
                    if let Some(joining) = &mut self.joining {
                        joining.state = koru_agent_wire::PairingState::Failed;
                        joining.error_code = Some(code);
                    } else if let Some(session) = &mut self.pairing {
                        session.fail();
                    }
                }
                koru_sync_control::PairingEvent::TrustRecordReceived {
                    delivery_id,
                    from,
                    record,
                } => {
                    let accepted = self.apply_remote_trust_record(from, *record).is_ok();
                    control.complete_trust_record(delivery_id, accepted);
                    if accepted {
                        control.refresh_authorization();
                    }
                }
                koru_sync_control::PairingEvent::TrustRecordsReceived {
                    delivery_id,
                    from,
                    records,
                } => {
                    let accepted = self.apply_remote_trust_records(from, records).is_ok();
                    control.complete_trust_record(delivery_id, accepted);
                    if accepted {
                        control.refresh_authorization();
                    }
                }
            }
        }
        self.broadcast_pairing();
    }

    fn apply_remote_trust_record(
        &mut self,
        from: koru_device::DeviceId,
        record: koru_device::TrustRecord,
    ) -> Result<(), String> {
        use koru_device::TrustRepository as _;
        let local = self
            .sync_identity
            .as_ref()
            .ok_or_else(|| "identity_unavailable".to_owned())?
            .descriptor
            .device_id;
        // Singular delivery carries an urgent fact signed by this peer:
        // unlinking this device or changing one exact Profile lifecycle. Mesh
        // gossip travels the batch path below, which is third-party by design.
        match &record {
            koru_device::TrustRecord::Revocation(revocation)
                if revocation.issued_by == from && revocation.revoked_device_id == local => {}
            koru_device::TrustRecord::ProfileLifecycle(lifecycle)
                if lifecycle.issued_by == from => {}
            _ => return Err("unexpected_trust_record".into()),
        }
        let mut repository =
            crate::identity::SqliteTrustRepository::open(&self.paths.identity_db())
                .map_err(|error| error.to_string())?;
        let view = repository.trust_view().map_err(|error| error.to_string())?;
        let lifecycle = match &record {
            koru_device::TrustRecord::ProfileLifecycle(value) => Some(value.clone()),
            _ => None,
        };
        let verified =
            koru_device::verify_gossip_record(&view, record).map_err(|error| error.to_string())?;
        let outcome = repository
            .apply(verified)
            .map_err(|error| error.to_string())?;
        if !matches!(outcome, koru_device::ApplyOutcome::Stale { .. })
            && let Some(lifecycle) = lifecycle
        {
            crate::identity::profile_lifecycle::observe_received(&self.paths, &lifecycle)
                .map_err(str::to_owned)?;
            if lifecycle.state == koru_device::ProfileLifecycleStateV1::Deleted {
                self.broadcast_profile_lifecycle_deleted(lifecycle.profile_id.to_storage());
            }
        }
        self.refresh_trust_status()
    }

    /// Apply a gossiped device-list batch. Every record is verified against
    /// this device's own trust view — never against the sender's say-so — in
    /// canonical order, so a batch is verifiable in one pass. A record that
    /// fails is skipped rather than poisoning the rest; the sender's
    /// anti-entropy beat re-tries it.
    fn apply_remote_trust_records(
        &mut self,
        from: koru_device::DeviceId,
        mut records: Vec<koru_device::TrustRecord>,
    ) -> Result<(), String> {
        use koru_device::TrustRepository as _;
        if self.sync_identity.is_none() {
            return Err("identity_unavailable".to_owned());
        }
        let mut repository =
            crate::identity::SqliteTrustRepository::open(&self.paths.identity_db())
                .map_err(|error| error.to_string())?;
        koru_device::sort_for_gossip(&mut records);
        let total = records.len();
        let mut applied = 0usize;
        let mut rejected = 0usize;
        let mut skipped = 0usize;
        let mut deleted_profiles = Vec::new();
        for record in records {
            // Re-read per record: an applied relationship is what the next
            // grant or revocation in the same batch verifies against.
            let view = repository.trust_view().map_err(|error| error.to_string())?;
            let lifecycle = match &record {
                koru_device::TrustRecord::ProfileLifecycle(value) => Some(value.clone()),
                _ => None,
            };
            match koru_device::verify_gossip_record(&view, record) {
                Ok(verified) => match repository.apply(verified) {
                    Ok(koru_device::ApplyOutcome::Stale { .. }) => applied += 1,
                    Ok(_) => {
                        let observed = lifecycle.as_ref().is_none_or(|lifecycle| {
                            crate::identity::profile_lifecycle::observe_received(
                                &self.paths,
                                lifecycle,
                            )
                            .is_ok()
                        });
                        if observed {
                            if let Some(lifecycle) = lifecycle
                                && lifecycle.state == koru_device::ProfileLifecycleStateV1::Deleted
                            {
                                deleted_profiles.push(lifecycle.profile_id.to_storage());
                            }
                            applied += 1;
                        } else {
                            rejected += 1;
                        }
                    }
                    Err(_) => rejected += 1,
                },
                // A kind this build does not know comes from a newer device.
                // Skipped, not rejected: the batch is still acknowledged, or
                // the sender would offer it again forever.
                Err(koru_device::GossipRejection::UnknownKind) => skipped += 1,
                Err(_) => rejected += 1,
            }
        }
        koru_store_client::debug::write(format_args!(
            "trust gossip from {}: applied={} rejected={} skipped_unknown_kind={} total={}",
            from.to_storage(),
            applied,
            rejected,
            skipped,
            total
        ));
        deleted_profiles.sort();
        deleted_profiles.dedup();
        for profile_id in deleted_profiles {
            self.broadcast_profile_lifecycle_deleted(profile_id);
        }
        self.refresh_trust_status()?;
        (rejected == 0)
            .then_some(())
            .ok_or_else(|| "rejected_trust_records".to_owned())
    }

    fn unlink_device(&mut self, value: &str) -> Response {
        use koru_device::TrustRepository as _;
        let Some(device_id) = koru_device::DeviceId::parse(value) else {
            return Response::Failed {
                code: "bad_device_id".into(),
                message: "the linked device id is not a uuid".into(),
            };
        };
        let Some(identity) = self.sync_identity.as_ref() else {
            return Response::Failed {
                code: "identity_unavailable".into(),
                message: "sync identity is not available".into(),
            };
        };
        let mut repository =
            match crate::identity::SqliteTrustRepository::open(&self.paths.identity_db()) {
                Ok(repository) => repository,
                Err(error) => {
                    return Response::Failed {
                        code: error.code.into(),
                        message: error.detail,
                    };
                }
            };
        let before = match repository.trust_view() {
            Ok(view) => view,
            Err(error) => {
                return Response::Failed {
                    code: error.code.into(),
                    message: error.detail,
                };
            }
        };
        let revoked: std::collections::HashSet<_> = before
            .revocations
            .iter()
            .map(|record| record.relationship_id)
            .collect();
        let local = identity.descriptor.device_id;
        let relationships: Vec<_> = before
            .relationships
            .iter()
            .filter(|relationship| {
                !revoked.contains(&relationship.relationship_id)
                    && ((relationship.inviter.device_id == local
                        && relationship.joiner.device_id == device_id)
                        || (relationship.joiner.device_id == local
                            && relationship.inviter.device_id == device_id))
            })
            .map(|relationship| relationship.relationship_id)
            .collect();
        if relationships.is_empty() {
            return Response::Failed {
                code: "device_not_linked".into(),
                message: "the device has no active trust relationship".into(),
            };
        }
        let issued_at_millis = wall_time_millis();
        for relationship_id in relationships {
            let record = koru_device::RevocationV1::sign(
                relationship_id,
                device_id,
                local,
                koru_device::RecordId::new(),
                issued_at_millis,
                &identity.signing_key,
            );
            let verified = match record
                .clone()
                .verify(identity.descriptor.signing_public_key)
            {
                Ok(verified) => verified,
                Err(error) => {
                    return Response::Failed {
                        code: "invalid_revocation".into(),
                        message: error.to_string(),
                    };
                }
            };
            if let Err(error) = repository.apply(verified) {
                return Response::Failed {
                    code: error.code.into(),
                    message: error.detail,
                };
            }
            if let Some(control) = &self.sync_control {
                control.send_trust_record(device_id, koru_device::TrustRecord::Revocation(record));
            }
        }
        let after = match repository.trust_view() {
            Ok(view) => view,
            Err(error) => {
                return Response::Failed {
                    code: error.code.into(),
                    message: error.detail,
                };
            }
        };
        self.sync_status.trusted_device_count = linked_devices(&after, None).len();
        self.sync_status.authorization_generation = after.effective_revision;
        Response::LinkedDevices {
            devices: linked_devices(
                &after,
                self.sync_control
                    .as_ref()
                    .map(crate::service::SyncControl::status)
                    .as_ref(),
            ),
            this_device: this_device(&after).map(Box::new),
            relay: relay_view().map(Box::new),
        }
    }

    /// Take a connection the accept thread has welcomed.
    ///
    /// Handed over *before* `ShellArrived` is raised, so the loop never meets an
    /// id it has no connection for.
    pub fn adopt(&mut self, connection: Connection) {
        self.routing_log.event(format_args!(
            "transport adopted connection={} connections_before={}",
            connection.id().0,
            self.connections.len()
        ));
        self.connections.push(connection);
    }

    pub(crate) fn set_reminder_activation_ingress(
        &mut self,
        ingress: crate::reminders::ReminderActivationIngress,
    ) {
        self.reminder_activation_ingress = Some(ingress);
    }

    pub(crate) fn set_reminder_control_ingress(
        &mut self,
        ingress: crate::reminders::ReminderControlIngress,
    ) {
        self.reminder_control_ingress = Some(ingress);
    }

    pub(crate) fn set_event_waker(&mut self, waker: crate::event::Waker) {
        self.event_waker = Some(waker.clone());
        let settings_waker = waker.clone();
        self.settings
            .on_update(move || settings_waker.wake(AgentEvent::SettingsChanged));
        self.apply_appearance();
        if !cfg!(test) && self.machine_input_enabled && self.global_dictation.is_none() {
            self.global_dictation = Some(Box::new(crate::dictation_coordinator::Handle::spawn(
                self.paths.clone(),
                waker.clone(),
            )));
        }
        self.apply_global_dictation_settings();
        let voice_waker = waker.clone();
        self.dictation
            .client()
            .install_wake(move || voice_waker.wake(AgentEvent::Dictation));
        let voice_paths = self.paths.clone();
        let voice_read_waker = waker.clone();
        self.dictation.client().install_ensure(move || {
            let paths = voice_paths.clone();
            let waker = voice_read_waker.clone();
            let _ = std::thread::Builder::new()
                .name("koru-agent ensure dictation".into())
                .spawn(move || {
                    match service::ensure_voice(&paths) {
                        Ok(_) => {
                            let root = paths.store_dir();
                            waker.wake(AgentEvent::VoiceWorkerRead {
                                worker: voice_worker_facts(&root),
                                devices: voice_devices(&root),
                            });
                        }
                        Err(error) => {
                            tracing::warn!(target: "koru::voice", %error, "global search could not ensure the voice worker");
                        }
                    }
                });
        });
        // An explicit root is an isolated developer/test instance: it owns no
        // machine-wide UI, including an update prompt that could appear over
        // an unattended lifecycle gate. The installed, machine-owning Agent is
        // the one that supervises replacement of the installed gateway image.
        if self.paths.owns_the_machine() {
            self.start_updates(&waker);
        } else if let Some(controller) = self.updates.clone() {
            // The only rooted controller is installed explicitly by
            // `--rehearse-update`. It has a local package source and must never
            // start the automatic feed loop.
            self.attach_updates(controller, &waker, false);
        }
        // Establish the tray's live status without putting discovery or a
        // process launch on the window thread. This is also the Agent actually
        // supervising the Store from startup, rather than only after Search.
        self.ensure_gateway_off_thread(None);
        // **And make an installed input picker complete.** Device enumeration
        // belongs to the worker, so a fresh Agent with no worker could only
        // offer *System default*. Warm the worker when voice is installed and
        // read its inputs after it binds. This opens no microphone and loads no
        // model; it pays only for the idle helper process. Without installed
        // models there is nothing to warm, so retain the read-only adoption
        // path and start no process.
        if warms_voice_input_picker(&self.voice) {
            self.ensure_voice_off_thread(None);
        } else {
            self.read_voice_worker_off_thread();
        }
    }

    pub(crate) fn set_hotkey_control(&mut self, control: crate::hotkey::Control) {
        self.hotkey_control = Some(control);
        self.apply_global_dictation_settings();
    }

    pub(crate) fn set_machine_input_enabled(&mut self, enabled: bool) {
        self.machine_input_enabled = enabled;
    }

    fn apply_global_dictation_settings(&mut self) {
        let section = self.settings.authoritative();
        if let Some(coordinator) = &self.global_dictation {
            coordinator.configure(crate::dictation_coordinator::Policy {
                enabled: section.global_dictation_enabled.unwrap_or(false)
                    && !self.voice_forced_off,
                preferences: koru_dictation::Preferences::from_section(section),
                consent: self.global_consent.clone(),
            });
        }
        let voice_owns_shortcut = self.voice_shortcut_prepare.is_some()
            || self
                .voice_write
                .as_ref()
                .is_some_and(|write| matches!(write.intent, VoiceIntent::Shortcut(_, _)))
            || self.hotkey_settling.is_some();
        if voice_owns_shortcut {
            return;
        }
        let wanted = section.global_dictation_shortcut.as_str();
        let already = matches!(
            &self.dictation_shortcut,
            crate::tray::Shortcut::Chord(active) if active == wanted
        );
        if !already
            && self
                .hotkey_preparing
                .as_ref()
                .is_none_or(|(_, pending)| pending != wanted)
            && let Some(control) = &self.hotkey_control
        {
            self.hotkey_generation = self.hotkey_generation.wrapping_add(1);
            let generation = self.hotkey_generation;
            if control.prepare_dictation(generation, wanted) {
                self.hotkey_preparing = Some((generation, wanted.to_owned()));
            }
        }
    }

    fn apply_appearance(&self) {
        if !cfg!(test) {
            koru_window::Appearance::from_section(self.settings.authoritative()).apply();
        }
    }

    pub(crate) fn system_theme_changed(&mut self) -> bool {
        let appearance = koru_window::Appearance::from_section(self.settings.authoritative());
        let Some(dark) = system_theme_result(
            appearance,
            theme::is_dark(),
            koru_window::system_theme::system_is_dark(),
        ) else {
            return false;
        };
        theme::set_dark(dark);
        true
    }

    fn voice_model(&self) -> koru_voice_ui::ViewModel {
        let section = self.settings.authoritative();
        let saved = match section.global_dictation_enabled {
            None => koru_voice_ui::Saved::NeverEnabled,
            Some(false) => koru_voice_ui::Saved::Disabled,
            Some(true) => koru_voice_ui::Saved::Enabled,
        };
        let pending = self
            .voice_write
            .as_ref()
            .map(|write| match write.intent {
                VoiceIntent::Enable => koru_voice_ui::Pending::Enable,
                VoiceIntent::Disable => koru_voice_ui::Pending::Disable,
                VoiceIntent::Startup(_) => koru_voice_ui::Pending::Startup,
                VoiceIntent::Shortcut(_, _) => koru_voice_ui::Pending::Shortcut,
            })
            .or_else(|| {
                self.hotkey_settling
                    .as_ref()
                    .map(|_| koru_voice_ui::Pending::Shortcut)
            });
        let phase = match self.global_dictation_snapshot.phase {
            crate::dictation_coordinator::Phase::Idle if self.voice_start.is_some() => {
                koru_voice_ui::Phase::Preparing
            }
            crate::dictation_coordinator::Phase::Idle => koru_voice_ui::Phase::Idle,
            crate::dictation_coordinator::Phase::Listening => koru_voice_ui::Phase::Listening,
            crate::dictation_coordinator::Phase::Finalising => koru_voice_ui::Phase::Finalising,
        };
        let shortcut = match &self.dictation_shortcut {
            crate::tray::Shortcut::Chord(value) => Some(value.clone()),
            crate::tray::Shortcut::Unknown | crate::tray::Shortcut::Absent => None,
        };
        koru_voice_ui::ViewModel {
            saved,
            effective_enabled: section.global_dictation_enabled.unwrap_or(false)
                && !self.voice_forced_off,
            start_on_login: section.start_on_login,
            start_on_login_available: self.paths.owns_the_machine(),
            shortcut,
            requested_shortcut: self
                .voice_shortcut_prepare
                .as_ref()
                .map(|(_, value)| value.clone())
                .or_else(|| {
                    self.voice_write
                        .as_ref()
                        .and_then(|write| match &write.intent {
                            VoiceIntent::Shortcut(value, _) => Some(value.clone()),
                            _ => None,
                        })
                })
                .or_else(|| {
                    self.hotkey_settling
                        .as_ref()
                        .map(|(_, value)| value.clone())
                }),
            pending,
            phase,
            problem: self.voice_problem,
        }
    }

    fn refresh_voice_surface(&mut self) -> bool {
        let model = self.voice_model();
        self.voice_window.set_surface_model(model)
    }

    fn problem_for_global_refusal(
        refusal: Option<crate::dictation_coordinator::Refusal>,
    ) -> koru_voice_ui::Problem {
        use crate::dictation_coordinator::Refusal;
        use crate::global_input::Refusal as Target;
        match refusal {
            Some(Refusal::Target(Target::ProtectedTarget)) => {
                koru_voice_ui::Problem::ProtectedTarget
            }
            Some(Refusal::Target(Target::TargetChanged | Target::TargetDestroyed)) => {
                koru_voice_ui::Problem::TargetChanged
            }
            Some(Refusal::Target(_)) => koru_voice_ui::Problem::TargetUnavailable,
            Some(Refusal::Voice(koru_dictation::ErrorCode::ConsentRequired)) => {
                koru_voice_ui::Problem::ConsentRequired
            }
            Some(Refusal::Voice(
                koru_dictation::ErrorCode::ModelMissing
                | koru_dictation::ErrorCode::ModelLoadFailed,
            )) => koru_voice_ui::Problem::ModelUnavailable,
            Some(Refusal::Voice(koru_dictation::ErrorCode::WorkerUnavailable)) => {
                koru_voice_ui::Problem::WorkerUnavailable
            }
            Some(Refusal::Voice(_)) | Some(Refusal::Disabled | Refusal::DirectPartial) | None => {
                koru_voice_ui::Problem::Refused
            }
        }
    }

    fn open_voice_surface(&mut self) {
        if self.voice_window.has_surface() {
            self.refresh_voice_surface();
            return;
        }
        if self.overlay.is_some() || self.window_transition.is_some() {
            self.close_overlay();
            self.voice_window.close_surface();
            self.window_transition = Some(WindowTransition::ClosingVoice);
            return;
        }
        self.prepare_voice_surface();
    }

    fn open_voice_surface_fresh(&mut self) {
        self.voice_problem = None;
        self.open_voice_surface();
    }

    fn handle_installed_voice_tray(&mut self) -> Option<bool> {
        if !matches!(self.voice.row, crate::tray::VoiceRow::Installed) {
            return None;
        }
        if self.global_dictation_snapshot.phase != crate::dictation_coordinator::Phase::Idle {
            self.open_voice_surface_fresh();
            return Some(true);
        }
        if self
            .settings
            .authoritative()
            .global_dictation_enabled
            .unwrap_or(false)
            && !self.voice_forced_off
        {
            self.voice_attempt = Some(VoiceAttemptOrigin::Tray);
            self.tray_voice_start = true;
            if let Some(coordinator) = &self.global_dictation {
                coordinator.prepare_target();
            }
            return Some(false);
        }
        self.open_voice_surface_fresh();
        Some(true)
    }

    fn prepare_voice_surface(&mut self) {
        self.refresh_global_consent_off_thread();
        if self.global_dictation_snapshot.phase != crate::dictation_coordinator::Phase::Idle {
            self.voice_open_waiting = false;
            self.open_voice_surface_now();
            return;
        }
        self.voice_open_waiting = true;
        if let Some(coordinator) = &self.global_dictation {
            coordinator.discard_prepared();
            coordinator.prepare_target();
            return;
        }
        self.voice_open_waiting = false;
        self.voice_problem = Some(koru_voice_ui::Problem::TargetUnavailable);
        self.open_voice_surface_now();
    }

    fn open_voice_surface_now(&mut self) {
        let lang =
            koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
        self.voice_window
            .open_surface(koru_voice_ui::VoiceSurface::new(lang, self.voice_model()));
    }

    fn refresh_global_consent_off_thread(&mut self) {
        if cfg!(test) {
            return;
        }
        let Some(waker) = self.event_waker.clone() else {
            return;
        };
        let consent_file = self.paths.voice_consent_file();
        self.consent_refresh_generation = self.consent_refresh_generation.wrapping_add(1);
        let generation = self.consent_refresh_generation;
        let _ = std::thread::Builder::new()
            .name("koru-agent voice consent".into())
            .spawn(move || {
                let consent = consent_file
                    .map(|path| koru_voice_consent::read(&path))
                    .and_then(|consent| consent.granted().cloned());
                waker.wake(AgentEvent::GlobalConsentRefreshed {
                    generation,
                    consent,
                });
            });
    }

    fn start_voice_write(&mut self, intent: VoiceIntent) {
        if self.voice_write.is_some() {
            return;
        }
        let section = self.settings.authoritative();
        let patch = match &intent {
            VoiceIntent::Enable => voice_enable_patch(
                section.global_dictation_enabled.is_none(),
                self.paths.owns_the_machine(),
            ),
            VoiceIntent::Disable => {
                self.voice_forced_off = true;
                koru_settings::GlobalPatch {
                    global_dictation_enabled: Some(false),
                    ..Default::default()
                }
            }
            VoiceIntent::Startup(enabled) => koru_settings::GlobalPatch {
                start_on_login: Some(*enabled),
                ..Default::default()
            },
            VoiceIntent::Shortcut(value, _) => koru_settings::GlobalPatch {
                global_dictation_shortcut: koru_settings::DictationShortcut::try_new(value.clone())
                    .ok(),
                ..Default::default()
            },
        };
        if matches!(intent, VoiceIntent::Enable) {
            self.voice_forced_off = false;
        }
        self.voice_problem = None;
        self.voice_retry = None;
        self.voice_write = Some(VoiceWrite {
            intent,
            receipt: self.settings.save_confirmed(patch),
        });
        self.apply_global_dictation_settings();
        self.refresh_voice_surface();
    }

    fn poll_voice_write(&mut self) -> bool {
        let Some(mut write) = self.voice_write.take() else {
            return false;
        };
        let Some(outcome) = write.receipt.poll() else {
            self.voice_write = Some(write);
            return false;
        };
        use koru_api_client::ConfirmedOutcome;
        match outcome {
            ConfirmedOutcome::Accepted(authoritative) => {
                self.settings.install_confirmed(authoritative);
                match &write.intent {
                    VoiceIntent::Enable => self.voice_forced_off = false,
                    VoiceIntent::Disable => {
                        self.voice_forced_off =
                            self.settings.authoritative().global_dictation_enabled != Some(false);
                    }
                    VoiceIntent::Shortcut(value, generation) => {
                        if let Some(control) = &self.hotkey_control {
                            if control.commit_dictation(*generation) {
                                self.hotkey_settling = Some((*generation, value.clone()));
                            }
                        }
                    }
                    VoiceIntent::Startup(_) => {}
                }
                self.voice_problem = None;
            }
            ConfirmedOutcome::Stale => {
                self.rollback_voice_intent(&write.intent);
            }
            ConfirmedOutcome::TimedOut => {
                self.rollback_voice_intent(&write.intent);
                let disabling = matches!(write.intent, VoiceIntent::Disable);
                self.voice_retry = Some(write.intent);
                self.voice_problem = Some(if disabling {
                    koru_voice_ui::Problem::UnsavedRuntime
                } else {
                    koru_voice_ui::Problem::TimedOut
                });
            }
            ConfirmedOutcome::Disconnected => {
                self.rollback_voice_intent(&write.intent);
                let disabling = matches!(write.intent, VoiceIntent::Disable);
                self.voice_retry = Some(write.intent);
                self.voice_problem = Some(if disabling {
                    koru_voice_ui::Problem::UnsavedRuntime
                } else {
                    koru_voice_ui::Problem::Disconnected
                });
            }
            ConfirmedOutcome::Reconciled { authoritative, .. }
            | ConfirmedOutcome::ContractMismatch { authoritative } => {
                self.settings.install_confirmed(authoritative);
                self.rollback_voice_intent(&write.intent);
                let disabling = matches!(write.intent, VoiceIntent::Disable);
                self.voice_retry = Some(write.intent);
                self.voice_problem = Some(if disabling {
                    koru_voice_ui::Problem::UnsavedRuntime
                } else {
                    koru_voice_ui::Problem::SettingRefused
                });
            }
            ConfirmedOutcome::Refused { .. } => {
                self.rollback_voice_intent(&write.intent);
                let disabling = matches!(write.intent, VoiceIntent::Disable);
                self.voice_retry = Some(write.intent);
                self.voice_problem = Some(if disabling {
                    koru_voice_ui::Problem::UnsavedRuntime
                } else {
                    koru_voice_ui::Problem::SettingRefused
                });
            }
        }
        self.apply_global_dictation_settings();
        self.refresh_voice_surface()
    }

    fn rollback_voice_intent(&mut self, intent: &VoiceIntent) {
        if let VoiceIntent::Shortcut(value, generation) = intent
            && let Some(control) = &self.hotkey_control
        {
            if control.rollback_dictation(*generation) {
                self.hotkey_settling = Some((*generation, value.clone()));
            }
        }
    }

    fn handle_voice_action(&mut self, action: koru_voice_ui::Action) {
        match action {
            koru_voice_ui::Action::Enable => self.start_voice_write(VoiceIntent::Enable),
            koru_voice_ui::Action::Disable => self.start_voice_write(VoiceIntent::Disable),
            koru_voice_ui::Action::SetStartOnLogin(value) => {
                self.start_voice_write(VoiceIntent::Startup(value))
            }
            koru_voice_ui::Action::SetShortcut(value) => {
                if koru_settings::DictationShortcut::try_new(value.clone()).is_err() {
                    self.voice_problem = Some(koru_voice_ui::Problem::SettingRefused);
                    return;
                }
                self.hotkey_generation = self.hotkey_generation.wrapping_add(1);
                let generation = self.hotkey_generation;
                if self
                    .hotkey_control
                    .as_ref()
                    .is_some_and(|control| control.prepare_dictation(generation, &value))
                {
                    self.voice_shortcut_prepare = Some((generation, value));
                } else {
                    self.voice_problem = Some(koru_voice_ui::Problem::ShortcutTaken);
                }
            }
            koru_voice_ui::Action::Start => {
                self.voice_window.close_surface();
                self.voice_open_waiting = false;
                self.window_transition = None;
                self.voice_attempt = Some(VoiceAttemptOrigin::Surface);
                self.voice_start = Some(VoiceStartStage::Closing);
            }
            koru_voice_ui::Action::Cancel => {
                if let Some(coordinator) = &self.global_dictation {
                    coordinator.cancel(crate::global_input::CancelReason::Explicit);
                }
            }
            koru_voice_ui::Action::Retry => {
                self.refresh_global_consent_off_thread();
                if let Some(intent) = self.voice_retry.clone() {
                    self.start_voice_write(intent);
                } else {
                    self.voice_problem = None;
                    self.voice_window.close_surface();
                    self.voice_open_waiting = false;
                    self.window_transition = Some(WindowTransition::ClosingVoice);
                }
            }
            koru_voice_ui::Action::Close => {
                self.voice_window.close_surface();
                self.voice_open_waiting = false;
                self.voice_start = None;
                self.window_transition = None;
                self.voice_attempt = None;
                if let Some(coordinator) = &self.global_dictation {
                    coordinator.discard_prepared();
                }
            }
            koru_voice_ui::Action::PickShortcut => {}
        }
        self.refresh_voice_surface();
    }

    fn connection(&self, id: ConnectionId) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id() == id)
    }

    // ----------------------------------------------------------- the updates

    /// Become -- or stop being -- the build that opens `koru://` links.
    ///
    /// **Not for a `--root` agent.** Unlike OS-arbitrated chords, a URL scheme
    /// has one replaceable machine-wide owner. A developer's throwaway root
    /// must not decide what a person's browser opens.
    fn set_url_scheme(&mut self, register: bool) {
        if !self.paths.owns_the_machine() {
            koru_ipc::ipc_debug!("tray: a rooted agent does not register koru:// links");
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            koru_ipc::ipc_debug!("tray: this executable has no path to register");
            return;
        };
        let outcome = if register {
            koru_url_scheme::register(&exe)
        } else {
            koru_url_scheme::unregister(&exe)
        };
        if let Err(error) = outcome {
            koru_ipc::ipc_debug!("tray: koru:// registration did not change: {error}");
        }
        // The row is refreshed by the host after this event, the same way the
        // shortcut and store rows are -- and it re-reads the registry rather
        // than trusting what was asked for, because another build may own it.
    }

    /// Start checking for updates, if this agent is one that should.
    ///
    /// **Three gates, and each closes a different door.**
    ///
    /// An explicit root resolves its own update paths, but the running
    /// executable must also be a copied bundle beneath that root. This keeps a
    /// rooted checkout unavailable without disabling isolated update rehearsals.
    ///
    /// `updates_enabled()` is false for anything cargo built — no build number,
    /// no public key, no updater hash — so a developer's agent never dials the
    /// update service at all.
    ///
    /// `cfg!(test)` is the belt under both. A `cargo test` run that reached
    /// `updates.koru.do` would be the worst outcome of this whole change, and
    /// `AGENTS.md` records that the store gate got this wrong once already by
    /// relying on a single check.
    fn start_updates(&mut self, waker: &crate::event::Waker) {
        if cfg!(test) || !koru_update::build_info().updates_enabled() {
            return;
        }
        let install = install_dir();
        let Some(update_paths) = update_paths_for(&self.paths, &install) else {
            koru_ipc::ipc_debug!(
                "update: rooted executable is outside its root-owned copied bundle"
            );
            return;
        };
        let Ok(controller) = koru_update::UpdateController::new(update_base_url(), update_paths)
        else {
            koru_ipc::ipc_debug!("update: could not initialize update paths");
            return;
        };
        self.attach_updates(controller, waker, true);
    }

    fn attach_updates(
        &mut self,
        controller: koru_update::UpdateController,
        waker: &crate::event::Waker,
        automatic: bool,
    ) {
        let notify_waker = waker.clone();
        controller.set_notifier(move || notify_waker.wake(crate::event::AgentEvent::UpdateChanged));
        if automatic {
            controller.start_automatic();
        }
        // The shared half of the installation lock. Held for as long as this
        // agent runs, so `other_instances` can tell a window it did not start
        // from no window at all — and dropped deliberately before the updater
        // asks for the exclusive one.
        debug_assert!(self.update_lease.is_some());
        // Once, here, and never on the loop — see the field's comment.
        self.install_writable = koru_update::is_writable_install(&install_dir());
        self.updates = Some(controller);
        if !automatic {
            // The local manifest is already an Available status; there is no
            // check worker to generate the first wake. Feed it through the same
            // broadcast/dialog path a completed network check uses.
            waker.wake(crate::event::AgentEvent::UpdateChanged);
        }
    }

    /// Put back the windows an update closed.
    ///
    /// **The other half of the promise `begin_install` makes.** An update that
    /// closed somebody's windows and handed back a tray icon would be its own
    /// bug report, so the list is written before they are asked to go and
    /// replayed here — by a different run of a different build, which is why it
    /// travelled through a file rather than through memory.
    ///
    /// A failed launch stays in the record for the next Agent. A window is
    /// removed only after its real `Hello`; chat only after its singleton
    /// endpoint answers. The worker performs every disk operation and process
    /// launch, so the Agent event loop only schedules the transition.
    fn reopen_after_update(&mut self) {
        use std::sync::atomic::Ordering;

        if cfg!(test)
            || !self.reopen_pending.load(Ordering::Acquire)
            || self.reopen_replay_running.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let paths = self.paths.clone();
        let io = self.reopen_io.clone();
        let pending = self.reopen_pending.clone();
        let running = self.reopen_replay_running.clone();
        let live = self
            .registry
            .shells()
            .into_iter()
            .filter_map(|shell| {
                self.reopen_claims.get(&shell.connection).map(|claim| {
                    (
                        shell.profile.to_storage(),
                        shell.workspace.to_storage(),
                        claim.clone(),
                    )
                })
            })
            .collect();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-agent reopen replay".into())
            .spawn(move || replay_reopen(paths, io, pending, running, live))
        {
            self.reopen_replay_running.store(false, Ordering::Release);
            koru_ipc::ipc_debug!("update: could not start the reopen replay worker: {error}");
        }
    }

    fn acknowledge_reopened_window(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        claim: Option<&str>,
    ) {
        use std::sync::atomic::Ordering;

        let Some(claim) = claim.filter(|claim| !claim.is_empty()) else {
            return;
        };
        if !self.reopen_pending.load(Ordering::Acquire) {
            return;
        }
        let paths = self.paths.clone();
        let io = self.reopen_io.clone();
        let pending = self.reopen_pending.clone();
        let profile = profile.to_storage();
        let workspace = workspace.to_storage();
        let claim = claim.to_owned();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-agent reopen ack".into())
            .spawn(move || {
                let Ok(_guard) = io.lock() else {
                    return;
                };
                let mut reopen = crate::update::Reopen::load(&paths);
                let mut persistence_failed = false;
                if reopen.acknowledge_claim(&profile, &workspace, &claim) {
                    match reopen.save(&paths) {
                        Ok(()) => koru_ipc::ipc_debug!(
                            "update: a reopened window completed its Agent handshake"
                        ),
                        Err(error) => {
                            persistence_failed = true;
                            koru_ipc::ipc_debug!(
                                "update: could not persist a window reopen acknowledgement: {error}"
                            )
                        }
                    }
                }
                pending.store(persistence_failed || !reopen.is_empty(), Ordering::Release);
            })
        {
            koru_ipc::ipc_debug!(
                "update: could not start the window reopen acknowledgement worker: {error}"
            );
        }
    }

    /// Finish the far side of an update only after the service stack is ready.
    ///
    /// `ensure_gateway` returns only after the gateway authenticated its private
    /// bridge and spawned the Store worker. The health file is the updater's
    /// commit point, so it is written before the reopen record is consumed. A
    /// failed readiness event leaves health, What's new, and reopen untouched
    /// for the updater's rollback path.
    fn complete_startup_transition(&mut self) {
        if self.startup_transition_complete {
            return;
        }
        if let Some(token) = self.update_health_token.as_deref() {
            // The signed A/B release drill needs to prove rollback through the
            // same post-swap failure the product handles, without corrupting a
            // signed archive or fabricating a health marker. Scope the seam to
            // the two facts only a rehearsal restart has together: an explicit
            // root and the updater's one-time token. The rollback launch has no
            // token, so it comes up normally even though it inherits the env.
            let fail_rehearsal_health = should_fail_rehearsal_health(
                self.paths.explicit_root().is_some(),
                true,
                koru_env::switch(koru_env::name::KORU_REHEARSAL_FAIL_UPDATE_HEALTH),
            );
            if fail_rehearsal_health {
                koru_ipc::ipc_debug!(
                    "agent: rooted update rehearsal exiting before health confirmation"
                );
                self.exit = true;
                return;
            }
            let acknowledged = match self.paths.explicit_root() {
                Some(root) => koru_update::UpdatePaths::under(root).signal_health(token),
                None => koru_update::signal_health(token),
            };
            if let Err(error) = acknowledged {
                koru_ipc::ipc_debug!("agent: could not acknowledge update health: {error}");
                self.startup_gate_retry_at = Some(Instant::now() + Duration::from_millis(50));
                return;
            }
            crate::update::WhatsNew::owed(&self.paths, koru_update::build_info().version);
            // The far side of the upgrade funnel: `accepted` was the old
            // build's last word, this is the new build's first. Publishing
            // waits for the same gateway-and-Store readiness proof as health;
            // emitting earlier would call a merely bound Agent a successful
            // product restart.
            crate::service::emit_event(
                self.paths.clone(),
                koru_events::AppUpdated {
                    build: koru_update::build_info().build_number,
                }
                .into(),
            );
            koru_ipc::ipc_debug!(
                "agent: gateway and Store ready; told the updater this build came up"
            );
            self.update_health_token = None;
        }
        if let Some(lease) = &mut self.update_lease {
            match lease.join_restart_gate() {
                Ok(true) => {}
                Ok(false) => {
                    self.startup_gate_retry_at = Some(Instant::now() + Duration::from_millis(50));
                    return;
                }
                Err(error) => {
                    koru_ipc::ipc_debug!(
                        "agent: could not join the post-update restart gate: {error}"
                    );
                    self.startup_gate_retry_at = Some(Instant::now() + Duration::from_millis(50));
                    return;
                }
            }
        }
        self.startup_gate_retry_at = None;
        self.startup_transition_complete = true;
        self.reopen_after_update();
    }

    fn restart_gate_pending(&self) -> bool {
        self.update_health_token.is_some() || self.startup_gate_retry_at.is_some()
    }

    fn show_restart_gate_warning(&mut self) {
        koru_ipc::ipc_debug!("agent: child launch deferred until the update restart gate opens");
        self.open_overlay();
        if let Some(surface) = self.overlay.as_mut() {
            surface.set_host_warning(Some(
                koru_i18n::t(
                    koru_i18n::Lang::for_system(
                        koru_window::system_locale::system_locale().as_deref(),
                    ),
                    koru_i18n::Key::UpdateInstallInProgress,
                )
                .into(),
            ));
        }
    }

    /// What every window should currently believe about updating.
    ///
    /// `None` when this agent manages no updates at all — a development/test
    /// build, or a rooted executable outside its owned copy. A window told
    /// nothing draws its update buttons disabled, which is the honest
    /// rendering of "there is nobody to ask".
    /// The upgrade funnel's first two steps, derived from the controller's
    /// state on every change and deduped per offered build: `available` when
    /// an offer first appears, `downloaded` when its package first verifies.
    /// `accepted` is emitted where the Install consent is acted on, and
    /// `updated` by the restarted build — four events that say where an
    /// upgrade stalls, which is the whole reason they exist.
    fn note_update_funnel(&mut self) {
        let Some(controller) = &self.updates else {
            return;
        };
        match controller.status() {
            koru_update::UpdateStatus::Available(manifest)
                if self.update_funnel_available != Some(manifest.build_number) =>
            {
                self.update_funnel_available = Some(manifest.build_number);
                crate::service::emit_event(
                    self.paths.clone(),
                    koru_events::AppUpdateAvailable {
                        to_build: manifest.build_number,
                    }
                    .into(),
                );
            }
            koru_update::UpdateStatus::ReadyToInstall { manifest, .. }
                if self.update_funnel_downloaded != Some(manifest.build_number) =>
            {
                // A restart can land here without ever showing `Available`
                // this run (the package was already on disk); the offer is
                // still announced first so the funnel's steps stay ordered.
                if self.update_funnel_available != Some(manifest.build_number) {
                    self.update_funnel_available = Some(manifest.build_number);
                    crate::service::emit_event(
                        self.paths.clone(),
                        koru_events::AppUpdateAvailable {
                            to_build: manifest.build_number,
                        }
                        .into(),
                    );
                }
                self.update_funnel_downloaded = Some(manifest.build_number);
                crate::service::emit_event(
                    self.paths.clone(),
                    koru_events::AppUpdateDownloaded {
                        to_build: manifest.build_number,
                    }
                    .into(),
                );
            }
            _ => {}
        }
    }

    fn update_report(&self) -> Option<koru_agent_wire::UpdateReport> {
        let controller = self.updates.as_ref()?;
        let status = controller.status();
        let mut report = crate::update::report_for(
            &status,
            self.install_writable,
            controller.automatic_checks(),
            controller.channel().as_str(),
        );
        // **The handoff is what makes the stage `Installing`, not the
        // controller.** `take_pending_install` is the call that moves the
        // controller's own status, and it happens *last* — after every window
        // has gone. But `Installing` is precisely the message that asks them to
        // go, so waiting for the controller would mean nobody was ever told and
        // the handoff would sit until it timed out.
        if self.handoff.in_progress() {
            report.stage = koru_agent_wire::UpdateStage::Installing;
        }
        report.active_session_count = self.session_catalog.active_count;
        report.session_generation = self.session_catalog.generation;
        Some(report)
    }

    /// Tell one window, which is what a window that has just arrived needs.
    fn push_update_to(&mut self, connection: ConnectionId) {
        if !self.refresh_session_catalog_for_ready() {
            return;
        }
        let Some(report) = self.update_report() else {
            return;
        };
        self.push(
            connection,
            Push::Update {
                report: Box::new(report),
            },
        );
    }

    /// **Ask a window to open the voice consent step.**
    ///
    /// Every live window gets it — the first one to draw the modal is the one
    /// somebody answers, and a second window drawing the same question is not
    /// worth a routing decision the agent would have to keep in step with
    /// which window has focus.
    ///
    /// **With no window, the ask is latched rather than dropped.**
    /// `restore_windows` spawns one when there is none, and a push sent into an
    /// empty connection list would be a menu row that opened a window and then
    /// did nothing — the most confusing possible answer to a press. The latch
    /// is spent by the next `Hello`, the same shape `push_whats_new_to` uses
    /// and for the same reason: the window that has to be told does not exist
    /// yet.
    fn offer_feedback(&mut self) {
        let ids: Vec<ConnectionId> = self.connections.iter().map(|c| c.id()).collect();
        if ids.is_empty() {
            self.feedback_owed = true;
            self.restore_windows();
            return;
        }
        if let Some(&id) = ids.first() {
            self.push(id, Push::ShowFeedback);
        }
    }

    fn push_feedback_to(&mut self, connection: ConnectionId) {
        if !std::mem::take(&mut self.feedback_owed) {
            return;
        }
        self.push(connection, Push::ShowFeedback);
    }

    fn offer_voice_setup(&mut self) {
        let ids: Vec<ConnectionId> = self.connections.iter().map(|c| c.id()).collect();
        if ids.is_empty() {
            self.voice_setup_owed = true;
            return;
        }
        for id in ids {
            self.push(id, Push::Voice);
        }
    }

    /// Hand a latched voice offer to the window that just arrived.
    ///
    /// **Taken, not read**, so it reaches exactly one window and never opens a
    /// second modal — the agent outlives many shells, and a flag left set would
    /// greet every one of them with a question somebody answered last week.
    fn push_voice_setup_to(&mut self, connection: ConnectionId) {
        if !std::mem::take(&mut self.voice_setup_owed) {
            return;
        }
        self.push(connection, Push::Voice);
    }

    /// Hand a latched microphone choice to the window that just arrived.
    ///
    /// **Taken, not read**, for the same reason the offer above is: the agent
    /// outlives many shells, and a choice left latched would be re-applied to
    /// every one of them long after somebody had changed their mind in
    /// Settings.
    fn push_voice_input_to(&mut self, connection: ConnectionId) {
        let Some(device) = self.voice_input_owed.take() else {
            return;
        };
        self.push(connection, Push::VoiceInput { device });
    }

    /// Tell one window which chord opens Search, if it is already known.
    ///
    /// The same late-arrival rule `push_update_to` follows: the hotkey thread
    /// answers once, shortly after the agent starts, and a window that connects
    /// afterwards would otherwise never learn the chord. `Shortcut::Unknown`
    /// means the thread has not answered yet — push nothing, because the
    /// registration broadcast below will carry it when it does.
    fn push_shortcut_to(&mut self, connection: ConnectionId) {
        match &self.shortcut {
            crate::tray::Shortcut::Chord(label) => {
                self.push(
                    connection,
                    Push::Shortcut {
                        shortcut: Some(label.clone()),
                    },
                );
            }
            crate::tray::Shortcut::Absent => {
                self.push(connection, Push::Shortcut { shortcut: None });
            }
            crate::tray::Shortcut::Unknown => {}
        }
    }

    fn push_reminder_diagnostics_to(&mut self, connection: ConnectionId) {
        self.push(
            connection,
            Push::ReminderDiagnostics {
                reminders: reminder_diagnostics(&self.reminder_report),
            },
        );
    }

    fn broadcast_reminder_diagnostics(&mut self) {
        let reminders = reminder_diagnostics(&self.reminder_report);
        let ids = self
            .registry
            .shells()
            .into_iter()
            .map(|shell| shell.connection)
            .collect::<Vec<_>>();
        for connection in ids {
            self.push(
                connection,
                Push::ReminderDiagnostics {
                    reminders: reminders.clone(),
                },
            );
        }
    }

    /// Tell every window which chord opens Search.
    fn broadcast_shortcut(&mut self) {
        let shortcut = match &self.shortcut {
            crate::tray::Shortcut::Chord(label) => Some(label.clone()),
            crate::tray::Shortcut::Absent => None,
            crate::tray::Shortcut::Unknown => return,
        };
        let ids: Vec<ConnectionId> = self.connections.iter().map(|c| c.id()).collect();
        for id in ids {
            self.push(
                id,
                Push::Shortcut {
                    shortcut: shortcut.clone(),
                },
            );
        }
    }

    /// Hand the *What's new* an install left behind to the window that just
    /// arrived, if one is owed.
    ///
    /// **Not gated on `owns_the_machine`**, unlike the update controller. There
    /// is nothing to protect: `agent_dir()` is root-aware, so a `--root` agent's
    /// marker is in its own root and cannot reach a real installation's. A gate
    /// here would be a branch that is never taken, and it would make the flow
    /// impossible to exercise under `--root` — which is the only way to try it
    /// without publishing two builds.
    ///
    /// **Cleared only once the frame is away.** See [`WhatsNew::peek`].
    fn push_whats_new_to(&mut self, connection: ConnectionId) {
        let Some(version) = crate::update::WhatsNew::peek(&self.paths).version else {
            return;
        };
        koru_ipc::ipc_debug!("update: telling a window about {version}");
        if self.push(connection, Push::Updated { version }) {
            crate::update::WhatsNew::delivered(&self.paths);
        }
    }

    /// Tell every window what the agent now knows, or ask in a dialog if there
    /// are none.
    fn broadcast_update(&mut self) {
        let Some(report) = self.update_report() else {
            return;
        };
        // **Only when it says something new.** See `last_broadcast_report`:
        // the worker wakes this far more often than anything changes.
        let ids: Vec<ConnectionId> = self.connections.iter().map(|c| c.id()).collect();
        let changed = self.last_broadcast_report.as_ref() != Some(&report);
        if changed {
            // **One report to all of them.** Two windows disagreeing about
            // whether there is an update is a bug somebody reports as flicker.
            for id in &ids {
                self.push(
                    *id,
                    Push::Update {
                        report: Box::new(report.clone()),
                    },
                );
            }
            self.last_broadcast_report = Some(report.clone());
        }

        // Nobody is looking at a Koru window, so the tray has to ask. Only for
        // the two stages that are a question; the rest are progress.
        let asks = matches!(
            report.stage,
            koru_agent_wire::UpdateStage::Available | koru_agent_wire::UpdateStage::Ready
        );
        if ids.is_empty() && asks && !self.update_dialog_open {
            if let Some(waker) = self.event_waker.clone() {
                self.update_dialog_open = true;
                if !cfg!(test) {
                    crate::update::offer_in_a_dialog(report, waker);
                }
            }
        }
    }

    /// Answer a person's choice, refusing the ones this stage does not allow.
    fn apply_update_choice(
        &mut self,
        choice: koru_agent_wire::UpdateChoice,
        queued: std::time::Duration,
    ) -> Result<(), (String, String)> {
        use koru_agent_wire::UpdateChoice;

        // A consent is rare and consequential, so its arrival is dated
        // unconditionally — the window re-arms its buttons ten seconds after
        // sending one, and without this line a slow delivery and a click that
        // never happened look identical in the log. `queued_ms` is how long
        // the request sat between the reader thread and this handler, so a
        // slow consent splits into its pipe half and its event-loop half.
        self.routing_log.event(format_args!(
            "update choice arrived choice={choice:?} queued_ms={}",
            queued.as_millis()
        ));
        let Some(controller) = self.updates.clone() else {
            // An agent with no controller is a rooted or development one. Say
            // which, because "nothing happened" is the least helpful answer a
            // button can give.
            return Err((
                crate::update::NOT_PERMITTED.into(),
                "this agent does not manage updates for this installation".into(),
            ));
        };
        let status = controller.status();
        let stage = crate::update::report_for(&status, false, false, "").stage;
        if !crate::update::allows(stage, choice) {
            return Err((
                crate::update::WRONG_STAGE.into(),
                format!("{choice:?} cannot be answered from {stage:?}"),
            ));
        }
        match choice {
            UpdateChoice::Download => match status {
                koru_update::UpdateStatus::Available(manifest) => controller.download(manifest),
                _ => {
                    return Err((
                        crate::update::NO_UPDATE.into(),
                        "there is no update to download".into(),
                    ));
                }
            },
            UpdateChoice::Install { .. } => {
                // The second consent, on its way to being acted on — the
                // funnel's `accepted`. `app.updated` from the restarted build
                // is what says it worked; an accepted with no updated after
                // it is an install that failed or rolled back.
                return Err((
                    crate::update::WRONG_STAGE.into(),
                    "install consent must pass the session-generation gate".into(),
                ));
            }
            UpdateChoice::Later => controller.defer(),
            UpdateChoice::Cancel => controller.cancel_download(),
            UpdateChoice::Retry => controller.check(koru_update::CheckReason::Manual),
            UpdateChoice::CloseRefused => {
                // **Unsaved work wins, and the offer survives.** The handoff
                // ends now rather than at its deadline, and nothing about the
                // build is deferred — `advance_handoff` reports the reason and
                // the banner comes back on the same build.
                self.handoff.refused("a Koru window was not closed");
                self.advance_handoff();
            }
            UpdateChoice::CheckNow => controller.check(koru_update::CheckReason::Manual),
            UpdateChoice::DismissNewer => controller.dismiss_installed_newer(),
            UpdateChoice::SetAutomatic { on } => controller.set_automatic_checks(on),
        }
        self.broadcast_update();
        Ok(())
    }

    /// Compare install consent with the session host on a worker. The request
    /// reply is delayed until the host has atomically frozen new creation.
    fn prepare_sessions_for_install(
        &mut self,
        observed_generation: u64,
        requester: Option<(ConnectionId, u64)>,
        queued: std::time::Duration,
    ) -> Result<(), (String, String)> {
        self.routing_log.event(format_args!(
            "update choice arrived choice=Install session_generation={observed_generation} queued_ms={}",
            queued.as_millis()
        ));
        let Some(controller) = self.updates.clone() else {
            return Err((
                crate::update::NOT_PERMITTED.into(),
                "this agent does not manage updates for this installation".into(),
            ));
        };
        let status = controller.status();
        let stage = crate::update::report_for(&status, false, false, "").stage;
        if stage != koru_agent_wire::UpdateStage::Ready || self.handoff.in_progress() {
            return Err((
                crate::update::WRONG_STAGE.into(),
                format!("Install cannot be answered from {stage:?}"),
            ));
        }
        if observed_generation == 0 || observed_generation != self.session_catalog.generation {
            return Err((
                crate::update::SESSIONS_CHANGED.into(),
                "terminal sessions changed; review the current count and try again".into(),
            ));
        }
        if self.session_prepare_pending {
            return Err((
                crate::update::WRONG_STAGE.into(),
                "an install consent is already being checked".into(),
            ));
        }
        let Some(lifecycle) = self.session_lifecycle.clone() else {
            return Err((
                crate::update::NOT_PERMITTED.into(),
                "the terminal session host is unavailable".into(),
            ));
        };
        let Some(waker) = self.event_waker.clone() else {
            return Err((
                crate::update::NOT_PERMITTED.into(),
                "the Agent lifecycle worker is unavailable".into(),
            ));
        };
        self.session_prepare_pending = true;
        if let Err(error) = std::thread::Builder::new()
            .name("koru-agent session consent".into())
            .spawn(move || {
                let result = lifecycle.prepare_and_refresh(observed_generation);
                waker.wake(crate::event::AgentEvent::SessionShutdownPrepared { requester, result });
            })
        {
            self.session_prepare_pending = false;
            return Err((
                crate::update::NOT_PERMITTED.into(),
                format!("could not start the session consent worker: {error}"),
            ));
        }
        Ok(())
    }

    /// Refresh exactly once when a verified build first becomes Ready. This
    /// keeps the count in the consent surface current without adding an idle
    /// session poll or putting private IPC on winit.
    fn refresh_session_catalog_for_ready(&mut self) -> bool {
        let Some(controller) = &self.updates else {
            return true;
        };
        let koru_update::UpdateStatus::ReadyToInstall { manifest, .. } = controller.status() else {
            self.session_catalog_ready_build = None;
            return true;
        };
        let build_number = manifest.build_number;
        if self.session_catalog_ready_build == Some(build_number) {
            return true;
        }
        if self.session_catalog_refresh_pending {
            return false;
        }
        let Some(lifecycle) = self.session_lifecycle.clone() else {
            self.session_catalog = crate::session_host::CatalogFact {
                generation: 0,
                active_count: 0,
            };
            self.session_catalog_ready_build = Some(build_number);
            return true;
        };
        let Some(waker) = self.event_waker.clone() else {
            return false;
        };
        self.session_catalog_refresh_pending = true;
        if std::thread::Builder::new()
            .name("koru-agent session catalog".into())
            .spawn(move || {
                let result = lifecycle.catalog();
                waker.wake(crate::event::AgentEvent::SessionCatalogRefreshed {
                    build_number,
                    result,
                });
            })
            .is_err()
        {
            self.session_catalog_refresh_pending = false;
            self.session_catalog.generation = 0;
            self.session_catalog_ready_build = Some(build_number);
            return true;
        }
        false
    }

    /// Begin bringing the stack down so the swap can happen.
    ///
    /// **Windows first, then the store, then this process.** A window saves on
    /// its way out and a store that had already stopped could not accept those
    /// writes — so the order is not arbitrary and reversing it loses work.
    fn begin_install(&mut self) {
        if self.handoff.in_progress() {
            return;
        }
        // Read the registry *now*: after the windows are asked to close there
        // is nothing left to ask, and an update that silently closed somebody's
        // windows and handed back a tray icon would be its own bug report.
        self.reopen = crate::update::Reopen::from_shells(&self.registry.shells());
        // One serialization boundary for every reopen-record mutation. A late
        // acknowledgement from the preceding transition must finish before
        // this fresh snapshot replaces it; after this lock is released its
        // stale claim cannot match the new record.
        let saved = self
            .reopen_io
            .lock()
            .map_err(|_| "the reopen persistence lock was poisoned".to_string())
            .and_then(|_guard| self.reopen.save(&self.paths));
        if let Err(error) = saved {
            let reason = format!("open windows could not be recorded before update: {error}");
            self.routing_log
                .event(format_args!("install aborted: {reason}"));
            if let Some(controller) = &self.updates {
                controller.install_blocked(reason);
            }
            self.reopen = crate::update::Reopen::default();
            self.broadcast_update();
            return;
        }
        self.reopen_pending.store(
            !self.reopen.is_empty(),
            std::sync::atomic::Ordering::Release,
        );
        let waiting = self.connections.len();
        self.handoff = crate::update::Handoff::begin(waiting, std::time::Instant::now());
        self.routing_log.event(format_args!(
            "install handoff begun windows_recorded={} connections={waiting} handoff={}",
            self.reopen.windows.len(),
            self.handoff.label(),
        ));
        // Tell them before waiting for them: the `Installing` stage *is* the
        // request to save and close.
        self.broadcast_update();
        self.advance_handoff();
    }

    /// Move the install along as windows disappear and the store stops.
    fn advance_handoff(&mut self) {
        use crate::update::Handoff;

        match &self.handoff {
            Handoff::Idle | Handoff::AskingShells { .. } => {}
            Handoff::StoppingSessions => {
                if self.session_stopping {
                    return;
                }
                let Some(lifecycle) = self.session_lifecycle.clone() else {
                    self.handoff = Handoff::Aborted(
                        "the terminal session host is unavailable; nothing was replaced".into(),
                    );
                    self.advance_handoff();
                    return;
                };
                let Some(waker) = self.event_waker.clone() else {
                    return;
                };
                self.session_stopping = true;
                self.routing_log
                    .event(format_args!("install handoff stopping terminal sessions"));
                if let Err(error) = std::thread::Builder::new()
                    .name("koru-agent session shutdown".into())
                    .spawn(move || {
                        let outcome = lifecycle.stop();
                        waker.wake(crate::event::AgentEvent::SessionHostStopped { outcome });
                    })
                {
                    self.session_stopping = false;
                    self.handoff = Handoff::Aborted(format!(
                        "could not start terminal-session shutdown: {error}"
                    ));
                    self.advance_handoff();
                }
            }
            Handoff::StoppingChat => match &self.chat_stopping {
                Some(done) => {
                    let result = done.lock().ok().and_then(|mut result| result.take());
                    let Some(result) = result else {
                        return;
                    };
                    self.chat_stopping = None;
                    match result {
                        Ok(was_live) => {
                            self.reopen.ai_chat = was_live;
                            self.handoff = Handoff::StoppingSessions;
                            self.routing_log.event(format_args!(
                                "install handoff AI chat stopped was_live={was_live}"
                            ));
                            self.advance_handoff();
                        }
                        Err(error) => {
                            self.handoff = Handoff::Aborted(error);
                            self.advance_handoff();
                        }
                    }
                }
                None => {
                    let Some(waker) = self.event_waker.clone() else {
                        return;
                    };
                    let done = std::sync::Arc::new(std::sync::Mutex::new(None));
                    self.chat_stopping = Some(done.clone());
                    if cfg!(test) {
                        return;
                    }
                    let root = self.paths.explicit_root().map(Path::to_path_buf);
                    let paths = self.paths.clone();
                    let reopen_io = self.reopen_io.clone();
                    let worker_done = done.clone();
                    let worker_waker = waker.clone();
                    if let Err(error) = std::thread::Builder::new()
                        .name("koru-agent update AI chat handoff".into())
                        .spawn(move || {
                            let result = crate::ai_chat::stop_for_update(
                                root.as_deref(),
                                Duration::from_secs(15),
                                || {
                                    let _guard = reopen_io.lock().map_err(|_| {
                                        "the reopen persistence lock was poisoned".to_string()
                                    })?;
                                    let mut durable = crate::update::Reopen::load(&paths);
                                    durable.ai_chat = true;
                                    durable.save(&paths)
                                },
                            );
                            if let Ok(mut slot) = worker_done.lock() {
                                *slot = Some(result);
                            }
                            worker_waker.wake(crate::event::AgentEvent::UpdateChanged);
                        })
                    {
                        self.chat_stopping = None;
                        self.handoff = Handoff::Aborted(format!(
                            "Koru AI chat close worker could not start: {error}"
                        ));
                        self.advance_handoff();
                    }
                }
            },
            Handoff::StoppingStore => {
                // **The flag, not the event, is what says the store has gone.**
                // `UpdateChanged` is raised by the controller's own worker too —
                // a download tick would otherwise walk this state on to `Ready`
                // and spawn the updater over a store that was still running,
                // which is the one thing this step exists to prevent.
                let finished = self
                    .store_stopping
                    .as_ref()
                    .and_then(|slot| slot.lock().ok().and_then(|mut slot| slot.take()));
                match (&self.store_stopping, finished) {
                    // **The gate, and it fails closed.** `Ok` means the child
                    // confirmed its own Store shutdown — it exited 0, which the
                    // gateway role sets only after joining the Store worker and
                    // finding it clean. Anything else — a kill on the deadline,
                    // a child that had already died, a stop that never
                    // completed — is not evidence the last commit finished, and
                    // **an update never installs over a Store that has not
                    // finished its last commit**. Aborting resumes the service;
                    // installing would not be undoable.
                    (Some(_), Some(Ok(report))) => {
                        self.store_stopping = None;
                        self.handoff = Handoff::Ready;
                        self.routing_log
                            .event(format_args!("install handoff {report}"));
                        self.advance_handoff();
                    }
                    (Some(_), Some(Err(why))) => {
                        self.store_stopping = None;
                        self.routing_log
                            .event(format_args!("install handoff gateway stop refused: {why}"));
                        self.handoff = Handoff::Aborted(format!(
                            "the Koru Store did not confirm a clean shutdown: {why}"
                        ));
                        self.advance_handoff();
                    }
                    // Already asked; the wake that matters has not arrived yet.
                    (Some(_), None) => {}
                    (None, _) => {
                        let Some(waker) = self.event_waker.clone() else {
                            return;
                        };
                        self.routing_log
                            .event(format_args!("install handoff stopping the gateway"));
                        let done = std::sync::Arc::new(std::sync::Mutex::new(None));
                        self.store_stopping = Some(done.clone());
                        if cfg!(test) {
                            return;
                        }
                        let routing_log = self.routing_log.clone();
                        let _ = std::thread::Builder::new()
                            .name("koru-agent update handoff".into())
                            .spawn(move || {
                                // **Hold first, then a checked stop.** The hold
                                // suppresses the supervisor's own respawn so
                                // the stop is not racing one, and it leaves the
                                // supervision parked and revivable for an
                                // abort. The report names the operation, the
                                // generation and *how the child ended*, so a
                                // later status snapshot cannot stand in for it.
                                let stopped = crate::update::hold_and_stop_gateway_for_update();
                                if let Err(error) = &stopped {
                                    // **News, not a probe line.** This is what
                                    // stops the install a moment later, and a
                                    // person reading `updater.log` alone cannot
                                    // see why.
                                    tracing::warn!(
                                        target: "koru::gateway_host",
                                        %error,
                                        "the gateway did not confirm a clean Store shutdown; the install is refused"
                                    );
                                }
                                // The MCP bridge is the one shipped executable
                                // with no stop CLI and a foreign parent, so it
                                // is killed rather than asked — on this thread
                                // for the same reason the gateway stop is.
                                if let Some(install) = std::env::current_exe()
                                    .ok()
                                    .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                                {
                                    for pid in crate::update::kill_mcp_bridges(&install) {
                                        routing_log.event(format_args!(
                                            "install handoff killed {} pid={pid}",
                                            koru_update::MCP_EXE
                                        ));
                                    }
                                }
                                if let Ok(mut slot) = done.lock() {
                                    *slot = Some(
                                        stopped
                                            .map(|report| report.to_string())
                                            .map_err(|error| error.to_string()),
                                    );
                                }
                                waker.wake(crate::event::AgentEvent::UpdateChanged);
                            });
                    }
                }
            }
            Handoff::Ready => self.finish_install(),
            Handoff::Aborted(why) => {
                let why = why.clone();
                // **Leave the update window.** The handoff held the gateway's
                // supervision before stopping it, so without this the service
                // stays parked for the rest of the run — an aborted install
                // that leaves the Store down is a worse outcome than the
                // install that did not happen. Off-thread, because a resume
                // that has to replace a child spawns a process.
                let _ = std::thread::Builder::new()
                    .name("koru-agent resume gateway".into())
                    .spawn(crate::update::resume_gateway_after_abort);
                if let Some(lifecycle) = self.session_lifecycle.clone() {
                    let _ = std::thread::Builder::new()
                        .name("koru-agent cancel session shutdown".into())
                        .spawn(move || {
                            // A failure here means session admission may
                            // still be frozen — say so rather than letting
                            // the abort read as fully recovered.
                            if let Err(error) = lifecycle.cancel() {
                                tracing::warn!(
                                    target: "koru::session_host",
                                    error,
                                    "could not reopen session admission after the aborted install"
                                );
                            }
                        });
                }
                // Every abort funnels through here — a refusal, the 120 s
                // no-progress expiry, another instance holding the
                // installation, a launch failure — so one line covers them
                // all, with the reason it will also show in the banner.
                self.routing_log
                    .event(format_args!("install aborted: {why}"));
                if let Some(controller) = &self.updates {
                    controller.install_blocked(why.clone());
                }
                // The abort happens *after* every window was asked to close,
                // so the banner carrying `install_blocked` has nobody to show
                // it to — say it in a dialog, or "Yes" reads as nothing
                // happening while the windows quietly come back.
                #[cfg(target_os = "macos")]
                if self.connections.is_empty() {
                    crate::update::announce_install_blocked(why);
                }
                #[cfg(not(target_os = "macos"))]
                let _ = why;
                self.handoff = Handoff::Idle;
                self.broadcast_update();
                // The chat stop worker persists intent before asking the
                // process to exit. If its wait then fails, its cloned record
                // can be newer than this in-memory copy; merge rather than
                // overwrite that evidence during abort recovery.
                {
                    let persist = self
                        .reopen_io
                        .lock()
                        .map_err(|_| "the reopen persistence lock was poisoned".to_string());
                    if let Ok(_guard) = persist {
                        let durable = crate::update::Reopen::load(&self.paths);
                        self.reopen.ai_chat |= durable.ai_chat;
                        for window in durable.windows {
                            if !self.reopen.windows.contains(&window) {
                                self.reopen.windows.push(window);
                            }
                        }
                        self.reopen.retain_closed(&self.registry.shells());
                        let _ = self.reopen.save(&self.paths);
                    } else {
                        koru_ipc::ipc_debug!(
                            "update: reopen persistence lock was poisoned during abort recovery"
                        );
                    }
                }
                self.reopen_pending.store(
                    !self.reopen.is_empty(),
                    std::sync::atomic::Ordering::Release,
                );
                self.handoff_recovering = true;
                self.ensure_gateway_off_thread(None);
            }
        }
    }

    /// Everything is down: hand over and go.
    fn finish_install(&mut self) {
        let Some(controller) = self.updates.clone() else {
            return;
        };
        let Some(pending) = controller.take_pending_install() else {
            crate::update::Reopen::clear(&self.paths);
            self.handoff = crate::update::Handoff::Idle;
            return;
        };
        // A window this agent never started still holds the installation.
        let others = self
            .update_lease
            .as_ref()
            .map_or(0, koru_update::InstanceLease::other_instances);
        if others > 0 {
            self.handoff = crate::update::Handoff::Aborted(format!(
                "{others} other Koru instance(s) are using this installation"
            ));
            self.advance_handoff();
            return;
        }
        self.routing_log
            .event(format_args!("install handing over to the updater"));
        if let Err(error) = crate::update::launch_updater(
            pending,
            controller.paths(),
            self.paths.explicit_root().map(std::path::Path::to_path_buf),
            self.rehearsal_manifest.as_deref(),
        ) {
            self.handoff = crate::update::Handoff::Aborted(error);
            self.advance_handoff();
            return;
        }
        // **Drop the shared lease before leaving**, or the updater's exclusive
        // acquire fails against a process that is on its way out — and note
        // there is no `process::exit` on this path for exactly that reason: a
        // leaked lease reads as "another Koru is using this installation"
        // forever.
        self.update_lease = None;
        self.updates = None;
        self.exit = true;
    }

    fn drop_connection(&mut self, id: ConnectionId) {
        if let Some(coordinator) = &self.global_dictation {
            coordinator.shell_gone(id);
        }
        if self
            .workspace_dictation_route
            .is_some_and(|(owner, _)| owner == id)
        {
            self.workspace_dictation_route = None;
        }
        let showing = self.registry.shell(id).map(|shell| {
            format!(
                "workspace profile={} workspace={} pid={:?}",
                shell.profile.to_storage(),
                shell.workspace.to_storage(),
                shell.pid
            )
        });
        self.connections.retain(|c| c.id() != id);
        self.peer_pids.remove(&id);
        self.reopen_claims.remove(&id);
        let removed = self.registry.gone(id);
        for pending in &mut self.reminder_snoozes {
            if pending.assigned == Some(id) {
                pending.assigned = None;
            }
        }
        self.routing_log.event(format_args!(
            "connection gone connection={} removed={} showing={} connections={} workspace_windows={} pending={} handoff={}",
            id.0,
            removed,
            showing.as_deref().unwrap_or("unknown"),
            self.connections.len(),
            self.registry.len(),
            self.router.pending_count(),
            // Which install phase, if any, this window left during — an
            // ordinary close and an answered close request are otherwise the
            // same line.
            self.handoff.label(),
        ));
    }

    /// Answer a request. Queued, never written — see [`crate::connection`].
    fn reply(&mut self, connection: ConnectionId, id: u64, body: Response) {
        let alive = self
            .connection(connection)
            .is_some_and(|c| c.send(FromAgent::Response { id, body }));
        if !alive {
            // A socket that will not take a byte is one nobody is holding. The
            // reader will say so too; `gone` is idempotent.
            self.drop_connection(connection);
        }
    }

    fn said(
        &mut self,
        connection: ConnectionId,
        id: u64,
        body: Request,
        queued: std::time::Duration,
    ) {
        if self.restart_gate_pending() && starts_process_during_restart(&body) {
            self.reply(
                connection,
                id,
                Response::Failed {
                    code: "update_in_progress".into(),
                    message: koru_i18n::t(
                        koru_i18n::Lang::for_system(
                            koru_window::system_locale::system_locale().as_deref(),
                        ),
                        koru_i18n::Key::UpdateInstallInProgress,
                    )
                    .into(),
                },
            );
            return;
        }
        if self.handoff.in_progress() && starts_work_while_quiescing(&body) {
            self.reply(
                connection,
                id,
                Response::Failed {
                    code: "update_in_progress".into(),
                    message: "Koru is closing services to install an update".into(),
                },
            );
            return;
        }
        match body {
            Request::Account {
                command,
                expected_owner_scope,
            } => {
                // Plan 3.5's account conversation is independent of workspace
                // registration: in particular this is intentionally valid
                // before `Hello` for a zero-Profile shell.
                //
                // Every command but the status poll is logged on arrival.
                // The pushes were, the requests were not, so a click that
                // produced no result left nothing to read. `Debug` on the
                // command is the redacted spelling.
                if !matches!(command, koru_agent_wire::AccountCommand::Status) {
                    self.routing_log.event(format_args!(
                        "account command connection={} command={:?} scope={:?}",
                        connection.0, command, expected_owner_scope
                    ));
                }
                if matches!(command, koru_agent_wire::AccountCommand::Status) {
                    let state = self
                        .account
                        .as_ref()
                        .map(crate::account::AccountControl::state)
                        .unwrap_or_else(|| {
                            koru_agent_wire::AccountState::without_capability(self.cloud_configured)
                        });
                    self.reply(connection, id, Response::Account { state });
                } else if let Some(control) = &self.account {
                    if expected_owner_scope.is_none()
                        && !matches!(
                            command,
                            koru_agent_wire::AccountCommand::SwitchAccount { .. }
                                | koru_agent_wire::AccountCommand::AddAccount { .. }
                        )
                    {
                        self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "account_scope_missing".into(),
                                message: "the account view must supply its operation owner".into(),
                            },
                        );
                        return;
                    }
                    match control.enqueue_owned(connection.0, expected_owner_scope, command) {
                        Ok(admission) => {
                            let generation = admission.generation;
                            self.account_generations.insert(connection, generation);
                            self.reply(
                                connection,
                                id,
                                Response::AccountQueued {
                                    generation,
                                    owner_scope: admission.owner_scope,
                                },
                            );
                        }
                        Err(crate::account::EnqueueError::ScopeChanged) => self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "account_scope_changed".into(),
                                message:
                                    "the account view changed before this operation was admitted"
                                        .into(),
                            },
                        ),
                        Err(crate::account::EnqueueError::Full) => self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "account_queue_full".into(),
                                message: "the bounded account worker queue is full".into(),
                            },
                        ),
                        Err(crate::account::EnqueueError::NotOwner) => self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "account_cancel_not_owner".into(),
                                message: "the account operation belongs to another connection"
                                    .into(),
                            },
                        ),
                    }
                } else {
                    self.reply(
                        connection,
                        id,
                        Response::Account {
                            state: koru_agent_wire::AccountState::without_capability(
                                self.cloud_configured,
                            ),
                        },
                    );
                }
            }
            Request::RemoveLocalProfile { profile_id } => {
                let Some(profile) = koru_store_core::ids::ProfileId::parse(&profile_id) else {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "invalid_id".into(),
                            message: "profile_id is not a canonical Profile UUID".into(),
                        },
                    );
                    return;
                };
                let Some(waker) = self.event_waker.clone() else {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "internal".into(),
                            message: "the Agent event loop is not ready".into(),
                        },
                    );
                    return;
                };
                service::remove_local_profile(self.paths.clone(), waker, (connection, id), profile);
            }
            Request::BindLocalProfileAccount {
                profile_id,
                expected_identity_id,
                new_identity_id,
            } => {
                let Some(profile) = koru_store_core::ids::ProfileId::parse(&profile_id) else {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "invalid_id".into(),
                            message: "profile_id is not a canonical Profile UUID".into(),
                        },
                    );
                    return;
                };
                let Some(new) = profile_account_binding(&new_identity_id) else {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "invalid_id".into(),
                            message: "new_identity_id is not a canonical Koru Identity UUID".into(),
                        },
                    );
                    return;
                };
                let expected = match expected_identity_id.as_deref() {
                    Some(value) => {
                        let Some(binding) = profile_account_binding(value) else {
                            self.reply(
                                connection,
                                id,
                                Response::Failed {
                                    code: "invalid_id".into(),
                                    message:
                                        "expected_identity_id is not a canonical Koru Identity UUID"
                                            .into(),
                                },
                            );
                            return;
                        };
                        Some(binding)
                    }
                    None => None,
                };
                let Some(waker) = self.event_waker.clone() else {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "internal".into(),
                            message: "the Agent event loop is not ready".into(),
                        },
                    );
                    return;
                };
                service::bind_local_profile_account(
                    self.paths.clone(),
                    waker,
                    (connection, id),
                    profile,
                    expected,
                    new,
                );
            }
            Request::UpdateChoice { choice } => {
                if let koru_agent_wire::UpdateChoice::Install { session_generation } = choice {
                    if let Err((code, message)) = self.prepare_sessions_for_install(
                        session_generation,
                        Some((connection, id)),
                        queued,
                    ) {
                        self.reply(connection, id, Response::Failed { code, message });
                    }
                } else {
                    match self.apply_update_choice(choice, queued) {
                        Ok(()) => self.reply(connection, id, Response::Ok),
                        // **Refused, not ignored.** A window that pressed a button and
                        // saw nothing happen cannot tell that from a hung agent — and
                        // the refusal is what makes the two-step consent a rule rather
                        // than an intention.
                        Err((code, message)) => {
                            self.reply(connection, id, Response::Failed { code, message })
                        }
                    }
                }
            }
            Request::EnsureService {
                service: Service::ApiGateway | Service::Store,
            } => {
                // **Not on this thread.** `ensure_running` spawns a process and
                // then retries for up to ten seconds while it binds, and the
                // loop may never block — a store that is slow to start would
                // otherwise freeze the tray, the hotkey and every other shell.
                // The reply is queued from the worker, which is what a writer
                // queue per connection is for.
                self.ensure_gateway_off_thread(Some((connection, id)));
            }
            Request::EnsureService {
                service: Service::Voice,
            } => {
                // **A dictation makes the worker available.** An installed
                // Agent normally warmed it to populate the input picker; this
                // remains the authoritative on-demand path after an idle exit,
                // a manual stop, or a launch without installed models.
                // Off the thread for the same reason the gateway is -- a launch
                // and a bind race may not freeze the tray, the hotkey and every
                // other shell.
                self.ensure_voice_off_thread(Some((connection, id)));
            }
            Request::Hello {
                profile,
                workspace,
                profile_name,
                workspace_name,
                pinned,
                reopen_claim,
            } => {
                let Some((profile, workspace)) = ids(&profile, &workspace) else {
                    self.routing_log.event(format_args!(
                        "hello rejected connection={} reason=bad_id",
                        connection.0
                    ));
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "bad_id".into(),
                            message: "a profile or workspace id that is not a uuid".into(),
                        },
                    );
                    return;
                };
                // **The pid comes off the transport**, recorded when the socket
                // was accepted — never from this message, which does not carry
                // one. See `koru_ipc::peer_pid`.
                let pid = self.peer_pids.get(&connection).copied().flatten();
                let pending = self.router.has_pending(profile, workspace);
                self.registry.hello_named(
                    connection,
                    pid,
                    profile,
                    workspace,
                    &profile_name,
                    &workspace_name,
                );
                self.registry.set_pinned(connection, pinned);
                if let Some(claim) = reopen_claim.as_ref() {
                    self.reopen_claims.insert(connection, claim.clone());
                } else {
                    self.reopen_claims.remove(&connection);
                }
                self.acknowledge_reopened_window(profile, workspace, reopen_claim.as_deref());
                self.routing_log.event(format_args!(
                    "hello accepted connection={} pid={pid:?} profile={} workspace={} pending_match={} registry={}",
                    connection.0,
                    profile.to_storage(),
                    workspace.to_storage(),
                    pending,
                    self.registry.len()
                ));
                self.reply(connection, id, Response::Ok);
                // Account state is process-wide and the Agent commonly
                // outlives every shell. A late window needs the current state
                // immediately so exact lifecycle reconciliation does not
                // depend on first opening the Manage tab.
                if let Some(state) = self
                    .account
                    .as_ref()
                    .map(crate::account::AccountControl::state)
                {
                    self.push(connection, Push::Account { state });
                }
                // Anything that was waiting for this workspace to appear.
                if let Some(target) = self.router.claim(profile, workspace, Instant::now()) {
                    self.routing_log.event(format_args!(
                        "pending claimed connection={} profile={} workspace={} pending_after={}",
                        connection.0,
                        profile.to_storage(),
                        workspace.to_storage(),
                        self.router.pending_count()
                    ));
                    self.push(connection, Push::Open { target });
                } else if pending {
                    self.routing_log.event(format_args!(
                        "pending claim expired connection={} profile={} workspace={}",
                        connection.0,
                        profile.to_storage(),
                        workspace.to_storage()
                    ));
                }
                self.push_pending_reminder_snoozes(connection, profile, workspace);
                if self.reminder_opens_owed.remove(&profile) || self.reminder_tray_open_owed {
                    self.reminder_tray_open_owed = false;
                    self.push(connection, Push::OpenReminders);
                }
                // **Tell this window what is already known about updating.**
                //
                // The agent is a daemon and a window is not: it checks once a
                // few seconds after *it* starts and then once a day, while
                // shells come and go underneath it. Every other push is sent on
                // a *change*, so a window that connects between two changes —
                // which is nearly every window, since the agent usually
                // outlives several — would learn nothing until the next check.
                //
                // That is exactly how a published build sat unoffered: the agent
                // had found it hours earlier, told the windows that existed
                // then, and the window asking now had never been told anything.
                // A banner that depends on having been present at the right
                // moment is not a banner.
                self.push_update_to(connection);
                // And which chord opens Search, once the hotkey thread has
                // answered — a window that connected after registration still
                // learns it, exactly as it learns the update report.
                self.push_shortcut_to(connection);
                self.push_reminder_diagnostics_to(connection);
                // And whether there is an icon to hide behind — the window's
                // close button keeps it in the background only if there is.
                self.push_notification_area_to(connection);
                // **And, once, what the last install produced.**
                //
                // Read off disk rather than held in memory because the process
                // that installed is not this one. Taken rather than read, so it
                // reaches exactly one window and never opens a tab again — the
                // agent outlives many shells and every one of them would
                // otherwise be greeted by an update that happened last Tuesday.
                self.push_whats_new_to(connection);
                // And a voice consent step the notification area asked for
                // while there was no window to ask it in.
                self.push_voice_setup_to(connection);
                self.push_voice_input_to(connection);
                self.push_feedback_to(connection);
                // An Agents-menu ask made while there was no window to send it
                // to — the launch this Hello answers.
                self.push_workspace_session_to(connection);
                // A hosted commit survives both processes. Reconnect the
                // shell to that durable workflow before it can treat the
                // Profile as an ordinary active one again.
                self.push_pending_profile_cleanup_to(connection);
                // A window arriving is one of the moments the session catalog
                // plausibly moved without the watcher hearing it.
                self.kick_agents_menu();
            }
            Request::WorkspaceChanged {
                profile,
                workspace,
                profile_name,
                workspace_name,
                pinned,
            } => {
                if let Some((profile, workspace)) = ids(&profile, &workspace) {
                    self.registry.workspace_changed_named(
                        connection,
                        profile,
                        workspace,
                        &profile_name,
                        &workspace_name,
                    );
                    self.registry.set_pinned(connection, pinned);
                    self.routing_log.event(format_args!(
                        "workspace changed connection={} profile={} workspace={} registry={}",
                        connection.0,
                        profile.to_storage(),
                        workspace.to_storage(),
                        self.registry.len()
                    ));
                }
                self.reply(connection, id, Response::Ok);
            }
            Request::Active => {
                self.registry.active(connection);
                self.reply(connection, id, Response::Ok);
                // A focus change is the cheapest observable moment at which
                // sessions were plausibly created or ended in the window the
                // person just left — the agents-menu gap the module note on
                // `crate::agents_menu` names. Coalesced; nearly free.
                self.kick_agents_menu();
            }
            Request::DictationState { route, state } => {
                if !workspace_state_authenticated(
                    &self.registry,
                    self.workspace_dictation_route,
                    connection,
                    route,
                ) {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "dictation_route_mismatch".into(),
                            message: "dictation state does not belong to this connection".into(),
                        },
                    );
                    return;
                }
                let local = match state {
                    koru_agent_wire::DictationState::Refused => {
                        crate::dictation_coordinator::LocalState::Refused
                    }
                    koru_agent_wire::DictationState::Listening { mode } => {
                        crate::dictation_coordinator::LocalState::Listening(match mode {
                            koru_agent_wire::DictationMode::PushToTalk => {
                                koru_dictation::Mode::PushToTalk
                            }
                            koru_agent_wire::DictationMode::Toggle => koru_dictation::Mode::Toggle,
                        })
                    }
                    koru_agent_wire::DictationState::Finalising => {
                        crate::dictation_coordinator::LocalState::Finalising
                    }
                    koru_agent_wire::DictationState::Idle => {
                        crate::dictation_coordinator::LocalState::Idle
                    }
                };
                if let Some(coordinator) = &self.global_dictation {
                    coordinator.local_state(
                        crate::event::LocalDictationOwner::Workspace(connection),
                        route,
                        local,
                    );
                }
                if matches!(
                    local,
                    crate::dictation_coordinator::LocalState::Idle
                        | crate::dictation_coordinator::LocalState::Refused
                ) {
                    self.workspace_dictation_route = None;
                }
                self.reply(connection, id, Response::Ok);
            }
            Request::ShowOverlay => {
                self.open_overlay();
                self.reply(connection, id, Response::Ok);
            }
            Request::OpenWorkspace {
                profile,
                inherit_stdio,
            } => {
                // The Agent must own this spawn so the new Koru process is
                // entered in the first-party launch table before it asks for
                // workspace lifecycle scopes. The rooted development launcher
                // uses this exact route instead of starting `koru.exe` beside
                // the Agent and producing an inevitably unvouched window.
                let response = match self.open_workspace(profile.as_deref(), inherit_stdio) {
                    Ok(pid) => Response::WorkspaceOpened { pid },
                    Err(error) => Response::Failed {
                        code: "workspace_launch_failed".into(),
                        message: error.to_string(),
                    },
                };
                self.reply(connection, id, response);
            }
            Request::Route { target } => {
                match from_wire(target) {
                    // The same path the overlay's own choice takes, so there is
                    // one router and one set of rules rather than two that
                    // drift.
                    Some(target) => {
                        let koru_search_ui::Target::Store {
                            profile,
                            workspace,
                            document,
                            node,
                            disposition,
                            ..
                        } = &target
                        else {
                            unreachable!("wire routing only constructs Store targets")
                        };
                        self.routing_log.event(format_args!(
                            "route requested by connection={} profile={} workspace={} document={} node_present={} new_window={}",
                            connection.0,
                            profile.to_storage(),
                            workspace.to_storage(),
                            document.map_or_else(|| "none".into(), |id| id.to_storage()),
                            node.is_some(),
                            matches!(disposition, koru_search_ui::Disposition::NewWindow)
                        ));
                        self.open_target(target);
                        self.reply(connection, id, Response::Ok);
                    }
                    None => {
                        self.routing_log.event(format_args!(
                            "route rejected connection={} reason=bad_id",
                            connection.0
                        ));
                        self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "bad_id".into(),
                                message: "a target whose ids are not uuids".into(),
                            },
                        )
                    }
                }
            }
            Request::OpenLink { document, node } => {
                match koru_store_core::ids::DocumentId::parse(&document) {
                    // No waker means no loop to wake -- the agent is under
                    // test, or head-less in a way that cannot route anything
                    // anyway. Saying so beats resolving into a void.
                    Some(document) if self.event_waker.is_some() => {
                        self.routing_log.event(format_args!(
                            "link requested by connection={} document={} node_present={}",
                            connection.0,
                            document.to_storage(),
                            node.is_some()
                        ));
                        // Accepted for resolution. Where it lives is a question
                        // for the Store, and asking it is not this thread's
                        // work -- see `service::resolve_link`.
                        crate::service::resolve_link(
                            self.paths.clone(),
                            self.event_waker.clone().expect("checked above"),
                            document,
                            node,
                        );
                        self.reply(connection, id, Response::Ok);
                    }
                    Some(_) => self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "not_routable".into(),
                            message: "this agent has no event loop to open a window from".into(),
                        },
                    ),
                    None => {
                        self.routing_log.event(format_args!(
                            "link rejected connection={} reason=bad_link",
                            connection.0
                        ));
                        self.reply(
                            connection,
                            id,
                            Response::Failed {
                                code: "bad_link".into(),
                                message: "a link whose document id is not a uuid".into(),
                            },
                        )
                    }
                }
            }
            // **A build that hides Koru AI opens nothing**, whoever asks: the
            // tray row, the chord and the in-document link were gated in the
            // shells, but a `koru://chat/…` from Mail or Safari (LaunchServices
            // on a Mac, `koru-agent koru://…` on Windows) reached these two
            // arms and opened the hidden chat. `Ok` is silent in the shell, so
            // the link simply does nothing (koru-features, M39).
            Request::OpenChatLink { .. } | Request::OpenAiChat if !koru_features::ai() => {
                self.reply(connection, id, Response::Ok);
            }
            Request::OpenChatLink { chat, block } => {
                let parsed = uuid::Uuid::parse_str(&chat)
                    .ok()
                    .zip(block.as_deref().map(uuid::Uuid::parse_str).transpose().ok());
                match parsed {
                    Some((chat, block)) => {
                        let reply = self.connection(connection).map(Connection::sender);
                        let open = crate::ai_chat::Open::Chat { chat, block };
                        if let Some(sender) = reply {
                            crate::ai_chat::open_off_thread(
                                self.paths.explicit_root().map(Path::to_path_buf),
                                open,
                                crate::ai_chat::OpenCompletion::Shell { sender, id },
                            );
                        } else {
                            self.open_ai_chat_for_agent(open);
                        }
                    }
                    None => self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "bad_link".into(),
                            message: "a chat link whose ids are not uuids".into(),
                        },
                    ),
                }
            }
            Request::OpenAiChat => {
                let reply = self.connection(connection).map(Connection::sender);
                if let Some(sender) = reply {
                    crate::ai_chat::open_off_thread(
                        self.paths.explicit_root().map(Path::to_path_buf),
                        crate::ai_chat::Open::Landing,
                        crate::ai_chat::OpenCompletion::Shell { sender, id },
                    );
                } else {
                    self.open_ai_chat_for_agent(crate::ai_chat::Open::Landing);
                }
            }
            Request::Status => {
                let body = Response::Status {
                    agent_version: env!("CARGO_PKG_VERSION").into(),
                    device_id: self.device_id.clone(),
                    instance_id: self.instance_id.clone(),
                    shells: self.registry.len(),
                    pending_launches: self.router.pending_count(),
                    owns_the_machine: self.machine_input_enabled,
                    sync: self.sync_status.clone(),
                    reminders: reminder_diagnostics(&self.reminder_report),
                };
                self.reply(connection, id, body);
            }
            Request::ReminderStatus => {
                self.reply(
                    connection,
                    id,
                    Response::ReminderStatus {
                        reminders: reminder_diagnostics(&self.reminder_report),
                    },
                );
            }
            Request::StartPairing => {
                let body = match self.sync_identity.as_ref() {
                    Some(identity) => {
                        self.joining = None;
                        self.pairing = Some(koru_pairing::InvitationSession::new(
                            identity.descriptor.clone(),
                            None,
                            std::time::Instant::now(),
                        ));
                        if let (Some(control), Some(pairing)) = (&self.sync_control, &self.pairing)
                        {
                            control.begin_pairing(pairing.invitation.clone());
                        }
                        self.pairing_response()
                    }
                    None => Response::Failed {
                        code: "identity_unavailable".into(),
                        message: "sync identity is not available".into(),
                    },
                };
                self.reply(connection, id, body);
            }
            Request::AcceptLinkCode { code } => {
                let failed = |code: &str| Response::Pairing {
                    view: Box::new(koru_agent_wire::PairingView {
                        state: koru_agent_wire::PairingState::Failed,
                        qr_payload: None,
                        expires_in_seconds: 0,
                        peer_name: None,
                        error_code: Some(code.to_owned()),
                    }),
                };
                let body = match koru_pairing::PairingInvitationV1::decode_link(&code.0) {
                    Ok(invitation) => match (&self.sync_identity, &self.sync_control) {
                        (Some(identity), Some(_))
                            if invitation.inviter.device_id == identity.descriptor.device_id =>
                        {
                            failed("same_device")
                        }
                        (Some(identity), Some(control)) => {
                            match koru_pairing::PairingJoinV1::sign(
                                &invitation,
                                identity.descriptor.clone(),
                                &identity.signing_key,
                                wall_time_millis(),
                            ) {
                                Ok(join) => {
                                    self.pairing = None;
                                    self.joining = Some(koru_agent_wire::PairingView {
                                        state: koru_agent_wire::PairingState::Connecting,
                                        qr_payload: None,
                                        expires_in_seconds: 0,
                                        peer_name: Some(invitation.inviter.display_name.clone()),
                                        error_code: None,
                                    });
                                    control.join_pairing(join);
                                    self.pairing_response()
                                }
                                Err(_) => failed("pairing_signature"),
                            }
                        }
                        _ => failed("identity_unavailable"),
                    },
                    Err(_) => failed("invalid_link_code"),
                };
                self.reply(connection, id, body);
            }
            Request::PairingStatus => {
                self.reply(connection, id, self.pairing_response());
            }
            Request::CancelPairing => {
                if let Some(pairing) = &mut self.pairing {
                    pairing.cancel();
                }
                self.joining = None;
                if let Some(control) = &self.sync_control {
                    control.cancel_pairing();
                }
                self.reply(connection, id, self.pairing_response());
            }
            Request::LinkedDevices => {
                self.reply(connection, id, self.linked_devices_response());
            }
            Request::DeviceHistory { device_id, before } => {
                let body = self.device_history_response(&device_id, before);
                self.reply(connection, id, body);
            }
            Request::UnlinkDevice { device_id } => {
                let body = self.unlink_device(&device_id);
                self.reply(connection, id, body);
            }
            Request::RenameDevice { name } => {
                let body = self.rename_device(&name);
                self.reply(connection, id, body);
            }
            Request::SetRelay { enabled } => {
                let body = match crate::network::current() {
                    Some(network) => {
                        network.set_relay_enabled(enabled);
                        self.linked_devices_response()
                    }
                    None => Response::Failed {
                        code: "relay_unavailable".into(),
                        message: "the device network is unavailable".into(),
                    },
                };
                self.reply(connection, id, body);
            }
            Request::ReminderActivation { envelope } => {
                let accepted = self
                    .reminder_activation_ingress
                    .as_ref()
                    .ok_or(crate::reminders::ActivationIngressError::Busy)
                    .and_then(|ingress| ingress.accept_opaque(&envelope));
                let body = match accepted {
                    Ok(()) => Response::Ok,
                    Err(crate::reminders::ActivationIngressError::Invalid) => Response::Failed {
                        code: "bad_notification_activation".into(),
                        message: "notification activation is invalid or unsupported".into(),
                    },
                    Err(crate::reminders::ActivationIngressError::Busy) => Response::Failed {
                        code: "notification_activation_busy".into(),
                        message: "notification activation was not accepted; retry it".into(),
                    },
                };
                self.reply(connection, id, body);
            }
            Request::NotificationControl { operation } => {
                let Some(ingress) = self.reminder_control_ingress.as_ref() else {
                    self.reply(
                        connection,
                        id,
                        Response::NotificationControl {
                            result: koru_agent_wire::NotificationControlResult::Failed {
                                code: koru_agent_wire::NotificationFailure::Unavailable,
                            },
                        },
                    );
                    return;
                };
                let request = match notification_control_request(operation) {
                    Ok(request) => request,
                    Err(_) => {
                        self.reply(
                            connection,
                            id,
                            Response::NotificationControl {
                                result: koru_agent_wire::NotificationControlResult::Failed {
                                    code: koru_agent_wire::NotificationFailure::InvalidRequest,
                                },
                            },
                        );
                        return;
                    }
                };
                let Some(waker) = self.event_waker.clone() else {
                    self.reply(
                        connection,
                        id,
                        Response::NotificationControl {
                            result: koru_agent_wire::NotificationControlResult::Failed {
                                code: koru_agent_wire::NotificationFailure::Unavailable,
                            },
                        },
                    );
                    return;
                };
                let reply = match ingress.notification_control(request) {
                    Ok(reply) => reply,
                    Err(_) => {
                        self.reply(
                            connection,
                            id,
                            Response::NotificationControl {
                                result: koru_agent_wire::NotificationControlResult::Failed {
                                    code: koru_agent_wire::NotificationFailure::Busy,
                                },
                            },
                        );
                        return;
                    }
                };
                let spawned = std::thread::Builder::new()
                    .name("koru notification control reply".into())
                    .spawn(move || {
                        let result = reply
                            .recv_timeout(std::time::Duration::from_secs(15))
                            .map(notification_control_result)
                            .unwrap_or_else(|_| {
                                koru_agent_wire::NotificationControlResult::Failed {
                                    code: koru_agent_wire::NotificationFailure::Timeout,
                                }
                            });
                        waker.wake(AgentEvent::NotificationControlAnswered {
                            connection,
                            id,
                            result,
                        });
                    });
                if spawned.is_err() {
                    self.reply(
                        connection,
                        id,
                        Response::NotificationControl {
                            result: koru_agent_wire::NotificationControlResult::Failed {
                                code: koru_agent_wire::NotificationFailure::Unavailable,
                            },
                        },
                    );
                }
            }
            Request::AcceptReminderSnooze { delivery } => {
                let accepted = self
                    .reminder_snoozes
                    .iter()
                    .position(|pending| {
                        pending.intent.delivery == delivery && pending.assigned == Some(connection)
                    })
                    .and_then(|index| self.reminder_snoozes.remove(index))
                    .is_some();
                self.reply(
                    connection,
                    id,
                    if accepted {
                        Response::Ok
                    } else {
                        Response::Failed {
                            code: "unknown_reminder_snooze".into(),
                            message: "the snooze delivery is not assigned to this shell".into(),
                        }
                    },
                );
            }
            Request::RequestFirstPartyGrant => {
                // **The pid is the operating system's answer**, read off the
                // transport when this socket was accepted — the same rule
                // foregrounding follows, and for a stronger reason: a grant
                // handed to a self-reported pid is a grant handed to whoever
                // guessed one.
                let pid = self.peer_pids.get(&connection).copied().flatten();
                let body = match crate::grants::mint(pid) {
                    Ok(grant) => Response::FirstPartyGrant { grant },
                    Err(crate::grants::Refusal::NotLaunchedHere) => Response::Failed {
                        code: "not_first_party".into(),
                        message: "this Agent did not launch the process asking".into(),
                    },
                };
                self.routing_log.event(format_args!(
                    "first-party grant connection={} pid={pid:?} minted={}",
                    connection.0,
                    matches!(body, Response::FirstPartyGrant { .. })
                ));
                self.reply(connection, id, body);
            }
            Request::ValidateFirstPartyGrant { grant } => {
                // **Answered to anybody who asks**, deliberately. Validation
                // reveals nothing a caller did not already have: it takes a
                // token and returns whether that exact token was live, so a
                // process without one learns only that its guess was wrong.
                // Restricting it would mean the Gateway — which this Agent
                // launched but which reaches it as an ordinary wire client —
                // needed a credential of its own to check a credential.
                let valid = crate::grants::validate(grant.expose());
                self.reply(connection, id, Response::FirstPartyGrantValidity { valid });
            }
            Request::ResetIdentity => {
                // Only a broken identity starts over; a working one is what
                // every linked device trusts. The reset itself runs at the next
                // start (`IdentityManager::reset_if_requested`), before anything
                // holds `agent.db` open — so, like `Shutdown`, success is this
                // Agent stopping, and the shell relaunches onto the new identity.
                if self.sync_identity.is_some() {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: "identity_intact".into(),
                            message: "this device's identity is intact".into(),
                        },
                    );
                } else if let Err(error) =
                    crate::identity::IdentityManager::request_reset(&self.paths)
                {
                    self.reply(
                        connection,
                        id,
                        Response::Failed {
                            code: error.code.to_owned(),
                            message: "the identity reset could not be requested".into(),
                        },
                    );
                } else {
                    koru_ipc::ipc_debug!("identity: reset requested; stopping for it");
                    self.stop_sessions_then_quit();
                }
            }
            Request::Shutdown => {
                // **No reply.** The socket closing is the answer, and an agent
                // that acknowledged a shutdown would have to still be running
                // to send it. `should_exit` is read by the loop on its way
                // round, which is also what drops every other connection.
                //
                // **This used to leave the store alone**, on the grounds that
                // the caller that mattered was the updater's `quiesce`, which
                // asks the agent and the store separately so it can watch each
                // one go. That reasoning does not survive contact with the
                // other callers: `koru-agent.exe stop` is also what a person
                // types, and it left a headless daemon behind with nothing on
                // screen to say so. Nor does `quiesce` actually mind — its
                // second step finds a store already stopped, and
                // `stop_store` treats that as the outcome it wanted.
                koru_ipc::ipc_debug!("shutdown: asked over the socket");
                self.quit_because_asked();
            }
        }
    }

    /// Returns whether the frame reached a live connection. Ignored almost
    /// everywhere — a dead connection is dropped here and there is nothing else
    /// to do about it — and read by exactly one caller, which is holding a
    /// message it may only forget once.
    fn push(&mut self, connection: ConnectionId, body: Push) -> bool {
        let kind = match &body {
            // The code and the enrollment state are what a support
            // conversation needs beside the stage: a window showing "Koru
            // can't connect right now" is one of a dozen codes, and the
            // stage alone cannot say which. Codes only, never a value.
            Push::Account { state } => format!(
                "account stage={:?} failure={:?} enrollment={:?}",
                state.stage, state.failure_code, state.enrollment
            ),
            Push::AccountBrowser { .. } => "account-browser".to_string(),
            Push::AccountResult { .. } => "account-result".to_string(),
            Push::AccountChanged => "account-changed".to_string(),
            Push::ProfileLifecycleDeleted { .. } => "profile-lifecycle-deleted".to_string(),
            Push::Restore => "restore".to_string(),
            Push::Voice => "voice".to_string(),
            // **Whether, never which.** A device name is a person's hardware
            // and this line reaches a log.
            Push::VoiceInput { device } => match device {
                Some(_) => "voice-input device".to_string(),
                None => "voice-input system-default".to_string(),
            },
            Push::DictationGesture { .. } => "dictation-gesture".to_string(),
            // Whether it names a session or asks for a new one; never which
            // session — the id is routing data, and this line reaches a log.
            Push::ShowSession { session } => match session {
                Some(_) => "show_session".to_string(),
                None => "show_session new".to_string(),
            },
            Push::Open { target } => format!(
                "open profile={} workspace={} document={} node_present={}",
                target.profile,
                target.workspace,
                target.document.as_deref().unwrap_or("none"),
                target.node.is_some()
            ),
            Push::ReminderSnooze { intent } => format!(
                "reminder_snooze delivery={} profile={}",
                intent.delivery, intent.profile
            ),
            Push::ReminderDiagnostics { reminders } => format!(
                "reminder_diagnostics attention={} failed={}",
                reminders.attention.len(),
                reminders.failed
            ),
            Push::OpenReminders => "open_reminders".to_string(),
            // The stage and the build, never the notes. This log is attached to
            // reports, and a release title is somebody's copy rather than a
            // fact about the routing.
            Push::Update { report } => {
                format!(
                    "update stage={:?} build={}",
                    report.stage, report.build_number
                )
            }
            // A version is a fact about the build, not somebody's copy, so this
            // one is safe to name in full.
            Push::Updated { version } => format!("updated version={version}"),
            Push::ShowFeedback => "show-feedback".to_string(),
            Push::ShowSupport => "show-support".to_string(),
            Push::Pairing { view } => format!("pairing state={:?}", view.state),
            Push::Shortcut { shortcut } => {
                format!("shortcut {}", shortcut.as_deref().unwrap_or("none"))
            }
            Push::Quitting => "quitting".to_string(),
            Push::NotificationArea { present } => format!("notification_area present={present}"),
        };
        let alive = self
            .connection(connection)
            .is_some_and(|c| c.send(FromAgent::Push { body }));
        self.routing_log.event(format_args!(
            "push queued connection={} kind={} success={alive}",
            connection.0, kind
        ));
        if !alive {
            self.drop_connection(connection);
        }
        alive
    }

    fn push_pending_reminder_snoozes(
        &mut self,
        connection: ConnectionId,
        profile: ProfileId,
        workspace: WorkspaceId,
    ) {
        let deliveries: Vec<_> = self
            .reminder_snoozes
            .iter()
            .filter(|pending| {
                pending.assigned.is_none()
                    && pending.profile == profile
                    && pending.workspace == workspace
            })
            .map(|pending| (pending.intent.delivery.clone(), pending.intent.clone()))
            .collect();
        for (delivery, intent) in deliveries {
            if self.push(connection, Push::ReminderSnooze { intent })
                && let Some(pending) = self
                    .reminder_snoozes
                    .iter_mut()
                    .find(|pending| pending.intent.delivery == delivery)
            {
                pending.assigned = Some(connection);
            }
        }
    }

    /// Keep the shortcut's own keys out of the overlay it just opened.
    ///
    /// **All three parts of the chord, not just the letter.** The `K` typing
    /// itself into the search box is the half that is obvious; the half that is
    /// worse is Shift, whose *release* also goes to the newly focused window
    /// and is therefore never seen by anyone — so Shift reads as held, and
    /// moving the mouse selects. Both were reported, in those words, from one
    /// press.
    ///
    /// Both sides of each modifier, because `WM_HOTKEY` says a chord was
    /// pressed and never which Shift. Suppressing a key that was not down
    /// costs nothing: the entry expires, and the first real release clears it.
    fn quarantine_the_chord(&mut self, trigger: winit::keyboard::KeyCode) {
        use winit::keyboard::{KeyCode, PhysicalKey};
        self.quarantine.push(PhysicalKey::Code(trigger));
        for code in [
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
            KeyCode::AltLeft,
            KeyCode::AltRight,
        ] {
            self.quarantine.push(PhysicalKey::Code(code));
        }
    }

    fn open_overlay(&mut self) {
        if self.voice_window.has_hud()
            && self.global_dictation_snapshot.phase != crate::dictation_coordinator::Phase::Idle
        {
            return;
        }
        if self.voice_window.close_all() || self.window_transition.is_some() {
            if let Some(coordinator) = &self.global_dictation {
                coordinator.discard_prepared();
            }
            self.window_transition = Some(WindowTransition::ClosingSearch);
            return;
        }
        // A global hotkey by definition fires while another application is
        // frontmost, and on macOS `focus_window` cannot pull a background app
        // forward — the overlay would open behind whatever the person was
        // looking at, which reads exactly like nothing happening. A no-op
        // everywhere else.
        koru_window::activate_app();
        if self.overlay.is_none() {
            // **The caret this overlay draws is the one the main app draws.**
            // The agent is a separate process and cannot see the shell's
            // globals, so it takes the authority's answer — read on the link's
            // own thread, never here. Draining first means an overlay opened
            // right after somebody changed the caret shows the new one.
            // An explicit test/development root must not inspect the real
            // user's settings, exactly as before.
            if self.paths.owns_the_machine() {
                let _ = self.settings.poll();
                koru_window::CaretAppearance::from_section(self.settings.authoritative()).apply();
            }
            // **The agent's own overlay needs the store as much as a shell
            // does**, and nothing was asking for it: pressing the hotkey with
            // no store running gave an empty search with no explanation, which
            // is exactly the failure this stage exists to remove.
            self.ensure_gateway_off_thread(None);
            // Gateway discovery is at the channel root that
            // `ensure_gateway_off_thread` above has just started. Under `--root`
            // this is what keeps the gate's
            // overlay off the developer's real index.
            self.overlay = Some(koru_search_ui::SearchSurface::new_at(
                self.paths.explicit_root().map(std::path::Path::to_path_buf),
            ));
            // The observation, on its own thread: a client connect is I/O the
            // event loop must not wait on, and an overlay that opens with no
            // gateway simply goes unrecorded. Fires only when the overlay is
            // genuinely created, not on every re-show.
            let paths = self.paths.clone();
            let _ = std::thread::Builder::new()
                .name("koru-agent events emit".into())
                .spawn(move || {
                    if let Ok(client) =
                        koru_api_client::Client::connect_at(crate::segment(), paths.root())
                        && client.events().available()
                    {
                        let _ = client.events().send(koru_events::SearchOpened {
                            surface: koru_events::SearchSurface::Overlay,
                        });
                    }
                });
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.set_pinned(&self.registry.pinned_rows());
        }
        {
            // **Bring the overlay forward ourselves.** `koru_window::open`
            // calls `window.focus_window()` when the window is created, but the
            // foreground lock refuses a background process — which this agent
            // is when a shell asks for the overlay through `ShowOverlay`. Grant
            // the ability first; the hotkey and the tray are already
            // foreground-eligible through their own input, and this covers the
            // case where they are not.
            #[cfg(windows)]
            {
                use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
                let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
            }
            koru_ipc::ipc_debug!("overlay opening via ShowOverlay"); // Seed the height we are about to open at. Relying only on a later
            // `Resized` left `windows.conf` with `x;y` and no `h` whenever the
            // person moved the panel without a successful vertical drag.
            self.search_geometry
                .resized(search_window_height(&self.search_geometry));
            self.ignore_blur_until = Some(Instant::now() + Duration::from_millis(400));
        }
    }

    /// End the workers this Agent owns, on the way out.
    ///
    /// **Every way of ending the agent goes through here**, and that is the
    /// point: the tray's *Quit* and `koru-agent.exe stop` are the same promise
    /// made in two places, and they had drifted into two answers.
    ///
    /// The tray icon is the only handle a person has on this stack. The Store
    /// draws nothing, has no window and no icon of its own, so an Agent that
    /// quit and left it behind left a process running that the person had no
    /// way to see and no reason to expect — after using the one control that
    /// says *Quit*.
    ///
    /// **The gateway is asked unconditionally now, and the open-window
    /// exemption is gone with the reason for it.** It existed because a
    /// gateway outlived the Agent that started it, so leaving it up kept every
    /// window online; since Plan 4 the gateway is a supervised child of this
    /// process and the Job Object ends it with us whatever we do here. Skipping
    /// the ask would not keep it alive — it would only replace a graceful
    /// Store shutdown with a kill. So the honest thing is to ask.
    ///
    /// **Fencing property 1: the stop is not generation-fenced.** *This service
    /// should not be running* is true of whichever child is current, including
    /// one that replaced the child this Agent last observed.
    ///
    /// **The winit thread does not wait.** The stop blocks — it asks the child
    /// over its public lifecycle API and waits for the process to be reaped —
    /// so it runs on a lifecycle thread, and the Job Object is the fallback for
    /// anything still alive when this process goes.
    fn stop_the_workers_it_owns(&mut self, why: &str) {
        // The voice worker is a sibling process, not a child, and it really is
        // left behind: it would end itself after half an hour idle, and half an
        // hour of a microphone-capable process nobody asked for is not what
        // somebody who clicked *Quit* meant. Asking, never waiting.
        service::request_voice_stop(&self.paths);
        let Some(supervisor) = crate::gateway_host::current() else {
            return;
        };
        let reason = why.to_owned();
        let routing_log = self.routing_log.clone();
        let (done, finished) = std::sync::mpsc::sync_channel::<()>(1);
        if std::thread::Builder::new()
            .name("koru-agent quit gateway stop".into())
            .spawn(move || {
                // The delegate worker first: it reaches the gateway, and a
                // run's last commit must not race the Store it goes to.
                if let Some(delegate) = crate::delegate_host::current() {
                    routing_log.event(format_args!(
                        "{reason}: delegate {}",
                        delegate.stop_for_quit()
                    ));
                }
                // Names the operation, the generation and how the child ended,
                // so a later status snapshot cannot stand in for this request.
                routing_log.event(format_args!("{reason}: {}", supervisor.stop_for_quit()));
                let _ = done.send(());
            })
            .is_ok()
        {
            *QUIT_STOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(finished);
        } else {
            // Never a reason to refuse to go. The person asked to quit; the
            // Job Object still ends the child when this process does.
            koru_ipc::ipc_debug!("{why}: could not start a thread to stop the gateway");
        }
    }

    fn request_agent_quit(&mut self) {
        if self.quit_confirmation_pending || self.quit_after_session_stop {
            return;
        }
        let Some(lifecycle) = self.session_lifecycle.clone() else {
            self.quit_because_asked();
            return;
        };
        let Some(waker) = self.event_waker.clone() else {
            self.quit_because_asked();
            return;
        };
        self.quit_confirmation_pending = true;
        if std::thread::Builder::new()
            .name("koru-agent quit confirmation".into())
            .spawn(move || {
                let catalog = lifecycle.catalog();
                let confirmed = match &catalog {
                    Ok(catalog) => {
                        catalog.active_count == 0
                            || crate::update::confirm_agent_quit(catalog.active_count)
                    }
                    // The person asked the Agent to quit. An unavailable
                    // catalog cannot turn that into a dead button; lifecycle
                    // stop plus Supervisor::drop still provide bounded cleanup.
                    Err(_) => true,
                };
                waker.wake(crate::event::AgentEvent::AgentQuitDecided { confirmed, catalog });
            })
            .is_err()
        {
            self.quit_confirmation_pending = false;
        }
    }

    /// **A person asked this Agent to stop**: tell every window so, then stop.
    ///
    /// The tray's *Quit* and `koru-agent stop` both end here; an identity reset
    /// does not, because its windows are meant to relaunch onto the new
    /// identity. Without the notice a window cannot tell this ending from a
    /// crash, and its recovery for a crash is to start a new Agent — which is
    /// how *Quit* used to bring back the process it had just stopped.
    ///
    /// **Queued before the stop begins**, so each window's writer has the whole
    /// session and gateway stop to deliver it ahead of the end of the stream.
    fn quit_because_asked(&mut self) {
        self.announce_quit();
        self.stop_sessions_then_quit();
    }

    /// Queue [`Push::Quitting`] for every live window.
    fn announce_quit(&mut self) {
        let ids: Vec<ConnectionId> = self.connections.iter().map(Connection::id).collect();
        for id in ids {
            self.push(id, Push::Quitting);
        }
    }

    /// Stop private session trees off-thread, then finish ordinary Agent
    /// shutdown. Supervisor's drop remains the bounded kill fallback.
    fn stop_sessions_then_quit(&mut self) {
        self.close_overlay();
        let Some(lifecycle) = self.session_lifecycle.clone() else {
            self.stop_the_workers_it_owns("quit");
            self.exit = true;
            return;
        };
        if self.session_stopping {
            self.quit_after_session_stop = true;
            return;
        }
        let Some(waker) = self.event_waker.clone() else {
            self.stop_the_workers_it_owns("quit");
            self.exit = true;
            return;
        };
        self.session_stopping = true;
        self.quit_after_session_stop = true;
        if std::thread::Builder::new()
            .name("koru-agent quit session shutdown".into())
            .spawn(move || {
                let outcome = lifecycle.stop();
                waker.wake(crate::event::AgentEvent::SessionHostStopped { outcome });
            })
            .is_err()
        {
            self.session_stopping = false;
            self.stop_the_workers_it_owns("quit");
            self.exit = true;
        }
    }

    /// Hide the overlay and remember its last OS-reported geometry.
    fn close_overlay(&mut self) -> bool {
        self.interrupt_dictation(koru_dictation::Interruption::WindowClosed);
        let closed = self.overlay.take().is_some();
        if closed {
            self.ignore_blur_until = None;
            self.search_geometry.persist();
        }
        closed
    }

    /// Refresh roaming choices and this machine's consent before each hold.
    /// Agent is a separate process from Koru, so it cannot observe the shell's
    /// in-memory preference globals; the file is the shared source of truth.
    fn sync_dictation_preferences(&mut self) {
        if cfg!(test) {
            return;
        }
        // Drain first: a hold begun right after somebody changed the
        // microphone must use the one they chose, and the link's answer is
        // waiting on a channel rather than in a file this thread would open.
        let _ = self.settings.poll();
        koru_dictation::Preferences::from_section(self.settings.authoritative())
            .apply_to(&mut self.dictation);
        let consent = self
            .paths
            .voice_consent_file()
            .map(|path| koru_voice_consent::read(&path))
            .and_then(|consent| consent.granted().cloned());
        self.dictation.set_consent(consent);
    }

    fn route_dictation_action(&mut self, action: &koru_dictation::DictationAction) -> bool {
        let Some(surface) = self.overlay.as_mut() else {
            return false;
        };
        match action {
            koru_dictation::DictationAction::Begin { .. } => surface.begin_dictation(),
            koru_dictation::DictationAction::Update { text, .. } => {
                surface.update_dictation(text.as_str());
                true
            }
            koru_dictation::DictationAction::Commit { text, .. } => {
                surface.commit_dictation(text.as_str());
                true
            }
            koru_dictation::DictationAction::Cancel { .. } => {
                surface.cancel_dictation();
                true
            }
        }
    }

    fn begin_search_dictation(&mut self, physical: PhysicalKey) -> bool {
        self.sync_dictation_preferences();
        self.begin_search_dictation_cached(physical)
    }

    /// Begin from Agent's registered-hotkey route using the settings/consent
    /// already cached by startup and ordinary settings events. The event loop
    /// may not poll IPC or read the consent file on this path.
    fn begin_search_dictation_cached(&mut self, physical: PhysicalKey) -> bool {
        let Some(action) = self.dictation.press(Instant::now()) else {
            return self.service_dictation();
        };
        let accepted = self.route_dictation_action(&action);
        self.dictation.began(accepted);
        self.dictation_key = accepted.then_some(physical);
        self.service_dictation() | accepted
    }

    fn local_dictation_state(&self) -> crate::dictation_coordinator::LocalState {
        match self.dictation.stage() {
            koru_dictation::Stage::Idle => crate::dictation_coordinator::LocalState::Idle,
            koru_dictation::Stage::Live => {
                crate::dictation_coordinator::LocalState::Listening(self.dictation.mode())
            }
            koru_dictation::Stage::Finalising => {
                crate::dictation_coordinator::LocalState::Finalising
            }
        }
    }

    fn report_agent_local_dictation(&mut self, refused: bool) {
        let Some(route) = self.agent_dictation_route else {
            return;
        };
        let state = if refused {
            crate::dictation_coordinator::LocalState::Refused
        } else {
            self.local_dictation_state()
        };
        if self.agent_dictation_last_state == Some(state) {
            return;
        }
        self.agent_dictation_last_state = Some(state);
        if let Some(coordinator) = &self.global_dictation {
            coordinator.local_state(crate::event::LocalDictationOwner::Agent, route, state);
        }
        if matches!(
            state,
            crate::dictation_coordinator::LocalState::Idle
                | crate::dictation_coordinator::LocalState::Refused
        ) {
            self.agent_dictation_route = None;
            self.agent_dictation_last_state = None;
        }
    }

    fn route_agent_local_dictation(
        &mut self,
        route: u64,
        gesture: crate::event::LocalDictationGesture,
    ) -> bool {
        let key = match gesture {
            crate::event::LocalDictationGesture::Press { key, .. }
            | crate::event::LocalDictationGesture::Release { key, .. }
            | crate::event::LocalDictationGesture::Cancel { key, .. } => key,
        };
        let physical = PhysicalKey::Code(match key.raw() {
            value if value == b'D' as u16 => winit::keyboard::KeyCode::KeyD,
            _ => winit::keyboard::KeyCode::KeyM,
        });
        let decision = search_routed_press(
            self.agent_dictation_route,
            route,
            self.dictation.stage(),
            self.dictation_key,
            physical,
        );
        if matches!(gesture, crate::event::LocalDictationGesture::Press { .. }) {
            match decision {
                koru_dictation::RoutedPress::Ignore => return false,
                koru_dictation::RoutedPress::Refuse => {
                    if let Some(coordinator) = &self.global_dictation {
                        coordinator.local_state(
                            crate::event::LocalDictationOwner::Agent,
                            route,
                            crate::dictation_coordinator::LocalState::Refused,
                        );
                    }
                    return false;
                }
                _ => {}
            }
        } else if self.agent_dictation_route != Some(route) {
            return false;
        }
        if self.agent_dictation_route != Some(route) {
            self.agent_dictation_last_state = None;
        }
        self.agent_dictation_route = Some(route);
        let changed = match gesture {
            crate::event::LocalDictationGesture::Press { .. } => {
                if decision == koru_dictation::RoutedPress::Adopt {
                    false
                } else {
                    self.begin_search_dictation_cached(physical)
                }
            }
            crate::event::LocalDictationGesture::Release { .. } => {
                self.finish_search_dictation(physical)
            }
            crate::event::LocalDictationGesture::Cancel { .. } => {
                self.interrupt_dictation(koru_dictation::Interruption::Suspended)
            }
        };
        let refused = matches!(gesture, crate::event::LocalDictationGesture::Press { .. })
            && !self.dictation.is_live();
        self.report_agent_local_dictation(refused);
        changed
    }

    fn finish_search_dictation(&mut self, physical: PhysicalKey) -> bool {
        if self.dictation_key != Some(physical) {
            return false;
        }
        self.dictation_key = None;
        if self.dictation.release(Instant::now()) {
            self.service_dictation()
        } else {
            false
        }
    }

    fn interrupt_dictation(&mut self, why: koru_dictation::Interruption) -> bool {
        if !self.dictation.is_live() {
            return false;
        }
        self.dictation.interrupt(why);
        self.dictation_key = None;
        self.service_dictation()
    }

    fn service_dictation(&mut self) -> bool {
        let actions = self.dictation.pump(Instant::now());
        let mut changed = false;
        for action in &actions {
            changed |= self.route_dictation_action(action);
        }
        if let Some(refusal) = self.dictation.take_refusal() {
            tracing::info!(
                target: "koru::voice",
                code = refusal.code.as_str(),
                surface = refusal.surface,
                "global-search dictation refused"
            );
        }
        if !self.dictation.is_live() {
            self.dictation_key = None;
        }
        self.report_agent_local_dictation(false);
        changed
    }

    pub(crate) fn open_workspaces(&self) -> Vec<crate::registry::OpenWorkspace> {
        self.registry.open_workspaces()
    }

    /// The session-host catalog the Agents submenu is drawn from — the
    /// watcher's last answer, stored by [`AgentEvent::AgentsCatalog`].
    pub(crate) fn agents_catalog(&self) -> &crate::agents_menu::AgentsCatalog {
        &self.agents_catalog
    }

    pub(crate) fn store_running(&self) -> Option<bool> {
        self.store_running
    }

    /// What the menu should say opens Search.
    pub(crate) fn shortcut(&self) -> &crate::tray::Shortcut {
        &self.shortcut
    }

    /// What the menu should say starts global Dictation.
    pub(crate) fn dictation_shortcut(&self) -> &crate::tray::Shortcut {
        &self.dictation_shortcut
    }

    pub(crate) fn ai_shortcut(&self) -> bool {
        self.ai_shortcut
    }

    /// **Tell every window whether there is an icon to come back from**, when
    /// that has changed. A window hides behind the icon only while this says
    /// so; on Linux the icon's host can come and go under a running Agent.
    pub(crate) fn set_notification_area(&mut self, present: bool) {
        if self.notification_area == present {
            return;
        }
        self.notification_area = present;
        koru_ipc::ipc_debug!("tray: notification area present={present}");
        let ids: Vec<ConnectionId> = self.connections.iter().map(Connection::id).collect();
        for id in ids {
            self.push(id, Push::NotificationArea { present });
        }
    }

    /// The window that just said `Hello` learns there is an icon. Nothing is
    /// sent when there is none: a window assumes none until told.
    fn push_notification_area_to(&mut self, connection: ConnectionId) {
        if self.notification_area {
            self.push(connection, Push::NotificationArea { present: true });
        }
    }

    /// What the tray's dictation submenu should say.
    pub(crate) fn voice(&self) -> &crate::tray::VoiceMenu {
        &self.voice
    }

    /// **Re-read the disk, keeping what the worker last said.**
    ///
    /// The two halves move independently — a model can be installed while a
    /// worker sits there, and a worker can end while the model stays on the
    /// disk — so re-reading one must not blank the other.
    fn refresh_voice_state(&mut self) {
        let worker = self.voice.worker.clone();
        let devices = self.voice.devices.clone();
        let input = self.chosen_input();
        self.voice = read_voice_state(self.paths.explicit_root(), input)
            .keeping_worker(worker)
            .keeping_devices(devices);
    }

    /// Which microphone the person chose, as the authority holds it.
    ///
    /// Drains the link first, so a tray menu opened straight after somebody
    /// changed the microphone in Settings names the one they picked.
    fn chosen_input(&mut self) -> koru_voice::InputChoice {
        let _ = self.settings.poll();
        koru_dictation::Preferences::from_section(self.settings.authoritative()).input
    }

    /// **Ask whatever voice worker is already listening what it is doing.**
    ///
    /// Three properties, each of which is the reason for a line of this:
    ///
    /// - **It starts nothing.** `ask::status` connects, and a connect that
    ///   finds nobody answers `NotRunning`. The notification area is drawn on a
    ///   machine with no worker running, so the question has to be answerable
    ///   there; `ensure_voice_off_thread` is the *other* verb and is reached
    ///   only by somebody asking for a dictation.
    /// - **It is off this thread.** The answer is an IPC round trip with a ten
    ///   second timeout, and the caller is the winit loop.
    /// - **It says nothing about voice on a switched-off installation.**
    ///   `KORU_VOICE_DISABLED` means nothing is dialled, waited for or queued
    ///   behind a decision that would be refused (Q-V44) -- the same rule
    ///   `service::ensure_voice_with` applies before its own lock.
    ///
    /// Under test it does nothing at all, for the reason
    /// `ensure_voice_off_thread` gives: a suite must not reach a developer's
    /// own worker, and asserting that a connection *fails* is the mistake that
    /// passes on CI and fails on the machine of anybody with Koru open.
    fn read_voice_worker_off_thread(&mut self) {
        if cfg!(test) || !koru_voice::available() {
            return;
        }
        let Some(waker) = self.event_waker.clone() else {
            return;
        };
        let root = self.paths.store_dir();
        if std::thread::Builder::new()
            .name("koru-agent read voice".into())
            .spawn(move || {
                waker.wake(AgentEvent::VoiceWorkerRead {
                    worker: voice_worker_facts(&root),
                    devices: voice_devices(&root),
                });
            })
            .is_err()
        {
            koru_ipc::ipc_debug!("voice: could not start a thread to read the worker");
        }
    }

    /// **Release the model, or stop the worker** -- on a thread, and never a
    /// third thing.
    ///
    /// Both verbs end in the same place: a fresh reading pushed back through
    /// [`AgentEvent::VoiceWorkerRead`], so the submenu says what is true rather
    /// than what was asked for. That matters for `release` in particular: the
    /// worker refuses one during a dictation and answers with a report that
    /// still says the model is loaded, and the menu has to believe the report.
    fn ask_voice_worker_off_thread(&mut self, verb: VoiceVerb) {
        if cfg!(test) || !koru_voice::available() {
            return;
        }
        let Some(waker) = self.event_waker.clone() else {
            return;
        };
        let root = self.paths.store_dir();
        if std::thread::Builder::new()
            .name("koru-agent ask voice".into())
            .spawn(move || {
                let asked = match verb {
                    VoiceVerb::Release => koru_voice_client::ask::release(&root),
                    VoiceVerb::Stop => koru_voice_client::ask::stop(&root),
                };
                // **Stopping leaves nothing to read.** The worker answers with
                // a report and then ends, so re-asking would race the process
                // exit and usually report `NotRunning` anyway; saying so
                // directly is both true and one fewer round trip.
                let worker = match verb {
                    VoiceVerb::Stop => None,
                    VoiceVerb::Release => facts_from(asked),
                };
                // A stopped worker sees no devices either, and re-dialling to
                // be told so would be the same race in a second place.
                let devices = match verb {
                    VoiceVerb::Stop => Vec::new(),
                    VoiceVerb::Release => voice_devices(&root),
                };
                waker.wake(AgentEvent::VoiceWorkerRead { worker, devices });
            })
            .is_err()
        {
            koru_ipc::ipc_debug!("voice: could not start a thread to {verb:?} the worker");
        }
    }

    /// Start the store on a worker, and answer `asked_by` when it is up.
    ///
    /// Deduplicated on the flag rather than by asking the store: two hotkey
    /// presses a second apart must not spawn two supervisors, and
    /// `ensure_running` connecting first makes a duplicate harmless but not
    /// free.
    fn ensure_gateway_off_thread(&mut self, asked_by: Option<(ConnectionId, u64)>) {
        // **Under test this starts nothing**, and the reason is not tidiness:
        // an ensure waits on the supervisor, which spawns a real gateway child
        // on whatever root `Paths` carries. A unit test that opens the overlay
        // would therefore launch real infrastructure on a made-up root. It did
        // not do so on the machine this was written on only because the thread
        // is detached and the harness exited first, which is a race rather than
        // a guarantee.
        //
        // `cfg!` rather than `#[cfg]`, for the reason
        // `SearchSurface::start_reader` gives at length: two bodies would leave
        // `gateway_host::ensure` with no caller and a page of dead-code
        // warnings for somebody to silence.
        if cfg!(test) {
            return;
        }
        let paths = self.paths.clone();
        let waker = self.event_waker.clone();
        let reply = asked_by
            .and_then(|(connection, id)| self.connection(connection).map(|c| (c.sender(), id)));
        if std::thread::Builder::new()
            .name("koru-agent ensure store".into())
            .spawn(move || {
                let (body, warning, running) = match crate::gateway_host::ensure(&paths) {
                    Ok(_) => (Response::Ok, None, true),
                    // A stable code and developer text, never something to show
                    // a person — the rule the store's own API follows.
                    Err(e) => (
                        Response::Failed {
                            code: e.code().to_string(),
                            message: e.to_string(),
                        },
                        Some(e.user_message()),
                        false,
                    ),
                };
                if let Some(waker) = waker {
                    waker.wake(AgentEvent::StoreAvailability { warning, running });
                }
                if let Some((sender, id)) = reply {
                    let _ = sender.send(FromAgent::Response { id, body });
                }
            })
            .is_err()
        {
            koru_ipc::ipc_debug!("store: could not start a thread to ensure it");
            if let Some(surface) = self.overlay.as_mut() {
                surface.set_availability_warning(Some(
                    "Koru Store could not be checked. Reopen Search and try again.".into(),
                ));
            }
        }
    }

    /// **Put a window in front of the person**, opening one if none is
    /// running. What *Open Workspace* does, and what the Voice row does on the
    /// way to a dictation — a composition needs a focused editable surface, and
    /// an agent has none.
    ///
    /// Answers whether the agent's own surface changed, which is `false`:
    /// pushing to a shell and spawning a process are somebody else's window.
    fn restore_windows(&mut self) -> bool {
        if let Err(error) = self.open_workspace(None, false) {
            // Nothing else to do about it: there is no window to put an error
            // in, and the tray is still there to try again.
            koru_ipc::ipc_debug!("tray: could not open a window: {error}");
        }
        false
    }

    /// Bring one open workspace's Koru window to the front — a workspace row's
    /// click, or the icon's left button answering "the window in use".
    fn open_workspace_window(&mut self, connection: ConnectionId) {
        let target = self
            .registry
            .shell(connection)
            .map(|shell| (shell.connection, shell.pid));
        if let Some((connection, pid)) = target {
            let foreground = pid.map(foreground);
            let foreground = foreground
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unavailable".into());
            self.routing_log.event(format_args!(
                "tray open workspace connection={} pid={pid:?} foreground={foreground}",
                connection.0
            ));
            self.push(connection, Push::Restore);
        } else {
            self.routing_log.event(format_args!(
                "tray open workspace connection={} missing=true",
                connection.0
            ));
        }
    }

    fn open_workspace(
        &mut self,
        profile: Option<&str>,
        inherit_stdio: bool,
    ) -> std::io::Result<Option<u32>> {
        self.open_workspace_with(profile, inherit_stdio, spawn_shell_requested)
    }

    fn open_workspace_with(
        &mut self,
        profile: Option<&str>,
        inherit_stdio: bool,
        spawn: impl FnOnce(Option<&Path>, Option<&str>, bool) -> std::io::Result<u32>,
    ) -> std::io::Result<Option<u32>> {
        // Accepted sockets are not windows. In particular, `koru-agent open`
        // is itself a short-lived connection and has not sent `Hello`; counting
        // it here makes every open request "restore" the CLI instead of
        // spawning Koru. Registry contains only peers that proved their
        // presence kind, so only its workspace shells may receive Restore.
        let ids: Vec<ConnectionId> = self
            .registry
            .shells()
            .into_iter()
            .map(|shell| shell.connection)
            .collect();
        if ids.is_empty() {
            if self.restart_gate_pending() {
                self.show_restart_gate_warning();
                return Err(std::io::Error::other(
                    "an update restart is still being verified",
                ));
            }
            let pid = spawn(self.paths.explicit_root(), profile, inherit_stdio)?;
            koru_ipc::ipc_debug!("tray: opened a window (pid {pid})");
            Ok(Some(pid))
        } else {
            for id in ids {
                self.push(id, Push::Restore);
            }
            Ok(None)
        }
    }

    /// Start or adopt the voice worker on a worker thread, and answer
    /// `asked_by` when it is up.
    ///
    /// **Under test this starts nothing**, and the reason is the one
    /// `ensure_gateway_off_thread` gives at length: `ensure_voice` spawns
    /// `koru-voice-worker` beside this binary on whatever root `Paths` carries, so a
    /// unit test would launch real infrastructure on a made-up root and leave
    /// it holding an executable the next build has to overwrite. What the
    /// decision does is proved in `service/tests.rs`, which drives it with the
    /// effects handed in.
    ///
    /// `cfg!` rather than `#[cfg]`, so `service::ensure_voice` keeps its caller
    /// instead of leaving a page of dead-code warnings for somebody to silence.
    fn ensure_voice_off_thread(&mut self, asked_by: Option<(ConnectionId, u64)>) {
        if cfg!(test) {
            return;
        }
        let paths = self.paths.clone();
        let waker = self.event_waker.clone();
        let reply = asked_by
            .and_then(|(connection, id)| self.connection(connection).map(|c| (c.sender(), id)));
        if std::thread::Builder::new()
            .name("koru-agent ensure voice".into())
            .spawn(move || {
                let body = match service::ensure_voice(&paths) {
                    Ok(_) => {
                        // A successful ensure is also when an empty input menu
                        // becomes knowable. Refresh after the worker is ready,
                        // not alongside the launch where the query can win the
                        // bind race and preserve *System default* forever.
                        if let Some(waker) = waker {
                            let root = paths.store_dir();
                            waker.wake(AgentEvent::VoiceWorkerRead {
                                worker: voice_worker_facts(&root),
                                devices: voice_devices(&root),
                            });
                        }
                        Response::Ok
                    }
                    // A stable code and developer text, never something to show
                    // a person -- the rule the store's own API follows, and the
                    // one `koru_voice::ErrorCode` follows on the other side.
                    Err(error) => Response::Failed {
                        code: error.code().to_string(),
                        message: error.to_string(),
                    },
                };
                if let Some((sender, id)) = reply {
                    let _ = sender.send(FromAgent::Response { id, body });
                }
            })
            .is_err()
        {
            koru_ipc::ipc_debug!("voice: could not start a thread to ensure the worker");
        }
    }

    /// Start or activate standalone chat for a gesture owned by Agent itself.
    ///
    /// Unlike a shell request there is no socket waiting for the result. The
    /// worker therefore returns through the event mailbox, so failure can use
    /// Agent's existing warning surface and this thread never waits on process
    /// discovery or launch.
    fn open_ai_chat_for_agent(&mut self, open: crate::ai_chat::Open) {
        // The tray row and the global chord are both absent in a build that
        // hides Koru AI; this is the same answer at the door (koru-features).
        if !koru_features::ai() {
            return;
        }
        if self.restart_gate_pending() {
            self.show_restart_gate_warning();
            return;
        }
        let Some(waker) = self.event_waker.clone() else {
            tracing::error!(
                "Koru AI chat could not be opened: the Agent event loop is unavailable"
            );
            return;
        };
        crate::ai_chat::open_off_thread(
            self.paths.explicit_root().map(Path::to_path_buf),
            open,
            crate::ai_chat::OpenCompletion::Agent(waker),
        );
    }

    /// A result the person chose in the overlay.
    ///
    /// The decision is [`Router::open`]'s and is pure; what is here is
    /// performing it.
    fn open_target(&mut self, target: koru_search_ui::Target) {
        if self.restart_gate_pending() {
            self.show_restart_gate_warning();
            return;
        }
        let (profile, workspace, document, node_present) = match &target {
            koru_search_ui::Target::Store {
                profile,
                workspace,
                document,
                node,
                ..
            } => (*profile, *workspace, *document, node.is_some()),
            koru_search_ui::Target::Chat { chat, block, .. } => {
                self.open_ai_chat_for_agent(crate::ai_chat::Open::Chat {
                    chat: chat.as_uuid(),
                    block: Some(block.as_uuid()),
                });
                return;
            }
        };
        self.routing_log.event(format_args!(
            "selection profile={} workspace={} document={} node_present={} registry={} connections={} pending_before={}",
            profile.to_storage(),
            workspace.to_storage(),
            document.map_or_else(|| "none".into(), |id| id.to_storage()),
            node_present,
            self.registry.len(),
            self.connections.len(),
            self.router.pending_count()
        ));
        let wire = to_wire(target);
        match self
            .router
            .open(&self.registry, profile, workspace, wire, Instant::now())
        {
            Routed::Deliver {
                connection,
                pid,
                target,
            } => {
                let foreground = pid.map(foreground);
                let foreground = foreground
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unavailable".into());
                self.routing_log.event(format_args!(
                    "decision deliver profile={} workspace={} connection={} pid={pid:?} foreground={foreground}",
                    profile.to_storage(),
                    workspace.to_storage(),
                    connection.0
                ));
                // Permission has to exist before the target arrives: receiving
                // it is what makes the shell call `focus_window`, and granting
                // permission afterwards loses that race every time.
                self.push(connection, Push::Open { target: *target });
            }
            Routed::Launch { profile, workspace } => {
                // Only stable ids go on the command line. The target carries
                // `match_text`, a fragment of what somebody wrote, and remains
                // pending in this process until the exact workspace says Hello.
                match spawn_shell(profile, workspace, self.paths.explicit_root()) {
                    Ok(pid) => {
                        let foreground = foreground(pid);
                        self.routing_log.event(format_args!(
                            "decision launch profile={} workspace={} child_pid={} foreground={foreground} pending={}",
                            profile.to_storage(),
                            workspace.to_storage(),
                            pid,
                            self.router.pending_count()
                        ));
                    }
                    Err(e) => {
                        koru_ipc::ipc_debug!("launch: could not start a shell: {e}");
                        self.routing_log.event(format_args!(
                            "launch failed profile={} workspace={} error={e}",
                            profile.to_storage(),
                            workspace.to_storage()
                        ));
                        self.router.launch_failed(profile, workspace);
                        // The search window was deliberately removed before
                        // foreground routing. Put it back when there is no Koru
                        // window to receive the result, and say why the click
                        // appeared to do nothing.
                        self.open_overlay();
                        if let Some(surface) = self.overlay.as_mut() {
                            surface.set_host_warning(Some(shell_warning(&e)));
                        }
                    }
                }
            }
            Routed::AlreadyLaunching => self.routing_log.event(format_args!(
                "decision already_launching profile={} workspace={} pending={}",
                profile.to_storage(),
                workspace.to_storage(),
                self.router.pending_count()
            )),
        }
    }

    /// **Show a session — or offer starting one — in Koru Workspace** (Q-E8
    /// P9). `Some` is one of the Agents menu's session rows; `None` is its
    /// New.
    ///
    /// The most recently active Workspace window is the one asked — the
    /// routing rule every other decision here follows. Which *tabs* a
    /// Workspace window is showing is not presence data — the menu
    /// stopped being window-fed — so the finer "the exact window showing the
    /// session" step happens inside koru.exe, which checks its own panes when
    /// the push arrives. With no Workspace connected at all, the ask is
    /// latched and a window is launched; the next `Hello` spends it — the
    /// `feedback_owed` shape, for the same reason.
    fn show_workspace_session(&mut self, session: Option<koru_session_protocol::SessionId>) {
        if self.restart_gate_pending() {
            self.show_restart_gate_warning();
            return;
        }
        let target = self
            .registry
            .most_recent()
            .map(|shell| (shell.connection, shell.pid));
        match target {
            Some((connection, pid)) => {
                let foreground = pid.map(foreground);
                let foreground = foreground
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unavailable".into());
                self.routing_log.event(format_args!(
                    "agents decision focus connection={} pid={pid:?} foreground={foreground} new={}",
                    connection.0,
                    session.is_none()
                ));
                // Permission before the push, the rule every other foreground
                // hand-off here keeps: receiving it is what makes the shell
                // focus itself.
                self.push(
                    connection,
                    Push::ShowSession {
                        session: session.map(|session| session.to_string()),
                    },
                );
            }
            None => {
                self.routing_log.event(format_args!(
                    "agents decision launch new={}",
                    session.is_none()
                ));
                self.pending_workspace_session = Some(session);
                // Spawned directly rather than through `restore_windows`:
                // that one pushes `Restore` at *any* connection, and a Koru
                // Terminal window coming forward is not the Workspace window
                // this latch is waiting to spend itself on.
                match spawn_shell_bare(self.paths.explicit_root()) {
                    Ok(pid) => {
                        koru_ipc::ipc_debug!("agents: opened a Workspace window (pid {pid})");
                    }
                    // Nothing to draw an error in; the tray is still there to
                    // try again, and the latch keeps the ask for a window
                    // that does arrive.
                    Err(error) => {
                        koru_ipc::ipc_debug!("agents: could not open a Workspace window: {error}");
                    }
                }
            }
        }
        // Whatever this opens will change the catalog shortly; ask for a
        // fresh read rather than waiting for the next observable event.
        self.kick_agents_menu();
    }

    /// Hand a latched Agents-menu ask to the window that just arrived.
    /// Taken, not read — the agent outlives many shells, and an ask left
    /// latched would reopen a session tab on every later window.
    fn push_workspace_session_to(&mut self, connection: ConnectionId) {
        let Some(session) = self.pending_workspace_session.take() else {
            return;
        };
        self.push(
            connection,
            Push::ShowSession {
                session: session.map(|session| session.to_string()),
            },
        );
    }

    /// Route a missed-summary activation, retaining the open before a cold
    /// shell can race its `Hello` back onto this loop.
    fn open_reminder_profile(
        &mut self,
        profile: ProfileId,
        spawn: impl FnOnce(&mut Self, ProfileId, Option<&std::path::Path>) -> std::io::Result<u32>,
    ) {
        if let Some((connection, pid)) = self
            .registry
            .route_profile(profile)
            .map(|shell| (shell.connection, shell.pid))
        {
            if let Some(pid) = pid {
                let _ = foreground(pid);
            }
            self.push(connection, Push::OpenReminders);
            return;
        }

        // Arm first: a newly spawned shell may finish startup and say Hello
        // before `spawn` returns. Hello consumes this exact profile latch.
        self.reminder_opens_owed.insert(profile);
        let root = self.paths.explicit_root().map(std::path::Path::to_path_buf);
        if let Err(error) = spawn(self, profile, root.as_deref()) {
            self.reminder_opens_owed.remove(&profile);
            tracing::error!(%error, "Koru reminder profile could not be opened");
        }
    }

    /// Consume a choice made by any overlay input path.
    ///
    /// Search records the target while handling the input event; the host owns
    /// routing it. Keyboard used to bridge those two halves while mouse input
    /// did not, leaving a clicked result pending until another key event. Put
    /// the bridge in one place so click, double-click and Enter cannot drift.
    fn finish_overlay_target(&mut self, source: &'static str) -> bool {
        let target = self
            .overlay
            .as_mut()
            .and_then(|surface| surface.take_target());
        let Some(target) = target else { return false };
        self.routing_log.event(format_args!(
            "overlay target deferred source={source} kind={}",
            match &target {
                koru_search_ui::Target::Store { .. } => "store",
                koru_search_ui::Target::Chat { .. } => "chat",
            }
        ));
        // Drop the always-on-top overlay before handing foreground to Koru.
        // Otherwise it can remain above the very window the choice opened.
        self.close_overlay();
        self.deferred_open = Some(DeferredOpen {
            target,
            stage: DeferredStage::ClosingOverlay,
            deadline: Instant::now(),
        });
        true
    }
}

/// The local device as the first row of the list: always "connected", named
/// by its latest self-signed rename, and the only row a rename gesture lands
/// on.
fn this_device(view: &koru_device::TrustView) -> Option<koru_agent_wire::LinkedDeviceView> {
    let local = &view.local_device;
    Some(koru_agent_wire::LinkedDeviceView {
        device_id: local.device_id.to_storage(),
        display_name: view
            .display_name_of(local.device_id)
            .unwrap_or_else(|| local.display_name.clone()),
        linked_at_millis: local.created_at_millis,
        connected: true,
        last_seen_millis: None,
        introduced_by: None,
        build_label: view.build_of(local.device_id).map(|build| build.label()),
        build_drift: None,
        build_severity: None,
        update_needed_on: None,
        connection: None,
        last_sync_millis: None,
        history: Vec::new(),
    })
}

/// The word the device list uses for how a live peer is reached. Only the
/// two states that say so map; "connected, path unknown" says nothing.
fn connection_path(state: koru_sync_control::PeerState) -> Option<&'static str> {
    match state {
        koru_sync_control::PeerState::ConnectedDirect => Some("direct"),
        koru_sync_control::PeerState::ConnectedRelay => Some("relay"),
        _ => None,
    }
}

/// The relay switch and live connection observed by this Agent. The native
/// tray and the workspace modal both read the same network instance, so they
/// cannot disagree about the preference or the connected host.
fn relay_view() -> Option<koru_agent_wire::RelayView> {
    use koru_network_iroh::RelayConnectionStatus as Status;

    let network = crate::network::current()?;
    let enabled = network.relay_enabled();
    let (state, host) = match network.runtime().status().relay {
        Status::Off => ("off", None),
        Status::Connecting { host } => ("connecting", Some(host)),
        Status::Connected { host } => ("connected", Some(host)),
        Status::Disconnected { host } => ("disconnected", Some(host)),
    };
    Some(koru_agent_wire::RelayView {
        enabled,
        state: state.into(),
        host,
    })
}

fn linked_devices(
    view: &koru_device::TrustView,
    status: Option<&koru_sync_control::ControlStatus>,
) -> Vec<koru_agent_wire::LinkedDeviceView> {
    // The mesh, not just the directly paired ring: any device in this
    // device's connected component is linked. A transitive device is marked
    // with the direct peer that introduced it, and offers no Unlink — it is
    // removed by revoking the relationship that carries it. Names resolve
    // through gossiped renames, so a device appears under its current name,
    // and builds the same way, so the list says what each one runs.
    let local_build = view.build_of(view.local_device.device_id);
    koru_device::Authorization::devices(view)
        .into_iter()
        .map(|peer| {
            let build = view.build_of(peer.device_id);
            let drift = match (&local_build, &build) {
                (Some(ours), Some(theirs)) => Some(ours.drift_from(theirs)),
                _ => None,
            };
            let last_sync_millis = status.and_then(|status| {
                status
                    .peers
                    .iter()
                    .find(|candidate| candidate.device_id == peer.device_id)
                    .and_then(|candidate| candidate.last_state_change_millis)
            });
            koru_agent_wire::LinkedDeviceView {
                device_id: peer.device_id.to_storage(),
                display_name: view
                    .display_name_of(peer.device_id)
                    .unwrap_or_else(|| "Unknown device".into()),
                linked_at_millis: view
                    .relationship(peer.relationship_id)
                    .map_or(0, |relationship| relationship.created_at_millis),
                introduced_by: peer.via.and_then(|via| view.display_name_of(via)),
                build_drift: drift.map(|drift| drift.as_str().to_owned()),
                build_severity: drift
                    .and_then(|drift| drift.severity().as_str())
                    .map(str::to_owned),
                update_needed_on: drift
                    .and_then(koru_device::BuildDrift::update_needed_on)
                    .map(str::to_owned),
                build_label: build.map(|build| build.label()),
                connected: status.is_some_and(|status| {
                    status.peers.iter().any(|candidate| {
                        candidate.device_id == peer.device_id
                            && matches!(
                                candidate.state,
                                koru_sync_control::PeerState::ConnectedDirect
                                    | koru_sync_control::PeerState::ConnectedRelay
                                    | koru_sync_control::PeerState::ConnectedUnknown
                            )
                    })
                }),
                connection: status.and_then(|status| {
                    status
                        .peers
                        .iter()
                        .find(|candidate| candidate.device_id == peer.device_id)
                        .and_then(|candidate| connection_path(candidate.state))
                        .map(str::to_owned)
                }),
                last_seen_millis: status.and_then(|status| {
                    status
                        .peers
                        .iter()
                        .find(|candidate| candidate.device_id == peer.device_id)
                        .and_then(|candidate| candidate.last_seen_millis)
                }),
                last_sync_millis,
                history: Vec::new(),
            }
        })
        .collect()
}

fn device_state_event(
    event: koru_sync_control::StateChangeEvent,
) -> koru_agent_wire::DeviceStateEventView {
    koru_agent_wire::DeviceStateEventView {
        event_id: event.event_id,
        at_millis: event.at_millis,
        kind: event.kind,
        profile_id: event.profile.map(|id| id.to_storage()),
        workspace_id: event.workspace.map(|id| id.to_storage()),
        document_id: event.document.map(|id| id.to_storage()),
        document_name: event.document_name,
        blob_hash: event.blob_hash,
        detail: event.detail,
        items: event.items,
    }
}

fn wall_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn replay_reopen(
    paths: Paths,
    io: std::sync::Arc<std::sync::Mutex<()>>,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    live: Vec<(String, String, String)>,
) {
    use std::sync::atomic::Ordering;

    let root = paths.explicit_root().map(Path::to_path_buf);
    let chat_owed = {
        let Ok(_guard) = io.lock() else {
            koru_ipc::ipc_debug!("update: reopen persistence lock was poisoned");
            running.store(false, Ordering::Release);
            return;
        };
        let mut reopen = crate::update::Reopen::load(&paths);
        if reopen.is_empty() {
            let _ = reopen.save(&paths);
            pending.store(false, Ordering::Release);
            running.store(false, Ordering::Release);
            return;
        }
        let mut persistence_failed = false;
        for recorded in reopen.windows.clone() {
            let Some((profile, workspace)) = ids(&recorded.0, &recorded.1) else {
                koru_ipc::ipc_debug!("update: a recorded window had an id that will not parse");
                reopen.acknowledge_window(&recorded.0, &recorded.1);
                persistence_failed |= reopen.save(&paths).is_err();
                continue;
            };
            // A shell can survive an Agent-only crash and reconnect before
            // Store readiness. Its already-observed Hello is the ack.
            if let Some((_, _, claim)) = live
                .iter()
                .find(|(profile, workspace, _)| profile == &recorded.0 && workspace == &recorded.1)
                && reopen.acknowledge_claim(&recorded.0, &recorded.1, claim)
            {
                persistence_failed |= reopen.save(&paths).is_err();
                continue;
            }
            let claim = reopen.claim_window(&recorded);
            if let Err(error) = reopen.save(&paths) {
                persistence_failed = true;
                koru_ipc::ipc_debug!(
                    "update: could not persist a window reopen claim before launch: {error}"
                );
                continue;
            }
            match spawn_reopened_shell(profile, workspace, root.as_deref(), &claim) {
                Ok(pid) => koru_ipc::ipc_debug!(
                    "update: launched a window pid={pid}; awaiting shell acknowledgement"
                ),
                Err(error) => {
                    koru_ipc::ipc_debug!("update: could not reopen a window: {error}")
                }
            }
        }
        pending.store(persistence_failed || !reopen.is_empty(), Ordering::Release);
        reopen.ai_chat
    };

    if chat_owed {
        match crate::ai_chat::launch(root.as_deref(), None) {
            Ok(pid) => {
                koru_ipc::ipc_debug!(
                    "update: launched Koru AI chat pid={pid}; awaiting singleton readiness"
                );
                let started = Instant::now();
                let mut acknowledged = false;
                while started.elapsed() < Duration::from_secs(10) {
                    if crate::ai_chat::status(root.as_deref(), Duration::from_millis(200)).is_some()
                    {
                        let Ok(_guard) = io.lock() else {
                            break;
                        };
                        let mut reopen = crate::update::Reopen::load(&paths);
                        reopen.ai_chat = false;
                        let persistence_failed = match reopen.save(&paths) {
                            Ok(()) => {
                                koru_ipc::ipc_debug!(
                                    "update: reopened Koru AI chat bound its singleton endpoint"
                                );
                                false
                            }
                            Err(error) => {
                                koru_ipc::ipc_debug!(
                                    "update: could not persist the chat reopen acknowledgement: {error}"
                                );
                                true
                            }
                        };
                        pending.store(persistence_failed || !reopen.is_empty(), Ordering::Release);
                        acknowledged = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if !acknowledged {
                    koru_ipc::ipc_debug!(
                        "update: Koru AI chat did not bind its singleton endpoint before the reopen deadline"
                    );
                }
            }
            Err(error) => koru_ipc::ipc_debug!("update: could not reopen Koru AI chat: {error}"),
        }
    }
    running.store(false, Ordering::Release);
}

fn ids(profile: &str, workspace: &str) -> Option<(ProfileId, WorkspaceId)> {
    Some((ProfileId::parse(profile)?, WorkspaceId::parse(workspace)?))
}

fn merge_reminder_report(
    current: &crate::reminders::SchedulerReport,
    mut next: crate::reminders::SchedulerReport,
) -> (crate::reminders::SchedulerReport, bool) {
    // A zero-count failure report is the legacy shape emitted when the whole
    // scheduler operation failed before it could produce a snapshot, so every
    // prior scheduler fact (including the local enabled choice) stays current.
    // An isolated registration failure is already a complete snapshot.
    if next.failure.is_some() && next.failed_count == 0 {
        next.delivery_enabled = current.delivery_enabled;
        next.pending_count = current.pending_count;
        next.registered_count = current.registered_count;
        next.cursor = current.cursor;
        next.next_deadline_ms = current.next_deadline_ms;
        next.failed_count = 1;
        next.attention = current.attention.clone();
    }
    if next.last_successful_sync_ms.is_none() {
        next.last_successful_sync_ms = current.last_successful_sync_ms;
    }
    let changed = next.failure != current.failure;
    (next, changed)
}

fn reminder_diagnostics(
    report: &crate::reminders::SchedulerReport,
) -> koru_agent_wire::ReminderDiagnostics {
    koru_agent_wire::ReminderDiagnostics {
        enabled: report.delivery_enabled,
        pending: report.pending_count,
        registered: report.registered_count,
        failed: report.failed_count,
        cursor: report.cursor,
        next_deadline_ms: report.next_deadline_ms,
        last_successful_sync_ms: report.last_successful_sync_ms,
        failure_category: report.failure.map(|category| category.as_str().to_owned()),
        attention: report
            .attention
            .iter()
            .map(|(profile, reminder, occurrence, fired_at_ms)| {
                koru_agent_wire::ReminderAttention {
                    profile: profile.to_storage(),
                    reminder: reminder.to_storage(),
                    occurrence: occurrence.to_storage(),
                    fired_at_ms: *fired_at_ms,
                }
            })
            .collect(),
    }
}

/// Where releases are published.
///
/// Overridable at compile time so the release drill can be rehearsed against a
/// local feed without editing code — the same shape `KORU_SIGNTOOL_COMMAND` and
/// `KORU_NOTES_COMMAND` already have on the publishing side. `UpdateClient`
/// refuses a non-HTTPS origin unless it is loopback in a debug build, so this
/// cannot quietly become a plaintext channel in a release.
fn update_base_url() -> String {
    option_env!("KORU_UPDATE_BASE_URL")
        .unwrap_or(DEFAULT_UPDATE_BASE_URL)
        .to_string()
}

/// Where a build looks when nothing was baked.
///
/// **Linux defaults to the GitHub nightly release, not `updates.koru.do`.** CI
/// bakes `KORU_UPDATE_BASE_URL` into every Linux build, so this decides only
/// what a build that lost it does — and `updates.koru.do` has never held a
/// Linux feed, so pointing there is a nightly that silently never hears of its
/// next build. The nightly release directory is where the Linux feed is
/// published (`docs/features/linux/NIGHTLY.rev1.md` §6). Nothing about trust
/// changes with the host: whatever answers, the manifest must name this
/// channel, platform and architecture and the package must carry a Minisign
/// signature from the key baked into this binary, so a wrong default costs a
/// failed check, never an install. A Linux build of another channel asks this
/// directory for a manifest it does not hold and fails the same way, which is
/// right until that channel has a Linux feed of its own.
const DEFAULT_UPDATE_BASE_URL: &str = if cfg!(target_os = "linux") {
    "https://github.com/koruworks/mono/releases/download/nightly/"
} else {
    "https://updates.koru.do/"
};

/// Whether the release rehearsal should fail this target launch before its
/// health marker. Pure so the safety boundary remains pinned without mutating
/// the process environment in a parallel test suite.
fn should_fail_rehearsal_health(
    explicit_root: bool,
    has_health_token: bool,
    requested: bool,
) -> bool {
    explicit_root && has_health_token && requested
}

/// The directory whose contents an update replaces.
///
/// **Beside this executable**, which is the same rule everything else in this
/// process follows for finding `koru.exe` and every other package binary: the
/// binaries of one channel are installed together and released together. It falls back to
/// `.` only so the resolution cannot panic; a build that cannot find its own
/// executable will fail `is_writable_install` and offer a manual download,
/// which is the right answer to a question this confused.
pub(crate) fn install_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn update_paths_for(
    paths: &crate::Paths,
    install: &std::path::Path,
) -> Option<koru_update::UpdatePaths> {
    koru_update::UpdatePaths::for_install(paths.explicit_root(), install)
}

/// The other direction, for a target a shell asked to have routed.
///
/// **An id that will not parse is a refusal, never a `None`.** `None` already
/// means something real on `document` — a workspace-level hit — so it cannot
/// double as "unreadable", and routing a half-understood target would put a
/// window somewhere nobody chose.
fn search_target_label(target: &koru_search_ui::Target) -> String {
    match target {
        koru_search_ui::Target::Store {
            profile, workspace, ..
        } => format!("store:{}/{}", profile.to_storage(), workspace.to_storage()),
        koru_search_ui::Target::Chat { chat, block, .. } => {
            format!("chat:{chat}#{block}")
        }
    }
}

fn from_wire(target: koru_agent_wire::Target) -> Option<koru_search_ui::Target> {
    let (profile, workspace) = ids(&target.profile, &target.workspace)?;
    let document = match target.document.as_deref() {
        None => None,
        Some(raw) => Some(koru_store_core::ids::DocumentId::parse(raw)?),
    };
    Some(koru_search_ui::Target::Store {
        profile,
        workspace,
        document,
        node: target.node,
        trashed: target.trashed,
        match_text: target.match_text,
        disposition: if target.new_window {
            koru_search_ui::Disposition::NewWindow
        } else {
            koru_search_ui::Disposition::CurrentWindow
        },
    })
}

fn to_wire(target: koru_search_ui::Target) -> koru_agent_wire::Target {
    let koru_search_ui::Target::Store {
        profile,
        workspace,
        document,
        node,
        trashed,
        match_text,
        disposition,
    } = target
    else {
        unreachable!("chat targets launch directly")
    };
    koru_agent_wire::Target {
        profile: profile.to_storage(),
        workspace: workspace.to_storage(),
        document: document.map(|id| id.to_storage()),
        node,
        trashed,
        match_text,
        new_window: matches!(disposition, koru_search_ui::Disposition::NewWindow),
    }
}

/// Start a shell, detached, with only stable profile/workspace ids on its
/// command line. The private search target remains in the router.
///
/// Beside this executable and never `PATH`, the rule `Launcher` already
/// follows: the binaries of one channel are installed together, and the first
/// `koru.exe` on `PATH` could be any of them.
fn spawn_shell(
    profile: ProfileId,
    workspace: WorkspaceId,
    root: Option<&std::path::Path>,
) -> std::io::Result<u32> {
    // The socket lifecycle gate needs to observe the interval between the
    // launch decision and the matching window's Hello. A debug-only, rooted
    // fixture keeps that interval open; otherwise the real sibling Koru can
    // finish its cold launch before the test receives the Route response and
    // turn the assertion into a machine-speed race. Release builds and normal
    // (unrooted) agents cannot take this path.
    #[cfg(debug_assertions)]
    if root.is_some() && koru_env::switch(koru_env::name::KORU_AGENT_TEST_HOLD_SHELL_LAUNCH) {
        return Ok(std::process::id());
    }
    spawn_koru(&shell_launch_arguments(profile, workspace, root, None))
}

/// Open a profile without choosing one of its workspaces or a product surface.
fn spawn_profile_shell(profile: ProfileId, root: Option<&std::path::Path>) -> std::io::Result<u32> {
    let mut args = vec!["--store-profile".into(), profile.to_storage()];
    if let Some(root) = root {
        args.extend(["--root".into(), root.display().to_string()]);
    }
    spawn_koru(&args)
}

fn spawn_reopened_shell(
    profile: ProfileId,
    workspace: WorkspaceId,
    root: Option<&std::path::Path>,
    claim: &str,
) -> std::io::Result<u32> {
    spawn_koru(&shell_launch_arguments(
        profile,
        workspace,
        root,
        Some(claim),
    ))
}

/// Start `koru.exe` **beside this executable**, detached.
///
/// One body for both callers, because the rule it enforces is the one that
/// matters: never through `PATH`. Koru's binaries are built, signed and
/// shipped as one unit, and the first `koru.exe` on somebody's `PATH` could
/// belong to any channel.
fn spawn_koru(args: &[String]) -> std::io::Result<u32> {
    spawn_koru_with_stdio(args, false)
}

fn spawn_koru_with_stdio(args: &[String], inherit_stdio: bool) -> std::io::Result<u32> {
    // A test binary can have a real koru.exe beside it after a workspace
    // build. Refuse at the shared effect boundary before even resolving that
    // path: cold voice/session/menu fixtures must never open their fake root.
    // Launch decisions are exercised with injected launchers instead.
    if cfg!(test) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "shell process launch is disabled in Agent unit tests",
        ));
    }
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("this executable has no directory"))?;
    let candidate = dir.join(if cfg!(windows) { "koru.exe" } else { "koru" });
    if !candidate.exists() {
        return Err(std::io::Error::other(format!(
            "{} is not beside this executable",
            candidate.display()
        )));
    }
    let mut command = std::process::Command::new(candidate);
    command.args(args).stdin(std::process::Stdio::null());
    if inherit_stdio {
        keep_console(&mut command);
    } else {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        detach(&mut command);
    }
    Ok(remember_launch(command.spawn()?))
}

/// **Where a launch becomes a fact.** Every window this Agent starts passes
/// through here, so the first-party grant table is a record of `spawn` return
/// values rather than of intentions — and a caller that later asks for a grant
/// is matched against the operating system's own answer for its pid, never
/// against anything it said about itself.
///
/// It takes the whole `Child` so the pid is read from the handle rather than
/// passed alongside it: a call site that produced one and recorded the other
/// would compile.
pub(crate) fn remember_launch(child: std::process::Child) -> u32 {
    let pid = child.id();
    crate::grants::record_launch(pid);
    pid
}

/// Start `koru.exe` and let it decide what to open.
///
/// The counterpart to [`spawn_shell`], for the case where the agent has no
/// target to name — the tray's *Open Workspace* with nothing running. Naming
/// one would mean inventing it: on a first run there is no profile and no
/// workspace, and on any other run the shell's own resolution (the profile it
/// was last in, else the newest, else a fresh one) is better informed than
/// anything this process could reconstruct.
fn spawn_shell_bare(root: Option<&std::path::Path>) -> std::io::Result<u32> {
    spawn_shell_requested(root, None, false)
}

fn spawn_shell_requested(
    root: Option<&std::path::Path>,
    profile: Option<&str>,
    inherit_stdio: bool,
) -> std::io::Result<u32> {
    let mut args = Vec::new();
    if let Some(root) = root {
        args.extend(["--root".to_string(), root.display().to_string()]);
    }
    if let Some(profile) = profile {
        args.extend(["--profile".to_string(), profile.to_string()]);
    }
    spawn_koru_with_stdio(&args, inherit_stdio)
}

fn shell_launch_arguments(
    profile: ProfileId,
    workspace: WorkspaceId,
    root: Option<&std::path::Path>,
    reopen_claim: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "--store-profile".into(),
        profile.to_storage(),
        "--workspace".into(),
        workspace.to_storage(),
    ];
    if let Some(root) = root {
        args.extend(["--root".into(), root.display().to_string()]);
    }
    if let Some(claim) = reopen_claim {
        args.extend(["--reopen-claim".into(), claim.to_string()]);
    }
    args
}

#[cfg(windows)]
pub(crate) fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(windows)]
fn keep_console(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(windows))]
fn keep_console(_command: &mut std::process::Command) {}

#[cfg(not(windows))]
pub(crate) fn detach(_command: &mut std::process::Command) {}

/// Let the shell that is about to be pushed a target come forward.
///
/// **The pid is the operating system's answer**, read off the transport when
/// the socket was accepted — never one the shell claimed, because
/// `AllowSetForegroundWindow` is a capability and handing it to a self-reported
/// pid gives it to whoever guessed.
#[derive(Copy, Clone, Debug)]
pub(crate) struct ForegroundAttempt {
    allowed: bool,
    hwnd: Option<usize>,
    was_visible: bool,
    was_minimized: bool,
    wake_posted: bool,
    show_requested: bool,
    foregrounded: bool,
    verified_foreground: bool,
}

impl std::fmt::Display for ForegroundAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "allowed={} hwnd={:?} was_visible={} was_minimized={} wake_posted={} show_requested={} foregrounded={} verified_foreground={}",
            self.allowed,
            self.hwnd,
            self.was_visible,
            self.was_minimized,
            self.wake_posted,
            self.show_requested,
            self.foregrounded,
            self.verified_foreground
        )
    }
}

#[cfg(windows)]
pub(crate) fn foreground(pid: u32) -> ForegroundAttempt {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, EnumWindows, GetForegroundWindow, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, PostMessageW, SW_RESTORE, SetForegroundWindow, ShowWindowAsync,
        WM_NULL,
    };
    use windows::core::BOOL;

    struct Search {
        pid: u32,
        any: HWND,
        visible: HWND,
    }
    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> BOOL {
        let state = unsafe { &mut *(state.0 as *mut Search) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == state.pid {
            if state.any.0.is_null() {
                state.any = window;
            }
            if unsafe { IsWindowVisible(window).as_bool() } {
                state.visible = window;
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    // Grant first: the pushed target makes the shell call `focus_window`, and
    // that call needs permission even if our direct activation races it.
    let allowed = unsafe { AllowSetForegroundWindow(pid) }.is_ok();
    let mut search = Search {
        pid,
        any: HWND(std::ptr::null_mut()),
        visible: HWND(std::ptr::null_mut()),
    };
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    let window = if search.visible.0.is_null() {
        search.any
    } else {
        search.visible
    };
    if window.0.is_null() {
        return ForegroundAttempt {
            allowed,
            hwnd: None,
            was_visible: false,
            was_minimized: false,
            wake_posted: false,
            show_requested: false,
            foregrounded: false,
            verified_foreground: false,
        };
    }
    let was_visible = unsafe { IsWindowVisible(window).as_bool() };
    let was_minimized = unsafe { IsIconic(window).as_bool() };
    let wake_posted = unsafe { PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0)).is_ok() };
    let show_requested = !was_visible || was_minimized;
    if show_requested {
        // The shell will restore all of its panes when it drains `Push::Open`.
        // Showing one HWND here closes the gap in which Windows otherwise has
        // nothing visible to activate and silently ignores the foreground ask.
        let _ = unsafe { ShowWindowAsync(window, SW_RESTORE) };
    }
    let foregrounded = unsafe { SetForegroundWindow(window).as_bool() };
    let verified_foreground = unsafe { GetForegroundWindow().0 == window.0 };
    let attempt = ForegroundAttempt {
        allowed,
        hwnd: Some(window.0 as usize),
        was_visible,
        was_minimized,
        wake_posted,
        show_requested,
        foregrounded,
        verified_foreground,
    };
    koru_ipc::ipc_debug!("foreground: pid={pid} attempt={attempt:?}");
    attempt
}

/// Deliberately all-false off Windows, and on macOS that is probably the final
/// answer rather than a gap: the Windows dance exists because
/// `AllowSetForegroundWindow` is a *grant* one process makes to another, and
/// macOS has no such grant to give. There, activation belongs to the target —
/// the shell self-activates (`koru_window::activate_app`) on the paths that
/// drain a routed push, which is the only way AppKit allows it to happen. If
/// macOS 14's cooperative-activation rules ever refuse that self-activation,
/// the contingency is `NSRunningApplication`'s `activate` from here — build it
/// then, against the observed refusal, not now against a guess.
#[cfg(not(windows))]
pub(crate) fn foreground(_pid: u32) -> ForegroundAttempt {
    ForegroundAttempt {
        allowed: false,
        hwnd: None,
        was_visible: false,
        was_minimized: false,
        wake_posted: false,
        show_requested: false,
        foregrounded: false,
        verified_foreground: false,
    }
}

impl WindowApp for Agent {
    type Event = AgentEvent;

    fn window(&self) -> WindowAttributes {
        #[cfg(windows)]
        use winit::platform::windows::WindowAttributesExtWindows;

        if self.voice_window.visible() {
            return self.voice_window.window(window_icon());
        }

        #[allow(unused_mut)]
        let height = search_window_height(&self.search_geometry);
        let mut attributes = winit::window::Window::default_attributes()
            .with_title("Koru Search")
            .with_inner_size(winit::dpi::LogicalSize::new(SEARCH_WIDTH, height))
            .with_min_inner_size(winit::dpi::LogicalSize::new(
                SEARCH_WIDTH,
                search_min_height(),
            ))
            .with_max_inner_size(winit::dpi::LogicalSize::new(
                SEARCH_WIDTH,
                search_max_height(),
            ))
            // **Above everything, and out of the taskbar.** This is a panel
            // that appears over whatever the person was doing, not a window
            // they switched to: a taskbar button for it is a second way to
            // "open" something that is dismissed by clicking away, and Alt+Tab
            // would cycle through a thing with no persistent state.
            .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
            // **The taskbar and Alt+Tab read this, not the exe's resource.**
            // A `winit` window with no icon shows the system default — which is
            // what the overlay did: a blank page beside the app it belongs to.
            // The exe's resource is what the *notification area* reads, and the
            // two are separate settings for the same picture.
            .with_window_icon(window_icon());
        if let Some(position) = self.search_geometry.position() {
            attributes = attributes.with_position(position);
        }
        // **Borderless everywhere except macOS, where borderless means square
        // corners.** The shell's search panel learned this first: dropping the
        // `Titled` style mask is the obvious way to lose the traffic lights and
        // the wrong one, because the platform's corner radius and shadow go
        // with it — which is exactly what makes a floating panel look
        // unfinished beside every other Mac window. So the mask stays and the
        // furniture is hidden instead; the *fullsize content view* is the one
        // addition over the shell's recipe, because this panel draws its own
        // drag rail at the top and a native 28 px strip above it would be a
        // second, empty caption.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes = attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
                .with_titlebar_buttons_hidden(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            attributes = attributes.with_decorations(false);
        }
        // `skip_taskbar` is Windows-only in winit; elsewhere an always-on-top
        // panel with no buttons is already the closest thing to a panel.
        #[cfg(windows)]
        {
            attributes = attributes.with_skip_taskbar(true);
        }
        attributes
    }

    /// **Windowless until asked.** An agent lives in the notification area.
    fn wants_window(&self) -> bool {
        self.overlay.is_some() || self.voice_window.visible()
    }

    fn window_activation(&self) -> koru_window::WindowActivation {
        self.voice_window.window_activation()
    }

    fn window_generation(&self) -> u64 {
        if self.voice_window.visible() {
            self.voice_window.generation()
        } else {
            1
        }
    }

    fn should_exit(&self) -> bool {
        self.exit
    }

    /// Closing the overlay puts it away; it does not end the agent. The trait
    /// requires an answer here because a transient window and its process have
    /// independent lifetimes.
    fn close_requested(&mut self) {
        if self.voice_window.close_all()
            || matches!(
                self.window_transition,
                Some(WindowTransition::ClosingVoice | WindowTransition::ReadyVoice)
            )
        {
            self.voice_start = None;
            self.voice_open_waiting = false;
            self.window_transition = None;
            self.voice_attempt = None;
            if let Some(coordinator) = &self.global_dictation {
                coordinator.discard_prepared();
            }
        } else {
            self.window_transition = None;
            self.close_overlay();
        }
    }

    /// **Clicking away dismisses it.** An overlay over another application has
    /// no other way to be dismissed: the click that means "not this" lands
    /// somewhere else entirely, so there is nothing to hit-test and losing the
    /// focus is the whole of the signal.
    ///
    /// Gaining focus is not a reason to do anything — it is what opening it
    /// did.
    fn focus_changed(&mut self, focused: bool) -> bool {
        if focused {
            self.ignore_blur_until = None;
            return false;
        }
        if self.voice_window.visible() {
            return false;
        }
        if self
            .ignore_blur_until
            .is_some_and(|until| Instant::now() < until)
        {
            return false;
        }
        if self.overlay.is_some() {
            self.interrupt_dictation(koru_dictation::Interruption::FocusLost);
            return self.close_overlay();
        }
        false
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.overlay.is_some() && on_drag_handle(SEARCH_WIDTH, x, y)
    }

    fn cursor_icon(&self, x: f32, y: f32) -> winit::window::CursorIcon {
        if self
            .voice_window
            .surface()
            .is_some_and(koru_voice_ui::VoiceSurface::hovering_action)
        {
            // Koru press targets keep the desktop arrow; their drawn hover
            // and pressed states communicate actionability (ADR-215).
            winit::window::CursorIcon::Default
        } else if self.is_caption(x, y) {
            winit::window::CursorIcon::Move
        } else {
            winit::window::CursorIcon::Default
        }
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        if self.overlay.is_none() || maximised {
            return None;
        }
        // Full-width top edge (including over the pill) and the rest of the
        // rail off the pill. The pill itself, below the edge band, only moves.
        if on_drag_handle(width, x, y) {
            return None;
        }
        if on_top_resize_edge(y) || (y >= 0.0 && y < DRAG_RAIL_H) {
            return Some(ResizeDirection::North);
        }
        vertical_resize_direction(width, height, x, y, maximised)
    }

    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        if self.overlay.is_some() && size.height > 0 && scale_factor > 0.0 {
            let logical = size.height as f32 / scale_factor as f32;
            self.search_geometry.resized(logical);
            // Write on every size change, not only at teardown: focus-loss
            // close can race the last `Resized`, and the geometry worker
            // already coalesces.
            self.search_geometry.persist();
        }
    }

    /// The overlay is painted before it is shown, so a hotkey never puts an
    /// empty rectangle on screen while the results arrive.
    fn paint_before_showing(&self) -> bool {
        true
    }

    fn moved(&mut self, position: winit::dpi::PhysicalPosition<i32>) {
        if self.overlay.is_some() {
            self.search_geometry.moved(position);
        }
    }

    fn wake(&mut self, event: AgentEvent) -> bool {
        match event {
            AgentEvent::ReminderWorkerChanged { report } => {
                let previous = self.reminder_report.failure;
                let (report, changed) = merge_reminder_report(&self.reminder_report, report);
                if changed {
                    match report.failure {
                        Some(category) => tracing::warn!(
                            category = category.as_str(),
                            "reminder scheduler entered a failure state"
                        ),
                        None if previous.is_some() => {
                            tracing::info!("reminder scheduler recovered")
                        }
                        None => {}
                    }
                }
                self.reminder_report = report;
                self.broadcast_reminder_diagnostics();
                false
            }
            AgentEvent::ReminderOpen { target } => {
                let entity = &target.entity;
                tracing::info!(
                    target: "koru::notification_activation",
                    profile = %target.profile.to_storage(),
                    workspace = %entity.workspace().to_storage(),
                    document = %entity.document().to_storage(),
                    node = %entity.node(),
                    "Agent event loop received the resolved Open target"
                );
                self.open_target(koru_search_ui::Target::Store {
                    profile: target.profile,
                    workspace: entity.workspace(),
                    document: Some(entity.document()),
                    node: Some(entity.node().to_string()),
                    trashed: false,
                    match_text: None,
                    disposition: koru_search_ui::Disposition::CurrentWindow,
                });
                false
            }
            AgentEvent::ReminderProfileOpen { profile } => {
                self.open_reminder_profile(profile, |_, profile, root| {
                    spawn_profile_shell(profile, root)
                });
                false
            }
            AgentEvent::ReminderSnooze { target } => {
                let profile = target.profile;
                let workspace = target.entity.workspace();
                let delivery = uuid::Uuid::new_v4().to_string();
                self.reminder_snoozes.push_back(PendingReminderSnooze {
                    profile,
                    workspace,
                    intent: koru_agent_wire::ReminderSnoozeIntent {
                        delivery,
                        profile: profile.to_storage(),
                        reminder: target.reminder.to_storage(),
                        occurrence: target.occurrence.to_storage(),
                        entity: target.entity.to_storage(),
                    },
                    assigned: None,
                });
                let entity = &target.entity;
                self.open_target(koru_search_ui::Target::Store {
                    profile,
                    workspace,
                    document: Some(entity.document()),
                    node: Some(entity.node().to_string()),
                    trashed: false,
                    match_text: None,
                    disposition: koru_search_ui::Disposition::CurrentWindow,
                });
                if let Some(connection) = self
                    .registry
                    .route(profile, workspace)
                    .or_else(|| self.registry.route_profile(profile))
                    .map(|shell| shell.connection)
                {
                    self.push_pending_reminder_snoozes(connection, profile, workspace);
                }
                false
            }
            AgentEvent::NotificationControlAnswered {
                connection,
                id,
                result,
            } => {
                self.reply(connection, id, Response::NotificationControl { result });
                false
            }
            AgentEvent::Dictation => self.service_dictation(),
            AgentEvent::AiHotkey if !koru_features::ai() => false,
            AgentEvent::AiHotkey => {
                if self.restart_gate_pending() {
                    self.show_restart_gate_warning();
                    return true;
                }
                if let Some(waker) = self.event_waker.clone() {
                    crate::ai_chat::probe_off_thread(
                        self.paths.explicit_root().map(Path::to_path_buf),
                        waker,
                    );
                }
                true
            }
            AgentEvent::AiChatProbed { process } => {
                if self.restart_gate_pending() {
                    self.show_restart_gate_warning();
                    return true;
                }
                match process {
                    Some(process) if process.focused => {
                        if let Some((connection, pid)) = self
                            .registry
                            .most_recent()
                            .map(|shell| (shell.connection, shell.pid))
                        {
                            if let Some(pid) = pid {
                                let _ = foreground(pid);
                            }
                            self.push(connection, Push::Restore);
                        } else if let Err(error) = spawn_shell_bare(self.paths.explicit_root()) {
                            tracing::error!(%error, "Koru workspace could not be opened");
                        }
                    }
                    _ => {
                        self.open_ai_chat_for_agent(crate::ai_chat::Open::Landing);
                    }
                }
                true
            }
            AgentEvent::AiChatOpened { error } => {
                let Some(error) = error else { return false };
                let was_closed = self.overlay.is_none();
                self.open_overlay();
                let changed = self
                    .overlay
                    .as_mut()
                    .is_some_and(|surface| surface.set_host_warning(Some(ai_chat_warning(&error))));
                was_closed || changed
            }
            AgentEvent::Tray(TrayAsk::AiChat) => {
                self.open_ai_chat_for_agent(crate::ai_chat::Open::Landing);
                true
            }
            AgentEvent::Hotkey(crate::event::HotkeyPress::Search { trigger }) => {
                // **A toggle, not an idempotent open.** Pressing it again with
                // the overlay already up did nothing visible, which reads as
                // the shortcut having stopped working — and it was the second
                // half of the overlay never closing at all.
                if self.overlay.is_some() {
                    self.close_overlay();
                } else {
                    self.open_overlay();
                    self.quarantine_the_chord(trigger);
                }
                true
            }
            AgentEvent::Hotkey(crate::event::HotkeyPress::Dictation { key, foreground }) => {
                if let Some(coordinator) = &self.global_dictation {
                    let destination = dictation_destination(
                        foreground,
                        std::process::id(),
                        self.overlay.is_some(),
                        &self.registry,
                    );
                    let route = match destination {
                        DictationDestination::Global => "global",
                        DictationDestination::Local(crate::event::LocalDictationOwner::Agent) => {
                            "local-agent"
                        }
                        DictationDestination::Local(
                            crate::event::LocalDictationOwner::Workspace(_),
                        ) => "local-workspace",
                        DictationDestination::Refuse => "refused",
                    };
                    // This is deliberately content-free: it records neither
                    // the foreground PID/application nor any dictated text.
                    // A shortcut that visibly records but types nowhere must
                    // still leave enough evidence to distinguish routing from
                    // capture, recognition, and insertion failures.
                    self.routing_log
                        .event(format_args!("dictation hotkey route={route}"));
                    match destination {
                        DictationDestination::Global => {
                            self.voice_attempt = Some(VoiceAttemptOrigin::Hotkey);
                            coordinator.press(None, key);
                        }
                        DictationDestination::Local(owner) => coordinator.press(Some(owner), key),
                        DictationDestination::Refuse => {}
                    }
                }
                false
            }
            AgentEvent::LocalDictation(gesture) => {
                let (route, owner, key) = match gesture {
                    crate::event::LocalDictationGesture::Press { route, owner, key }
                    | crate::event::LocalDictationGesture::Release { route, owner, key }
                    | crate::event::LocalDictationGesture::Cancel { route, owner, key } => {
                        (route, owner, key)
                    }
                };
                match owner {
                    crate::event::LocalDictationOwner::Agent => {
                        self.route_agent_local_dictation(route, gesture)
                    }
                    crate::event::LocalDictationOwner::Workspace(connection) => {
                        let key = match key.raw() {
                            value if value == b'D' as u16 => koru_agent_wire::DictationKey::D,
                            _ => koru_agent_wire::DictationKey::M,
                        };
                        let gesture = match gesture {
                            crate::event::LocalDictationGesture::Press { .. } => {
                                koru_agent_wire::DictationGesture::Press { key }
                            }
                            crate::event::LocalDictationGesture::Release { .. } => {
                                koru_agent_wire::DictationGesture::Release { key }
                            }
                            crate::event::LocalDictationGesture::Cancel { .. } => {
                                koru_agent_wire::DictationGesture::Cancel { key }
                            }
                        };
                        if matches!(gesture, koru_agent_wire::DictationGesture::Press { .. }) {
                            match self.workspace_dictation_route {
                                None => self.workspace_dictation_route = Some((connection, route)),
                                Some(current) if current == (connection, route) => {}
                                Some(_) => return false,
                            }
                        }
                        self.push(connection, Push::DictationGesture { route, gesture });
                        false
                    }
                }
            }
            AgentEvent::GlobalDictationChanged(snapshot) => {
                let previous = self.global_dictation_snapshot;
                self.global_dictation_snapshot = snapshot;
                if snapshot.phase != previous.phase
                    || snapshot.refusal != previous.refusal
                    || snapshot.outcome != previous.outcome
                {
                    // Snapshot is already the coordinator's privacy boundary:
                    // it contains no transcript or target identity. Keep the
                    // routing log useful after a hidden root-stack launch.
                    self.routing_log.event(format_args!(
                        "dictation state phase={:?} refusal={:?} outcome={:?}",
                        snapshot.phase, snapshot.refusal, snapshot.outcome
                    ));
                }
                let origin = self.voice_attempt;
                let mut hud_changed = self.voice_window.sync_hud(snapshot, false, Instant::now());
                if self.voice_open_waiting && snapshot.prepared {
                    self.voice_open_waiting = false;
                    self.open_voice_surface_now();
                } else if self.voice_open_waiting && snapshot.refusal.is_some() {
                    self.voice_open_waiting = false;
                    self.voice_problem = Some(Self::problem_for_global_refusal(snapshot.refusal));
                    self.open_voice_surface_now();
                }
                if matches!(self.voice_start, Some(VoiceStartStage::Starting)) {
                    if snapshot.phase == crate::dictation_coordinator::Phase::Listening {
                        self.voice_start = None;
                    } else if snapshot.refusal.is_some() {
                        self.voice_start = None;
                        self.voice_problem =
                            Some(Self::problem_for_global_refusal(snapshot.refusal));
                        if origin == Some(VoiceAttemptOrigin::Surface) {
                            self.open_voice_surface();
                        } else {
                            hud_changed |=
                                self.voice_window.sync_hud(snapshot, true, Instant::now());
                        }
                        self.voice_attempt = None;
                    }
                }
                if self.tray_voice_start {
                    if snapshot.prepared {
                        self.tray_voice_start = false;
                        self.voice_start = Some(VoiceStartStage::Starting);
                        if let Some(coordinator) = &self.global_dictation {
                            coordinator.start_prepared();
                        }
                    } else if snapshot.refusal.is_some() {
                        self.tray_voice_start = false;
                        self.voice_attempt = None;
                        self.voice_problem =
                            Some(Self::problem_for_global_refusal(snapshot.refusal));
                        hud_changed |= self.voice_window.sync_hud(snapshot, true, Instant::now());
                    }
                }
                let explicit_attempt_ended = self.voice_attempt.is_some()
                    && snapshot.refusal.is_some()
                    && ((previous.phase != crate::dictation_coordinator::Phase::Idle
                        && snapshot.phase == crate::dictation_coordinator::Phase::Idle)
                        || (previous.phase == crate::dictation_coordinator::Phase::Idle
                            && snapshot.phase == crate::dictation_coordinator::Phase::Idle
                            && !snapshot.prepared
                            && self.voice_start.is_none()
                            && !self.tray_voice_start));
                if explicit_attempt_ended {
                    self.voice_attempt = None;
                    self.voice_problem = Some(Self::problem_for_global_refusal(snapshot.refusal));
                    if origin == Some(VoiceAttemptOrigin::Surface) {
                        self.open_voice_surface();
                    } else {
                        hud_changed |= self.voice_window.sync_hud(snapshot, true, Instant::now());
                    }
                } else if self.voice_attempt.is_some()
                    && previous.phase != crate::dictation_coordinator::Phase::Idle
                    && snapshot.phase == crate::dictation_coordinator::Phase::Idle
                {
                    self.voice_attempt = None;
                }
                if self.voice_window.has_surface() && snapshot.refusal.is_some() {
                    self.voice_problem = Some(Self::problem_for_global_refusal(snapshot.refusal));
                }
                hud_changed | self.refresh_voice_surface()
            }
            AgentEvent::SettingsChanged => {
                let settings_changed = self.settings.poll().is_some();
                let changed = self.poll_voice_write();
                if settings_changed || changed {
                    self.apply_appearance();
                }
                self.refresh_global_consent_off_thread();
                if self.settings.authoritative().global_dictation_enabled == Some(false)
                    && self.voice_forced_off
                    && !self
                        .voice_write
                        .as_ref()
                        .is_some_and(|write| matches!(write.intent, VoiceIntent::Disable))
                {
                    self.voice_forced_off = false;
                    if matches!(self.voice_retry, Some(VoiceIntent::Disable)) {
                        self.voice_retry = None;
                        if self.voice_problem == Some(koru_voice_ui::Problem::UnsavedRuntime) {
                            self.voice_problem = None;
                        }
                    }
                }
                self.apply_global_dictation_settings();
                settings_changed | changed | self.refresh_voice_surface()
            }
            AgentEvent::GlobalConsentRefreshed {
                generation,
                consent,
            } => {
                if generation != self.consent_refresh_generation {
                    return false;
                }
                self.global_consent = consent;
                self.apply_global_dictation_settings();
                self.refresh_voice_surface()
            }
            AgentEvent::Tray(TrayAsk::Search) => {
                self.open_overlay();
                true
            }
            AgentEvent::Tray(TrayAsk::Reminders) => {
                if let Some(connection) = self.registry.most_recent().map(|shell| shell.connection)
                {
                    self.push(connection, Push::OpenReminders);
                } else {
                    self.reminder_tray_open_owed = true;
                    self.restore_windows();
                }
                false
            }
            AgentEvent::Tray(TrayAsk::NewAgent) => {
                self.show_workspace_session(None);
                false
            }
            AgentEvent::Tray(TrayAsk::ShowAgentSession(session)) => {
                self.show_workspace_session(Some(session));
                false
            }
            // The watcher read a fresh catalog. Stored, not acted on: the
            // loop reads it after every wake and re-projects the menu — the
            // arrangement every other tray row keeps.
            AgentEvent::AgentsCatalog { catalog } => {
                self.agents_catalog = *catalog;
                false
            }
            // Recorded, not acted on: the tray is owned by the loop, which
            // reads this after every wake. `false`, because nothing on screen
            // changed — the menu is native and redraws itself.
            AgentEvent::HotkeyRegistered {
                action: crate::hotkey::Action::Search,
                shortcut,
            } => {
                self.shortcut = match shortcut {
                    Some(label) => crate::tray::Shortcut::Chord(label),
                    None => crate::tray::Shortcut::Absent,
                };
                // The tray reads `self.shortcut` after every wake; every window
                // learns it through a push. The same truth, two surfaces.
                self.broadcast_shortcut();
                false
            }
            AgentEvent::HotkeyRegistered {
                action: crate::hotkey::Action::Dictation,
                shortcut,
            } => {
                self.dictation_shortcut = match shortcut {
                    Some(label) => crate::tray::Shortcut::Chord(label),
                    None => crate::tray::Shortcut::Absent,
                };
                false
            }
            AgentEvent::DictationHotkeyPrepared {
                generation,
                requested,
                active,
                prepared,
            } => {
                if self.voice_shortcut_prepare.as_ref() == Some(&(generation, requested.clone())) {
                    self.voice_shortcut_prepare = None;
                    self.dictation_shortcut = match active.clone() {
                        Some(label) => crate::tray::Shortcut::Chord(label),
                        None => crate::tray::Shortcut::Absent,
                    };
                    if prepared {
                        self.start_voice_write(VoiceIntent::Shortcut(requested, generation));
                    } else {
                        self.voice_problem = Some(koru_voice_ui::Problem::ShortcutTaken);
                        self.refresh_voice_surface();
                    }
                    return true;
                } else if self
                    .hotkey_preparing
                    .as_ref()
                    .is_some_and(|(seen, value)| *seen == generation && value == &requested)
                {
                    self.hotkey_preparing = None;
                    if prepared {
                        if let Some(control) = &self.hotkey_control {
                            if control.commit_dictation(generation) {
                                self.hotkey_settling = Some((generation, requested.clone()));
                            }
                        }
                    }
                } else {
                    return false;
                }
                self.dictation_shortcut = match active {
                    Some(label) => crate::tray::Shortcut::Chord(label),
                    None => crate::tray::Shortcut::Absent,
                };
                false
            }
            AgentEvent::DictationHotkeySettled { generation, active } => {
                if self.hotkey_settling.as_ref().map(|(seen, _)| *seen) != Some(generation) {
                    return false;
                }
                self.hotkey_settling = None;
                self.dictation_shortcut = match active {
                    Some(label) => crate::tray::Shortcut::Chord(label),
                    None => crate::tray::Shortcut::Absent,
                };
                self.apply_global_dictation_settings();
                false
            }
            AgentEvent::AiHotkeyRegistered { available } => {
                self.ai_shortcut = available;
                false
            }
            AgentEvent::NotificationArea { present } => {
                self.set_notification_area(present);
                false
            }
            AgentEvent::StoreAvailability { warning, running } => {
                self.store_running = Some(running);
                if running {
                    self.complete_startup_transition();
                    if self.handoff_recovering {
                        self.handoff_recovering = false;
                        self.reopen_after_update();
                    }
                }
                self.overlay
                    .as_mut()
                    .is_some_and(|surface| surface.set_availability_warning(warning))
            }
            AgentEvent::PairingChanged => {
                self.settle_pairing_events();
                if let Some(account) = &self.account {
                    account.wake_enrollment();
                }
                false
            }
            AgentEvent::AccountChanged => {
                if let Some(control) = &self.account {
                    let state = control.state();
                    for connection in self
                        .connections
                        .iter()
                        .map(Connection::id)
                        .collect::<Vec<_>>()
                    {
                        self.push(
                            connection,
                            Push::Account {
                                state: state.clone(),
                            },
                        );
                        self.push(connection, Push::AccountChanged);
                    }
                }
                false
            }
            AgentEvent::AccountBrowser {
                owner_scope,
                requester,
                generation,
                attempt,
                url,
            } => {
                self.push(
                    requester,
                    Push::AccountBrowser {
                        owner_scope,
                        generation,
                        attempt,
                        url,
                    },
                );
                false
            }
            AgentEvent::AccountResult {
                owner_scope,
                requester,
                generation,
                result,
            } => {
                if let koru_agent_wire::AccountOperationResult::ProfileLifecycle {
                    lifecycle: Some(lifecycle),
                } = &result
                    && lifecycle.cleanup_stage
                        == Some(koru_agent_wire::ProfileCleanupStage::NavigationSettled)
                {
                    self.pending_profile_cleanup.remove(&lifecycle.profile_id);
                }
                self.push(
                    requester,
                    Push::AccountResult {
                        owner_scope,
                        generation,
                        result,
                    },
                );
                false
            }
            AgentEvent::LocalProfileAccountBound {
                requester: (connection, id),
                profile_id,
                result,
            } => {
                let response = match result {
                    Ok(outcome) => Response::LocalProfileAccountBound {
                        profile_id,
                        outcome,
                    },
                    Err((code, message)) => Response::Failed { code, message },
                };
                self.reply(connection, id, response);
                false
            }
            AgentEvent::LocalProfileRemoved {
                requester: (connection, id),
                profile_id,
                result,
            } => {
                let response = match result {
                    Ok(outcome) => Response::LocalProfileRemoved {
                        profile_id,
                        outcome,
                    },
                    Err((code, message)) => Response::Failed { code, message },
                };
                self.reply(connection, id, response);
                false
            }
            // The update worker moved. Drain it, tell every window, and let an
            // install in progress take its next step — the whole of the
            // agent's update loop is these three lines, because everything that
            // could block happens on a worker and arrives here as an event.
            AgentEvent::UpdateChanged => {
                if let Some(controller) = &self.updates {
                    controller.tick();
                }
                self.note_update_funnel();
                if self.refresh_session_catalog_for_ready() {
                    self.broadcast_update();
                }
                self.advance_handoff();
                false
            }
            AgentEvent::UpdateChoiceMade { choice } => {
                // A dialog answer never crossed the pipe, so it queued for
                // nothing — zero, not an estimate.
                let result =
                    if let koru_agent_wire::UpdateChoice::Install { session_generation } = choice {
                        self.prepare_sessions_for_install(
                            session_generation,
                            None,
                            std::time::Duration::ZERO,
                        )
                    } else {
                        self.apply_update_choice(choice, std::time::Duration::ZERO)
                    };
                if let Err((code, message)) = result {
                    koru_ipc::ipc_debug!("update: {code} — {message}");
                }
                false
            }
            AgentEvent::SessionCatalogRefreshed {
                build_number,
                result,
            } => {
                self.session_catalog_refresh_pending = false;
                let still_ready = self.updates.as_ref().is_some_and(|controller| {
                    matches!(
                        controller.status(),
                        koru_update::UpdateStatus::ReadyToInstall { ref manifest, .. }
                            if manifest.build_number == build_number
                    )
                });
                if still_ready {
                    self.session_catalog = result.unwrap_or(crate::session_host::CatalogFact {
                        generation: 0,
                        active_count: 0,
                    });
                    self.session_catalog_ready_build = Some(build_number);
                    self.broadcast_update();
                }
                false
            }
            AgentEvent::SessionShutdownPrepared { requester, result } => {
                self.session_prepare_pending = false;
                match result {
                    crate::session_host::PrepareResult::Prepared(catalog) => {
                        self.session_catalog = catalog;
                        if let Some(controller) = &self.updates
                            && let koru_update::UpdateStatus::ReadyToInstall { manifest, .. } =
                                controller.status()
                        {
                            crate::service::emit_event(
                                self.paths.clone(),
                                koru_events::AppUpdateAccepted {
                                    to_build: manifest.build_number,
                                }
                                .into(),
                            );
                        }
                        self.begin_install();
                        if let Some((connection, id)) = requester {
                            self.reply(connection, id, Response::Ok);
                        }
                    }
                    crate::session_host::PrepareResult::Refused { message, current } => {
                        if let Some(current) = current {
                            self.session_catalog = current;
                        }
                        if let Some((connection, id)) = requester {
                            self.reply(
                                connection,
                                id,
                                Response::Failed {
                                    code: crate::update::SESSIONS_CHANGED.into(),
                                    message,
                                },
                            );
                        }
                        self.broadcast_update();
                    }
                }
                false
            }
            AgentEvent::SessionHostStopped { outcome } => {
                self.session_stopping = false;
                if self.quit_after_session_stop {
                    // A quit ends the process either way — the Job Object is
                    // the fallback — so this is recorded rather than acted on,
                    // and it now says *how* the host ended rather than only
                    // whether the request was answered.
                    if !outcome.was_graceful() {
                        self.routing_log
                            .event(format_args!("quit session stop: {outcome}"));
                    }
                    self.stop_the_workers_it_owns("quit");
                    self.exit = true;
                    return false;
                }
                if matches!(self.handoff, crate::update::Handoff::StoppingSessions) {
                    // **The update gate fails closed on a timeout.** A forced
                    // kill and a child that had already died both released the
                    // image, so the handoff goes on to the Store; a stop that
                    // did not complete means the supervisor still owns a child
                    // and the image is still in use — a timeout is not
                    // permission to swap.
                    self.routing_log
                        .event(format_args!("install handoff sessions: {outcome}"));
                    self.handoff = if outcome.child_is_gone() {
                        crate::update::Handoff::StoppingStore
                    } else {
                        crate::update::Handoff::Aborted(format!(
                            "terminal sessions did not stop cleanly: {outcome}"
                        ))
                    };
                    self.advance_handoff();
                }
                false
            }
            AgentEvent::AgentQuitDecided { confirmed, catalog } => {
                self.quit_confirmation_pending = false;
                if let Ok(catalog) = catalog {
                    self.session_catalog = catalog;
                }
                if confirmed {
                    self.quit_because_asked();
                }
                false
            }
            AgentEvent::UpdateOfferHandled => {
                self.update_dialog_open = false;
                false
            }
            AgentEvent::Tray(TrayAsk::CheckForUpdates) => {
                if let Some(controller) = &self.updates {
                    controller.check(koru_update::CheckReason::Manual);
                }
                false
            }
            // **`tray::release_notes_url`, not a second `format!` here.** The
            // menu already has to know whether there is a page to point at, in
            // order to grey the row out — and that answer is two questions
            // (built by the release tool, *and* on a channel that publishes
            // anywhere), which an inline string here would get half right.
            AgentEvent::Tray(TrayAsk::About) => {
                if let Some(url) = crate::tray::release_notes_url()
                    && let Err(error) = service::open_location(std::path::Path::new(&url))
                {
                    koru_ipc::ipc_debug!("tray: could not open {url}: {error}");
                }
                false
            }
            AgentEvent::Tray(TrayAsk::SendFeedback) => {
                self.offer_feedback();
                false
            }
            AgentEvent::LinkResolved { target } => {
                // One router: the same call the overlay's own choice and a
                // shell's `Route` both make.
                match from_wire(*target) {
                    Some(target) => self.open_target(target),
                    None => koru_ipc::ipc_debug!("link: the Store placed it somewhere unparseable"),
                }
                false
            }
            AgentEvent::Tray(ask @ (TrayAsk::RegisterUrlScheme | TrayAsk::UnregisterUrlScheme)) => {
                self.set_url_scheme(ask == TrayAsk::RegisterUrlScheme);
                false
            }
            AgentEvent::Tray(TrayAsk::ToggleRelay) => {
                // Decision №6: on/off only, device-local, applied to this
                // channel's runtime immediately. `set_relay_enabled` is an
                // atomic store, a command send and an off-thread persist, so
                // nothing here waits on a network or a disk. The menu's tick
                // is read back from `relay_enabled()` on the way out of this
                // wake — see `run.rs` — rather than tracked here.
                match crate::network::current() {
                    Some(network) => network.set_relay_enabled(!network.relay_enabled()),
                    // The row is greyed when there is no network, so this is
                    // a race with startup at worst — news, not an error.
                    None => koru_ipc::ipc_debug!("tray: no device network to switch relays on"),
                }
                false
            }
            AgentEvent::Tray(TrayAsk::OpenLocation) => {
                if let Err(error) = service::open_location(self.paths.root()) {
                    koru_ipc::ipc_debug!(
                        "tray: could not open location {}: {error}",
                        self.paths.root().display()
                    );
                }
                false
            }
            AgentEvent::Tray(TrayAsk::OpenLocalDataLocation) => {
                if let Some(path) = self.paths.local_data_dir()
                    && let Err(error) = service::open_location(&path)
                {
                    koru_ipc::ipc_debug!(
                        "tray: could not open local data location {}: {error}",
                        path.display()
                    );
                }
                false
            }
            AgentEvent::Tray(TrayAsk::OpenSharedLocation) => {
                if let Some(path) = self.paths.shared_data_dir()
                    && let Err(error) = service::open_location(&path)
                {
                    koru_ipc::ipc_debug!(
                        "tray: could not open shared location {}: {error}",
                        path.display()
                    );
                }
                false
            }
            AgentEvent::Tray(TrayAsk::RefreshStore) => {
                self.ensure_gateway_off_thread(None);
                false
            }
            AgentEvent::Tray(TrayAsk::OpenGatewayDocs) => {
                service::open_gateway_docs(self.paths.clone());
                false
            }
            AgentEvent::Tray(TrayAsk::OpenStoreLocation) => {
                let path = self.paths.store_dir();
                if let Err(error) = service::open_location(&path) {
                    koru_ipc::ipc_debug!(
                        "tray: could not open Store location {}: {error}",
                        path.display()
                    );
                }
                false
            }
            AgentEvent::Tray(TrayAsk::OpenStoreStats) => {
                service::open_store_stats(self.paths.clone(), self.event_waker.clone());
                false
            }
            AgentEvent::Tray(TrayAsk::BackupStore) => {
                service::backup_store(self.paths.clone(), self.event_waker.clone());
                false
            }
            // **Open Workspace has to be able to *open* one.**
            //
            // This only ever pushed `Restore` to the windows that already
            // existed, so with none it did nothing at all — no window, no error,
            // no log. The tray is the whole of Koru when every window is closed,
            // and its one row for getting back in was inert precisely when it
            // was the only way in. Reported from a machine with a fresh store
            // and nothing running.
            //
            // **Launched bare, with no profile and no workspace.** The agent
            // knows neither on a first run, and it should not guess: `koru.exe`
            // already resolves the profile it was last in, falls back to the
            // newest, and creates one when the store holds none. Passing ids the
            // agent invented would override exactly the logic that knows better.
            // `--root` is the one thing carried through, because a rooted agent
            // must not open the real installation's content.
            AgentEvent::Tray(TrayAsk::Restore) => self.restore_windows(),
            // **Re-read, then act.** The row was labelled when the menu was
            // built and a model can arrive — or be removed by another
            // channel's process, since the cache is shared — while it sits
            // there, so what this means is decided now rather than carried on
            // the ask. See `TrayAsk::Voice`.
            // **Release and stop are two asks, and the loop keeps them two.**
            // Freeing the weights leaves voice working and makes the next
            // dictation pay the model load again; stopping ends the process.
            // Both dial the worker on a thread and neither starts one -- a row
            // that could not act because nothing is running was drawn greyed
            // and raised no event at all.
            AgentEvent::Tray(TrayAsk::VoiceRelease) => {
                koru_ipc::ipc_debug!("tray: releasing the voice model");
                self.ask_voice_worker_off_thread(VoiceVerb::Release);
                false
            }
            // **Routed, not applied.** `preferences.conf` has one writer --
            // the shell -- so a microphone chosen in the notification area
            // travels to a window, which sets it and saves. With no window
            // open one is brought back first, the same way the setup row does:
            // a menu row that silently did nothing would be the most confusing
            // possible answer to a press.
            AgentEvent::Tray(TrayAsk::ChooseVoiceInput(choice)) => {
                koru_ipc::ipc_debug!("tray: a voice input device was chosen");
                // Believed at once, so the tick moves under the pointer rather
                // than a round trip later. The shell is still the authority --
                // the next disk read replaces this with what was written.
                self.voice.input = choice.clone();
                let device = choice.name().map(str::to_owned);
                let ids: Vec<ConnectionId> = self.connections.iter().map(|c| c.id()).collect();
                if ids.is_empty() {
                    self.restore_windows();
                    self.voice_input_owed = Some(device);
                } else {
                    for id in ids {
                        self.push(
                            id,
                            Push::VoiceInput {
                                device: device.clone(),
                            },
                        );
                    }
                }
                false
            }
            AgentEvent::Tray(TrayAsk::VoiceStop) => {
                koru_ipc::ipc_debug!("tray: stopping the voice worker");
                self.ask_voice_worker_off_thread(VoiceVerb::Stop);
                false
            }
            // A reading came back. The disk half is re-read beside it, because
            // the two move independently and a menu that refreshed one of them
            // would be half right.
            AgentEvent::VoiceWorkerRead { worker, devices } => {
                let input = self.chosen_input();
                self.voice = read_voice_state(self.paths.explicit_root(), input)
                    .keeping_worker(worker)
                    .keeping_devices(devices);
                false
            }
            AgentEvent::Tray(TrayAsk::Voice) => {
                self.refresh_voice_state();
                if let Some(changed) = self.handle_installed_voice_tray() {
                    return changed;
                }
                match self.voice.row {
                    // Unreachable from a greyed row, and answered anyway: the
                    // rule is that nothing about voice is dialled, started or
                    // waited for on an installation with it switched off.
                    crate::tray::VoiceRow::Unavailable(_) => {
                        koru_ipc::ipc_debug!(
                            "tray: voice is unavailable ({} is set)",
                            koru_voice::DISABLE_VAR
                        );
                    }
                    // **Bring a window forward and warm the worker**, which is
                    // the whole of what the tray can do towards a dictation:
                    // the composition belongs to a focused editable surface,
                    // and the agent has none. The chord the row just taught is
                    // what begins it, and the worker being up already is what
                    // makes the first hold instant.
                    crate::tray::VoiceRow::Installed => unreachable!("handled above"),
                    // **Hand the question to a window**, which is the whole of
                    // what the agent can do towards a download: the consent
                    // step is a modal panel and the progress toast is a toast,
                    // and this process draws neither. The window fetches and
                    // verifies the catalog itself — see `koru_agent_wire::Push`
                    // for why the offer's terms do not travel on the message
                    // (Q-V67).
                    crate::tray::VoiceRow::NotInstalled { .. }
                    | crate::tray::VoiceRow::Downloading { .. } => {
                        koru_ipc::ipc_debug!("tray: asking a window to set voice input up");
                        self.offer_voice_setup();
                        return self.restore_windows();
                    }
                }
                false
            }
            AgentEvent::Tray(TrayAsk::VoiceSettings) => {
                self.open_voice_surface_fresh();
                true
            }
            AgentEvent::Tray(TrayAsk::OpenWorkspace(connection)) => {
                self.open_workspace_window(connection);
                false
            }
            // **The icon's left button brings back the window in use.** With no
            // workspace open it is the Search overlay the click has always
            // opened — the one thing left that acts on this process itself.
            AgentEvent::Tray(TrayAsk::IconClick) => {
                if let Some(connection) = self.registry.most_recent().map(|shell| shell.connection)
                {
                    self.open_workspace_window(connection);
                    false
                } else {
                    self.open_overlay();
                    true
                }
            }
            // **Quitting the Agent takes the Store with it.**
            //
            // The tray icon is the only handle a person has on this stack. The
            // Store draws nothing, has no window and no icon of its own, so an
            // Agent that quit and left it behind left a process running that
            // the person had no way to see and no reason to expect — after
            // using the one control that says *Quit*.
            //
            // **Even with a window open**, since the Gateway became a
            // supervised child: it cannot outlive this process, so the only
            // choice left was whether it is asked to stop or killed
            // ([ADR-319](../../../docs/decisions/ADR-301-325.md)).
            //
            // **And no window brings the Agent back.** Every window is told
            // this is a quit rather than a crash (`Push::Quitting`), because a
            // window's answer to an ended link is otherwise to start a new
            // Agent — which made *Quit* restart the stack it had just stopped.
            AgentEvent::Tray(TrayAsk::Quit) => {
                self.request_agent_quit();
                false
            }
            AgentEvent::ShellArrived { connection, pid } => {
                koru_ipc::ipc_debug!("shell {}: arrived pid={pid:?}", connection.0);
                self.routing_log.event(format_args!(
                    "transport arrived connection={} pid={pid:?}",
                    connection.0
                ));
                self.peer_pids.insert(connection, pid);
                self.registry.pid_arrived(connection, pid);
                false
            }
            AgentEvent::ShellSaid {
                connection,
                id,
                body,
                read_at,
            } => {
                self.said(connection, id, *body, read_at.elapsed());
                false
            }
            AgentEvent::ShellGone { connection } => {
                if let Some(control) = &self.account {
                    control.disconnect(connection.0);
                }
                self.account_generations.remove(&connection);
                self.drop_connection(connection);
                // A window closing is how an install in progress makes
                // progress. Harmless otherwise: `shell_gone` only looks at the
                // state that is waiting for one.
                let left = self.connections.len();
                self.handoff.shell_gone(left, Instant::now());
                self.advance_handoff();
                // A window leaving may have taken sessions with it.
                self.kick_agents_menu();
                false
            }
        }
    }

    /// **What was missing, and it is why the overlay said *Searching…* for
    /// ever.** The reader answers on another thread; `maintain` is what applies
    /// the answer, and until the shared loop grew a deadline nothing ever
    /// called it — so a query was sent, a reply arrived, and the surface never
    /// looked.
    fn maintenance_deadline(&self) -> Option<Instant> {
        let overlay = self
            .overlay
            .as_ref()
            .and_then(|surface| surface.maintenance_deadline());
        [
            overlay,
            self.voice_write
                .as_ref()
                .and_then(|write| write.receipt.deadline()),
            (self.window_transition.is_some()
                || matches!(
                    self.voice_start,
                    Some(VoiceStartStage::Closing | VoiceStartStage::Prepare)
                ))
            .then(Instant::now),
            self.router.next_deadline(),
            self.deferred_open.as_ref().map(|open| open.deadline),
            // A window that never answers must not leave the install
            // half-begun forever. Only the waiting state has a deadline, so
            // this adds no wake-up to an idle agent.
            self.handoff.deadline(),
            self.startup_gate_retry_at,
            self.dictation.deadline(),
            self.voice_window.maintenance_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn maintain(&mut self, now: Instant) -> bool {
        if self
            .startup_gate_retry_at
            .is_some_and(|deadline| now >= deadline)
        {
            if self.store_running == Some(true) {
                self.complete_startup_transition();
            } else {
                // Readiness may have fallen away while the updater still held
                // its restart gate. Do not consume reopen intent against a
                // Store we now know is down; the next running=true event
                // re-enters the transition.
                self.startup_gate_retry_at = None;
            }
        }
        let mut changed = self.poll_voice_write();
        changed |= self.voice_window.maintain(now);
        changed |= self
            .overlay
            .as_mut()
            .is_some_and(|surface| surface.maintain(now));
        if let Some(transition) = self.window_transition.take() {
            match transition {
                WindowTransition::ClosingSearch => {
                    self.window_transition = Some(WindowTransition::ReadySearch);
                }
                WindowTransition::ReadySearch => self.open_overlay(),
                WindowTransition::ClosingVoice => {
                    self.window_transition = Some(WindowTransition::ReadyVoice);
                }
                WindowTransition::ReadyVoice => self.prepare_voice_surface(),
            }
            changed = true;
        }
        if let Some(stage) = self.voice_start.take() {
            match stage {
                VoiceStartStage::Closing => {
                    self.voice_start = Some(VoiceStartStage::Prepare);
                }
                VoiceStartStage::Prepare => {
                    if let Some(coordinator) = &self.global_dictation {
                        coordinator.start_prepared();
                        self.voice_start = Some(VoiceStartStage::Starting);
                    } else {
                        self.voice_problem = Some(koru_voice_ui::Problem::TargetUnavailable);
                        self.open_voice_surface();
                    }
                }
                VoiceStartStage::Starting => {
                    self.voice_start = Some(VoiceStartStage::Starting);
                }
            }
            changed = true;
        }
        changed |= self.service_dictation();
        // Give up on an install whose windows never closed. `expire` is a no-op
        // unless one is waiting, and the abort it produces is what puts the
        // banner back rather than leaving the person with nothing.
        if self.handoff.deadline().is_some() {
            self.handoff.expire(now);
            self.advance_handoff();
        }
        if self
            .deferred_open
            .as_ref()
            .is_some_and(|open| now >= open.deadline)
            && let Some(mut open) = self.deferred_open.take()
        {
            match open.stage {
                DeferredStage::ClosingOverlay => {
                    // `koru-window::settle` runs maintenance before matching
                    // `wants_window == false` and dropping its live window.
                    // Re-arm for one immediate loop turn so activation happens
                    // only after that always-on-top HWND is actually gone.
                    open.stage = DeferredStage::Ready;
                    open.deadline = now;
                    self.routing_log.event(format_args!(
                        "overlay teardown barrier target={}",
                        search_target_label(&open.target)
                    ));
                    self.deferred_open = Some(open);
                }
                DeferredStage::Ready => {
                    self.routing_log.event(format_args!(
                        "deferred target routing target={}",
                        search_target_label(&open.target)
                    ));
                    self.open_target(open.target);
                }
            }
        }
        for (profile, workspace) in self.router.expire(now) {
            koru_ipc::ipc_debug!(
                "launch: gave up waiting for profile={} workspace={}",
                profile.to_storage(),
                workspace.to_storage()
            );
            self.routing_log.event(format_args!(
                "pending expired profile={} workspace={} pending_after={}",
                profile.to_storage(),
                workspace.to_storage(),
                self.router.pending_count()
            ));
        }
        changed
    }

    fn animating(&self) -> bool {
        self.overlay.as_ref().is_some_and(|s| s.animating()) || self.voice_window.animating()
    }

    fn scale_factor_changed(&mut self, _scale_factor: f64) -> bool {
        self.voice_window.refresh_hud_placement()
    }

    fn scroll_by(&mut self, delta: f32, _mods: winit::keyboard::ModifiersState) -> bool {
        if self.voice_window.visible() {
            return false;
        }
        let Some(surface) = self.overlay.as_mut() else {
            return false;
        };
        // The surface takes winit's own delta, because it decides what a notch
        // means for a list of results; the loop has already turned pixels into
        // logical ones, so a line delta is what is left to hand back.
        surface.scroll_by(winit::event::MouseScrollDelta::LineDelta(0.0, delta));
        true
    }

    fn mouse_up(&mut self) -> bool {
        if self.voice_window.has_surface() {
            let action = self
                .voice_window
                .surface_mut()
                .and_then(koru_voice_ui::VoiceSurface::mouse_up);
            if let Some(action) = action {
                self.handle_voice_action(action);
            }
            return true;
        }
        let changed = self.overlay.as_mut().is_some_and(|s| s.mouse_up());
        let opened = self.finish_overlay_target("mouse_up");
        changed | opened
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        if self.voice_window.has_surface() {
            return self
                .voice_window
                .surface_mut()
                .is_some_and(|surface| surface.pointer_moved(x, y));
        }
        self.overlay
            .as_mut()
            .is_some_and(|s| s.mouse_move(x, y - DRAG_RAIL_H))
    }

    fn pointer_left(&mut self) -> bool {
        if let Some(surface) = self.voice_window.surface_mut() {
            return surface.pointer_left();
        }
        false
    }

    fn click(&mut self, _engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool {
        if let Some(surface) = self.voice_window.surface_mut() {
            return surface.mouse_down(x, y);
        }
        if self.voice_window.has_hud() {
            if self.voice_window.hud_action_at(x, y) == Some(koru_voice_ui::HudAction::Finish) {
                if let Some(coordinator) = &self.global_dictation {
                    coordinator.finish();
                }
            }
            return true;
        }
        let interrupted = self.interrupt_dictation(koru_dictation::Interruption::LocalEvent);
        let changed = self
            .overlay
            .as_mut()
            .is_some_and(|s| s.mouse_down(x, y - DRAG_RAIL_H, ctrl));
        let opened = self.finish_overlay_target("mouse_down");
        interrupted | changed | opened
    }

    fn double_click(&mut self, _engine: &mut TextEngine, x: f32, y: f32) -> bool {
        if self.voice_window.has_hud() {
            return true;
        }
        let interrupted = self.interrupt_dictation(koru_dictation::Interruption::LocalEvent);
        let changed = self
            .overlay
            .as_mut()
            .is_some_and(|s| s.mouse_double_click(x, y - DRAG_RAIL_H));
        let opened = self.finish_overlay_target("double_click");
        interrupted | changed | opened
    }

    fn physical_key_pressed(
        &mut self,
        _engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        if !repeat
            && self
                .voice_window
                .surface()
                .is_some_and(koru_voice_ui::VoiceSurface::picking)
        {
            let outcome = self
                .voice_window
                .surface_mut()
                .expect("the picker surface was present above")
                .shortcut_key(physical, mods);
            if let Some(action) = outcome.action {
                self.handle_voice_action(action);
            }
            if outcome.changed {
                return true;
            }
        }
        let chord = koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord;
        if !chord.matches(key, text, mods) {
            return false;
        }
        if !repeat && self.dictation_key.is_none() && self.overlay.is_some() {
            self.begin_search_dictation(physical);
        }
        true
    }

    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.finish_search_dictation(physical)
    }

    fn key(
        &mut self,
        _engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if let Some(surface) = self.voice_window.surface_mut() {
            let outcome = surface.key(key, mods);
            if let Some(action) = outcome.action {
                self.handle_voice_action(action);
            }
            return outcome.changed;
        }
        let interrupted = if self.dictation.is_live() {
            let why = if matches!(key, Key::Named(NamedKey::Escape)) {
                koru_dictation::Interruption::Escape
            } else {
                koru_dictation::Interruption::LocalEvent
            };
            self.interrupt_dictation(why)
        } else {
            false
        };
        let Some(surface) = self.overlay.as_mut() else {
            return interrupted;
        };
        let changed = surface.key(key, text, mods);
        let opened = self.finish_overlay_target("key");
        interrupted | changed | opened
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        if self.voice_window.draw(scene, engine, w, h) {
            return;
        }
        let pins = self.registry.pinned_rows();
        if let Some(surface) = self.overlay.as_mut() {
            surface.set_pinned(&pins);
            let p = theme::p();
            scene::fill(
                scene,
                vello::kurbo::Affine::IDENTITY,
                p.bg,
                &scene::rect(0.0, 0.0, w, h),
            );
            let handle_x = drag_handle_x(w);
            let handle_y = (DRAG_RAIL_H - DRAG_HANDLE_H) * 0.5;
            scene::fill(
                scene,
                vello::kurbo::Affine::IDENTITY,
                p.panel_border,
                &scene::rrect(
                    handle_x,
                    handle_y,
                    DRAG_HANDLE_W.min(w),
                    DRAG_HANDLE_H,
                    DRAG_HANDLE_H * 0.5,
                ),
            );
            surface.set_viewport(w, (h - DRAG_RAIL_H).max(1.0));
            surface.draw(
                scene,
                vello::kurbo::Affine::translate((0.0, f64::from(DRAG_RAIL_H))),
                true,
                (0.0, 0.0),
            );
        }
    }

    fn take_key_quarantine(&mut self) -> Vec<winit::keyboard::PhysicalKey> {
        std::mem::take(&mut self.quarantine)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        let request = self.overlay.as_mut().and_then(|s| s.take_window_request());
        if request == Some(WindowRequest::Close) {
            // SearchSurface can say what Escape or its caption button means,
            // but Agent is the WindowApp and therefore owns whether a window
            // exists. Bridge the request before the shared loop settles.
            self.close_overlay();
        }
        request
    }
}

fn shell_warning(error: &std::io::Error) -> String {
    if error.to_string().contains("is not beside this executable") {
        "Koru app was not found beside Koru Agent. Build or install it, then try again.".into()
    } else {
        "Koru could not open a window for that result. Check the installation and try again.".into()
    }
}

fn ai_chat_warning(error: &str) -> String {
    let lang = koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
    if error.contains("is not beside this executable") {
        koru_i18n::t(lang, koru_i18n::Key::AiChatAppMissing).into()
    } else {
        koru_i18n::t(lang, koru_i18n::Key::AiChatOpenFailed).into()
    }
}

/// The overlay's window icon, decoded from the same `.ico` the exe embeds.
///
/// Compiled in with `include_bytes!` rather than read from `assets/` at run
/// time: a bundle with no asset folder must still show the right picture, which
/// is the same argument `Icon::from_resource` makes for the tray. **Failure is
/// `None` and never fatal** — a window with the default icon is a cosmetic
/// disappointment, and one that refused to open would not be.
fn window_icon() -> Option<winit::window::Icon> {
    const ICO: &[u8] = include_bytes!("../../../assets/koru.ico");
    // The 64px entry: large enough for Alt+Tab, small enough that the badge is
    // still the shape it was drawn as rather than a resample of a resample.
    let decoded = image::load_from_memory_with_format(ICO, image::ImageFormat::Ico).ok()?;
    let rgba = decoded.to_rgba8();
    let (w, h) = rgba.dimensions();
    // The embedded bytes are the brand teal; the channel tint is applied here
    // rather than by swapping the asset, the same recipe the exe's own
    // resource icon went through in `scripts/icons/set-logo.py`.
    let mut pixels = rgba.into_raw();
    koru_channel::tint::apply(&mut pixels, koru_channel::tint::tint());
    winit::window::Icon::from_rgba(pixels, w, h).ok()
}

#[cfg(test)]
mod tests;
