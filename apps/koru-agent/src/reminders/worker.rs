// SPDX-License-Identifier: MPL-2.0

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use koru_native_notifications::{Authorization, Capability, FailureCode, NativeNotifications};
use koru_store_core::reminders::ToneId;

use super::{
    Clock, DeliveryPreferences, EventCursor, EventsClientCursor, FileDeliveryPreferences,
    FileLedger, Ledger, MemoryDeliveryPreferences, ReminderStore, Scheduler, SchedulerFailure,
    SchedulerReport, StoreClientPort, SystemClock, SystemZoneProvider, Trigger, ZoneProvider,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Events,
    Reconnect,
    DeviceZoneChanged,
    Resume,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationControl {
    Inspect,
    RequestAuthorization,
    OpenSettings,
    SetDeliveryEnabled {
        enabled: bool,
    },
    Repair,
    SendTest {
        tone: ToneId,
        content: koru_native_notifications::Content,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationControlResponse {
    Capability(Capability),
    Authorization(Authorization),
    Done,
    Failed(FailureCode),
}

pub struct NotificationControlReply(std::sync::mpsc::Receiver<NotificationControlResponse>);

impl NotificationControlReply {
    pub fn recv_timeout(
        self,
        timeout: Duration,
    ) -> Result<NotificationControlResponse, std::sync::mpsc::RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationControlQueueFull;

struct ControlEnvelope {
    request: NotificationControl,
    reply: std::sync::mpsc::SyncSender<NotificationControlResponse>,
}

impl Command {
    fn trigger(&self) -> Option<Trigger> {
        match self {
            Self::Events => Some(Trigger::Events),
            Self::Reconnect => Some(Trigger::Reconnect),
            Self::DeviceZoneChanged => Some(Trigger::DeviceZoneChanged),
            Self::Resume => Some(Trigger::Resume),
            Self::Shutdown => None,
        }
    }
}

/// A bounded, coalescing producer handle. Native callbacks, the event tail,
/// resume, and zone-change signals can all wake the same worker without ever
/// queueing unbounded work or touching the winit loop.
pub struct ReminderWorker {
    signal: Arc<Signal>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ReminderActivationIngress {
    signal: Arc<Signal>,
}

/// **Nothing raises these on Linux yet.** Windows hears `WM_TIMECHANGE` and
/// resume in the Agent's message hook and macOS from `NSWorkspace`; Linux has
/// no equivalent wired (logind's `PrepareForSleep` would be it), so a reminder
/// there notices a zone change or a resume only at its next scheduled check.
#[derive(Clone)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) struct ReminderWakeIngress {
    signal: Arc<Signal>,
}

#[derive(Clone)]
pub(crate) struct ReminderControlIngress {
    signal: Arc<Signal>,
}

impl ReminderControlIngress {
    pub(crate) fn notification_control(
        &self,
        request: NotificationControl,
    ) -> Result<NotificationControlReply, NotificationControlQueueFull> {
        let (reply, receive) = std::sync::mpsc::sync_channel(1);
        self.signal
            .enqueue_control(ControlEnvelope { request, reply })?;
        Ok(NotificationControlReply(receive))
    }
}

impl ReminderWakeIngress {
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(crate) fn wake(&self, command: Command) {
        self.signal.raise(command);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivationIngressError {
    Invalid,
    Busy,
}

impl ReminderActivationIngress {
    pub(crate) fn activate(
        &self,
        activation: koru_native_notifications::Activation,
    ) -> Result<(), koru_native_notifications::Activation> {
        self.signal.enqueue_activation(activation)
    }

    pub(crate) fn accept_opaque(&self, envelope: &str) -> Result<(), ActivationIngressError> {
        let activation = koru_native_notifications::Activation::decode_opaque(envelope)
            .map_err(|_| ActivationIngressError::Invalid)?;
        let registration = activation.registration.as_str().to_owned();
        let action = activation.action;
        match self.activate(activation) {
            Ok(()) => {
                tracing::info!(
                    target: "koru::notification_activation",
                    registration,
                    action = ?action,
                    "Agent accepted notification action into the reminder queue"
                );
                Ok(())
            }
            Err(_) => {
                tracing::warn!(
                    target: "koru::notification_activation",
                    registration,
                    action = ?action,
                    "Agent notification action queue is full"
                );
                Err(ActivationIngressError::Busy)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            signal: Arc::new(Signal::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn take_one_for_test(&self) -> Option<koru_native_notifications::Activation> {
        match self.signal.wait(Some(Duration::ZERO)) {
            Some(Work::Activation(activation)) => Some(activation),
            _ => None,
        }
    }
}

impl ReminderWorker {
    pub fn start<S, N, L, C, Z, E>(
        mut scheduler: Scheduler<S, N, L, C, Z, E>,
        waker: crate::event::Waker,
    ) -> Self
    where
        S: ReminderStore + 'static,
        N: NativeNotifications + 'static,
        L: Ledger + 'static,
        C: Clock + 'static,
        Z: ZoneProvider + 'static,
        E: EventCursor + 'static,
    {
        let signal = Arc::new(Signal::default());
        let worker_signal = Arc::clone(&signal);
        let join = std::thread::Builder::new()
            .name("koru reminder scheduler".into())
            .spawn(move || {
                run_loop(
                    &mut scheduler,
                    MemoryDeliveryPreferences::default(),
                    worker_signal,
                    waker,
                )
            })
            .ok();
        Self { signal, join }
    }

    pub fn wake(&self, command: Command) {
        self.signal.raise(command);
    }

    /// Queue an OS activation without coalescing it. A full queue returns the
    /// exact payload to the native ingress so it can remain pending there;
    /// user gestures are never silently discarded.
    pub fn activate(
        &self,
        activation: koru_native_notifications::Activation,
    ) -> Result<(), koru_native_notifications::Activation> {
        self.signal.enqueue_activation(activation)
    }

    pub(crate) fn activation_ingress(&self) -> ReminderActivationIngress {
        ReminderActivationIngress {
            signal: Arc::clone(&self.signal),
        }
    }

    pub(crate) fn wake_ingress(&self) -> ReminderWakeIngress {
        ReminderWakeIngress {
            signal: Arc::clone(&self.signal),
        }
    }

    /// Queue one correlated native control operation. Calling this is
    /// non-blocking; Piece 7 must wait for the reply off the winit thread.
    pub fn notification_control(
        &self,
        request: NotificationControl,
    ) -> Result<NotificationControlReply, NotificationControlQueueFull> {
        self.control_ingress().notification_control(request)
    }

    pub(crate) fn control_ingress(&self) -> ReminderControlIngress {
        ReminderControlIngress {
            signal: Arc::clone(&self.signal),
        }
    }

    /// Install the real producer topology before winit starts. Gateway/Store
    /// discovery, journal streaming, timezone resolution and reconciliation
    /// all stay on this thread. Piece 5 replaces only `NativeUnavailable` and
    /// `VolatileLedger` with platform and atomic-file adapters.
    pub(crate) fn start_runtime(
        paths: crate::paths::Paths,
        waker: crate::event::Waker,
        native_activation_ready: Arc<AtomicBool>,
    ) -> Self {
        let signal = Arc::new(Signal::default());
        let worker_signal = Arc::clone(&signal);
        let join = std::thread::Builder::new()
            .name("koru reminder scheduler".into())
            .spawn(move || bootstrap_runtime(paths, worker_signal, waker, native_activation_ready))
            .ok();
        Self { signal, join }
    }
}

impl Drop for ReminderWorker {
    fn drop(&mut self) {
        self.signal.raise(Command::Shutdown);
        // Joining is bounded by the worker's current Store/native call, whose
        // adapters already carry their own request bounds. The winit loop owns
        // this handle until shutdown; ordinary event dispatch never drops it.
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run_loop<S, N, L, C, Z, E, P>(
    scheduler: &mut Scheduler<S, N, L, C, Z, E>,
    mut preferences: P,
    signal: Arc<Signal>,
    waker: crate::event::Waker,
) where
    S: ReminderStore,
    N: NativeNotifications,
    L: Ledger,
    C: Clock,
    Z: ZoneProvider,
    E: EventCursor,
    P: DeliveryPreferences,
{
    let mut next = Trigger::Startup;
    let mut retry = RetryBackoff::default();
    let mut last_failure = None;
    let (mut enabled, mut preferences_healthy) = match preferences.load_enabled() {
        Ok(enabled) => (enabled, true),
        Err(failure) => {
            report_failure(&mut last_failure, failure.category, false, &waker);
            (false, false)
        }
    };
    let mut reconciled_by_control = false;
    // Open and Snooze are user gestures, not wake hints. Keep one transiently
    // blocked activation local to the worker until Store authority is readable
    // again; the bounded ingress queue retains any later gestures behind it.
    let mut deferred_activation: Option<koru_native_notifications::Activation> = None;
    loop {
        let reconciled_before_wait = if reconciled_by_control {
            reconciled_by_control = false;
            true
        } else {
            let mut run = if enabled {
                scheduler.run(next)
            } else {
                scheduler.disable_delivery()
            };
            if !preferences_healthy && let Ok(report) = &mut run {
                report.failure = Some(super::FailureCategory::Preferences);
                report.failed_count = 1;
            }
            match run {
                Ok(report) => {
                    retry.reset();
                    last_failure = report.failure;
                    waker.wake(crate::event::AgentEvent::ReminderWorkerChanged {
                        report: report.clone(),
                    });
                    true
                }
                Err(failure) => {
                    report_failure(&mut last_failure, failure.category, enabled, &waker);
                    false
                }
            }
        };

        // Duration is derived inside Scheduler from its injected clock. A fake
        // worker never consults the machine clock merely because it waits. A
        // failed ledger read is not an indefinite plan: report it and use the
        // same bounded transient retry as a reconciliation ledger failure.
        let wait = if enabled {
            wait_after_run(
                &mut retry,
                &mut last_failure,
                scheduler.wait_duration(),
                &waker,
            )
        } else {
            last_failure
                .filter(|category| automatic_retry(*category))
                .map(|_| retry.next())
        };
        let work = if signal.is_stopping() {
            signal.wait(Some(Duration::ZERO))
        } else if reconciled_before_wait {
            if let Some(activation) = deferred_activation.as_ref() {
                tracing::info!(
                    target: "koru::notification_activation",
                    registration = activation.registration.as_str(),
                    action = ?activation.action,
                    "authoritative reconciliation completed; retrying notification action"
                );
            }
            deferred_activation.take().map(Work::Activation)
        } else {
            None
        }
        .or_else(|| signal.wait(wait));
        match work {
            Some(Work::Command(Command::Shutdown)) => break,
            Some(Work::Command(command)) => next = command.trigger().unwrap_or(Trigger::Events),
            Some(Work::Activation(activation)) => {
                let started = std::time::Instant::now();
                tracing::info!(
                    target: "koru::notification_activation",
                    registration = activation.registration.as_str(),
                    action = ?activation.action,
                    "reminder worker began resolving notification action"
                );
                match scheduler.activate(activation.clone()) {
                    Ok(super::ActivationResult::Open(target)) => {
                        let entity = &target.entity;
                        tracing::info!(
                            target: "koru::notification_activation",
                            registration = activation.registration.as_str(),
                            action = ?activation.action,
                            elapsed_ms = started.elapsed().as_millis(),
                            profile = %target.profile.to_storage(),
                            workspace = %entity.workspace().to_storage(),
                            document = %entity.document().to_storage(),
                            node = %entity.node(),
                            "notification action resolved to an Open target"
                        );
                        waker.wake(crate::event::AgentEvent::ReminderOpen { target });
                    }
                    Ok(super::ActivationResult::OpenProfile(profile)) => {
                        tracing::info!(
                            target: "koru::notification_activation",
                            registration = activation.registration.as_str(),
                            action = ?activation.action,
                            elapsed_ms = started.elapsed().as_millis(),
                            profile = %profile.to_storage(),
                            "notification summary action resolved to a profile"
                        );
                        waker.wake(crate::event::AgentEvent::ReminderProfileOpen { profile });
                    }
                    Ok(super::ActivationResult::SnoozeManagement(target)) => {
                        let entity = &target.entity;
                        tracing::info!(
                            target: "koru::notification_activation",
                            registration = activation.registration.as_str(),
                            action = ?activation.action,
                            elapsed_ms = started.elapsed().as_millis(),
                            profile = %target.profile.to_storage(),
                            workspace = %entity.workspace().to_storage(),
                            document = %entity.document().to_storage(),
                            node = %entity.node(),
                            "notification action resolved to Snooze management"
                        );
                        waker.wake(crate::event::AgentEvent::ReminderSnooze { target });
                    }
                    Ok(super::ActivationResult::Handled(report)) => {
                        tracing::info!(
                            target: "koru::notification_activation",
                            registration = activation.registration.as_str(),
                            action = ?activation.action,
                            elapsed_ms = started.elapsed().as_millis(),
                            "notification action marked its occurrence handled"
                        );
                        waker.wake(crate::event::AgentEvent::ReminderWorkerChanged { report });
                    }
                    Err(failure) => {
                        let retry_activation = matches!(
                            activation.action,
                            koru_native_notifications::Action::Open
                                | koru_native_notifications::Action::Snooze
                        ) && automatic_retry(failure.category);
                        tracing::warn!(
                            target: "koru::notification_activation",
                            registration = activation.registration.as_str(),
                            action = ?activation.action,
                            elapsed_ms = started.elapsed().as_millis(),
                            category = failure.category.as_str(),
                            retained = retry_activation,
                            "notification action could not yet be resolved"
                        );
                        report_failure(&mut last_failure, failure.category, enabled, &waker);
                        if retry_activation {
                            deferred_activation = Some(activation);
                        }
                    }
                }
                next = if deferred_activation.is_some() {
                    Trigger::Reconnect
                } else {
                    Trigger::Events
                };
            }
            Some(Work::Control(control)) => {
                let response = match control.request {
                    NotificationControl::SetDeliveryEnabled { enabled: requested } => {
                        match preferences.set_enabled(requested) {
                            Ok(()) => {
                                enabled = requested;
                                preferences_healthy = true;
                                // A changed user choice is itself a health
                                // transition even when repair meets the same
                                // failure category as the previous state.
                                last_failure = None;
                                let result = if enabled {
                                    scheduler.run(Trigger::NativeCapabilityChanged)
                                } else {
                                    scheduler.disable_delivery()
                                };
                                reconciled_by_control = true;
                                match result {
                                    Ok(report) => {
                                        retry.reset();
                                        last_failure = report.failure;
                                        waker.wake(
                                            crate::event::AgentEvent::ReminderWorkerChanged {
                                                report,
                                            },
                                        );
                                        NotificationControlResponse::Done
                                    }
                                    Err(failure) => {
                                        report_failure(
                                            &mut last_failure,
                                            failure.category,
                                            enabled,
                                            &waker,
                                        );
                                        NotificationControlResponse::Failed(scheduler_failure_code(
                                            failure.category,
                                        ))
                                    }
                                }
                            }
                            Err(failure) => {
                                report_failure(
                                    &mut last_failure,
                                    failure.category,
                                    enabled,
                                    &waker,
                                );
                                NotificationControlResponse::Failed(FailureCode::Platform)
                            }
                        }
                    }
                    NotificationControl::Repair if enabled => {
                        reconciled_by_control = true;
                        match scheduler.run(Trigger::NativeCapabilityChanged) {
                            Ok(report) => {
                                retry.reset();
                                last_failure = report.failure;
                                waker.wake(crate::event::AgentEvent::ReminderWorkerChanged {
                                    report,
                                });
                                NotificationControlResponse::Done
                            }
                            Err(failure) => {
                                report_failure(
                                    &mut last_failure,
                                    failure.category,
                                    enabled,
                                    &waker,
                                );
                                NotificationControlResponse::Failed(scheduler_failure_code(
                                    failure.category,
                                ))
                            }
                        }
                    }
                    NotificationControl::Repair => {
                        reconciled_by_control = true;
                        NotificationControlResponse::Done
                    }
                    request => scheduler.notification_control(request),
                };
                let repair = matches!(
                    response,
                    NotificationControlResponse::Capability(Capability {
                        authorization: Authorization::Allowed,
                        ..
                    }) | NotificationControlResponse::Authorization(Authorization::Allowed)
                );
                let _ = control.reply.send(response);
                // Inspect is the explicit refresh after returning from OS
                // Settings, while authorization is the only control allowed
                // to prompt. An Allowed result is therefore a strong repair
                // wake: a prior permission rejection has no ledger row or
                // deadline, and an empty durable event tail cannot select the
                // affected profiles for reconciliation.
                next = if repair {
                    Trigger::NativeCapabilityChanged
                } else {
                    Trigger::Events
                };
            }
            None if last_failure.is_some() => next = Trigger::Reconnect,
            None => next = Trigger::Deadline,
        }
    }
}

fn wait_after_run(
    retry: &mut RetryBackoff,
    last_failure: &mut Option<super::FailureCategory>,
    scheduled_wait: Result<Option<Duration>, SchedulerFailure>,
    waker: &crate::event::Waker,
) -> Option<Duration> {
    let scheduled_wait = match scheduled_wait {
        Ok(wait) => wait,
        Err(failure) => {
            report_failure(last_failure, failure.category, true, waker);
            None
        }
    };
    let retry_wait = last_failure
        .filter(|category| automatic_retry(*category))
        .map(|_| retry.next());
    match (retry_wait, scheduled_wait) {
        (Some(retry), Some(scheduled)) => Some(retry.min(scheduled)),
        (Some(retry), None) => Some(retry),
        (None, scheduled) => scheduled,
    }
}

fn report_failure(
    last_failure: &mut Option<super::FailureCategory>,
    category: super::FailureCategory,
    delivery_enabled: bool,
    waker: &crate::event::Waker,
) {
    if *last_failure == Some(category) {
        return;
    }
    waker.wake(crate::event::AgentEvent::ReminderWorkerChanged {
        report: SchedulerReport {
            delivery_enabled,
            failure: Some(category),
            ..SchedulerReport::default()
        },
    });
    *last_failure = Some(category);
}

fn scheduler_failure_code(category: super::FailureCategory) -> FailureCode {
    match category {
        super::FailureCategory::NativeUnsupported => FailureCode::Unsupported,
        super::FailureCategory::NativePermission => FailureCode::PermissionDenied,
        super::FailureCategory::NativeAsset => FailureCode::AssetMissing,
        super::FailureCategory::NativeInvalid => FailureCode::InvalidRequest,
        _ => FailureCode::Platform,
    }
}

fn automatic_retry(category: super::FailureCategory) -> bool {
    matches!(
        category,
        super::FailureCategory::Store
            | super::FailureCategory::Events
            | super::FailureCategory::NativePlatform
            | super::FailureCategory::Ledger
            | super::FailureCategory::ProjectionUpdating
    )
}

fn bootstrap_runtime(
    paths: crate::paths::Paths,
    signal: Arc<Signal>,
    waker: crate::event::Waker,
    native_activation_ready: Arc<AtomicBool>,
) {
    let ledger_path = paths.reminder_ledger_file();
    let preferences_path = paths.reminder_preferences_file();
    let mut retry = RetryBackoff::default();
    let mut reported = false;
    let client = loop {
        match crate::service::store_client(&paths, Some(&waker)) {
            Ok(client) => break client,
            Err(_) => {
                if !reported {
                    waker.wake(crate::event::AgentEvent::ReminderWorkerChanged {
                        report: SchedulerReport {
                            failure: Some(super::FailureCategory::Store),
                            ..SchedulerReport::default()
                        },
                    });
                    reported = true;
                }
                if signal.wait_for_bootstrap(retry.next()) {
                    return;
                }
                continue;
            }
        }
    };

    let event_client = client.clone();
    let event_signal = Arc::clone(&signal);
    let event_waker = waker.clone();
    let event_join = std::thread::Builder::new()
        .name("koru reminder event wake".into())
        .spawn(move || event_wake_loop(event_client, event_signal, event_waker))
        .ok();
    let mut scheduler = Scheduler::new(
        StoreClientPort::new(client.clone()),
        crate::notification_activation::platform_notifications(
            &paths,
            notification_explicit_root(&paths),
            native_activation_ready,
        ),
        FileLedger::new(ledger_path),
        SystemClock,
        SystemZoneProvider,
        EventsClientCursor::new(client),
    )
    .with_action_labels(native_action_labels());
    run_loop(
        &mut scheduler,
        FileDeliveryPreferences::new(preferences_path),
        Arc::clone(&signal),
        waker,
    );
    if let Some(join) = event_join {
        let _ = join.join();
    }
}

fn native_action_labels() -> koru_native_notifications::ActionLabels {
    let language =
        koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref());
    koru_native_notifications::ActionLabels {
        open: koru_i18n::t(language, koru_i18n::Key::ReminderNotificationOpen).into(),
        snooze: koru_i18n::t(language, koru_i18n::Key::ReminderSnooze).into(),
        handled: koru_i18n::t(language, koru_i18n::Key::ReminderNotificationHandled).into(),
    }
}

/// Only a root named on the command line may cross a stopped-activation
/// boundary. The channel's resolved storage root is not an explicit root and
/// relaunching it with `--root` would create a differently owned Agent.
fn notification_explicit_root(paths: &crate::paths::Paths) -> Option<std::path::PathBuf> {
    paths.explicit_root().map(std::path::Path::to_path_buf)
}

fn event_wake_loop(
    client: koru_api_client::Client,
    signal: Arc<Signal>,
    waker: crate::event::Waker,
) {
    let wait_signal = Arc::clone(&signal);
    let mut source = ApiEventWake { client };
    drive_event_wake(
        &mut source,
        signal,
        move |timeout| {
            wait_signal.wait_until_stopping(timeout);
            wait_signal.is_stopping()
        },
        move || {
            waker.wake(crate::event::AgentEvent::ReminderWorkerChanged {
                report: SchedulerReport {
                    failure: Some(super::FailureCategory::Events),
                    ..SchedulerReport::default()
                },
            });
        },
    );
}

trait LiveEventWake {
    type Subscription;

    fn subscribe(&mut self, signal: Arc<Signal>) -> Result<Self::Subscription, ()>;
}

struct ApiEventWake {
    client: koru_api_client::Client,
}

impl LiveEventWake for ApiEventWake {
    type Subscription = koru_api_client::events::Subscription;

    fn subscribe(&mut self, signal: Arc<Signal>) -> Result<Self::Subscription, ()> {
        self.client
            .events()
            .on(None, koru_api_client::Filter::all(), move |stored| {
                wake_for_live_event(&signal, &stored.envelope)
            })
            .map_err(|_| ())
    }
}

fn wake_for_live_event(signal: &Signal, event: &koru_events::Envelope) {
    if super::event_wake(event) != super::EventWake::Irrelevant {
        signal.raise(Command::Events);
    }
}

/// Install the live hint source, retrying only installation failures. `wait`
/// is a port so the recovery path is proven without sleeping; in production it
/// is the same shutdown-aware condition variable used by the scheduler.
fn drive_event_wake<S>(
    source: &mut S,
    signal: Arc<Signal>,
    mut wait: impl FnMut(Option<Duration>) -> bool,
    mut report_failure: impl FnMut(),
) where
    S: LiveEventWake,
{
    let mut retry = RetryBackoff::default();
    let mut reported = false;
    while !signal.is_stopping() {
        match source.subscribe(Arc::clone(&signal)) {
            Ok(_subscription) => {
                // Close the startup head/tail race only after the tail is
                // active. If `on(None)` sampled beyond an event that raced
                // with the scheduler's head read, this full repair observes
                // current Store authority; callbacks remain ordinary Events
                // hints and stay queued behind this stronger wake.
                signal.raise(Command::Reconnect);
                // The subscription repairs stream reconnects internally. No
                // timer runs while it is healthy; shutdown drops it.
                wait(None);
                return;
            }
            Err(_) => {
                if !reported {
                    report_failure();
                    reported = true;
                }
                if wait(Some(retry.next())) {
                    return;
                }
            }
        }
    }
}

#[derive(Default)]
struct Pending {
    events: bool,
    reconnect: bool,
    zone: bool,
    resume: bool,
    shutdown: bool,
    activations: VecDeque<koru_native_notifications::Activation>,
    controls: VecDeque<ControlEnvelope>,
}

/// Lossless coalescing: an arbitrary event storm is five booleans, and a
/// stronger wake cannot sit behind or be discarded by an ordinary event.
#[derive(Default)]
struct Signal {
    pending: Mutex<Pending>,
    /// Wakes the scheduler for commands, notification actions, and controls.
    /// This cannot be shared with the event-tail shutdown wait: `notify_one`
    /// on a shared condition variable can wake that non-consumer and leave a
    /// user gesture asleep until the next reminder deadline.
    work_changed: Condvar,
    stopping_changed: Condvar,
    stopping: AtomicBool,
}

impl Signal {
    fn raise(&self, command: Command) {
        if command == Command::Shutdown {
            self.stopping.store(true, Ordering::Release);
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match command {
            Command::Events => pending.events = true,
            Command::Reconnect => pending.reconnect = true,
            Command::DeviceZoneChanged => pending.zone = true,
            Command::Resume => pending.resume = true,
            Command::Shutdown => pending.shutdown = true,
        }
        if command == Command::Shutdown {
            // The scheduler and the live event tail can both be asleep on this
            // signal. Each has its own condition so ordinary work can never be
            // consumed by the shutdown-only waiter.
            self.work_changed.notify_all();
            self.stopping_changed.notify_all();
        } else {
            self.work_changed.notify_one();
        }
    }

    fn enqueue_activation(
        &self,
        activation: koru_native_notifications::Activation,
    ) -> Result<(), koru_native_notifications::Activation> {
        const MAX_PENDING_ACTIVATIONS: usize = 64;
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.activations.len() >= MAX_PENDING_ACTIVATIONS {
            return Err(activation);
        }
        pending.activations.push_back(activation);
        self.work_changed.notify_one();
        Ok(())
    }

    fn enqueue_control(
        &self,
        control: ControlEnvelope,
    ) -> Result<(), NotificationControlQueueFull> {
        const MAX_PENDING_CONTROLS: usize = 16;
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.controls.len() >= MAX_PENDING_CONTROLS {
            return Err(NotificationControlQueueFull);
        }
        pending.controls.push_back(control);
        self.work_changed.notify_one();
        Ok(())
    }

    fn wait(&self, timeout: Option<Duration>) -> Option<Work> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !has_pending(&pending) {
            pending = match timeout {
                Some(timeout) => {
                    self.work_changed
                        .wait_timeout_while(pending, timeout, |pending| !has_pending(pending))
                        .unwrap_or_else(|error| error.into_inner())
                        .0
                }
                None => self
                    .work_changed
                    .wait_while(pending, |pending| !has_pending(pending))
                    .unwrap_or_else(|error| error.into_inner()),
            };
        }
        take_strongest(&mut pending)
    }

    /// Initial Store discovery may be retrying while OS activations arrive.
    /// Leave those payloads queued and wait only for the retry boundary or a
    /// lossless shutdown; the first connected worker drains them in order.
    fn wait_for_bootstrap(&self, timeout: Duration) -> bool {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut pending = self
            .stopping_changed
            .wait_timeout_while(pending, timeout, |pending| !pending.shutdown)
            .unwrap_or_else(|error| error.into_inner())
            .0;
        std::mem::take(&mut pending.shutdown)
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    fn wait_until_stopping(&self, timeout: Option<Duration>) {
        if self.is_stopping() {
            return;
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match timeout {
            Some(timeout) => drop(
                self.stopping_changed
                    .wait_timeout_while(pending, timeout, |_| !self.is_stopping())
                    .unwrap_or_else(|error| error.into_inner()),
            ),
            None => drop(
                self.stopping_changed
                    .wait_while(pending, |_| !self.is_stopping())
                    .unwrap_or_else(|error| error.into_inner()),
            ),
        }
    }
}

fn has_pending(pending: &Pending) -> bool {
    pending.shutdown
        || pending.reconnect
        || pending.zone
        || pending.resume
        || !pending.activations.is_empty()
        || !pending.controls.is_empty()
        || pending.events
}

enum Work {
    Command(Command),
    Activation(koru_native_notifications::Activation),
    Control(ControlEnvelope),
}

fn take_strongest(pending: &mut Pending) -> Option<Work> {
    if std::mem::take(&mut pending.shutdown) {
        Some(Work::Command(Command::Shutdown))
    } else if std::mem::take(&mut pending.reconnect) {
        Some(Work::Command(Command::Reconnect))
    } else if std::mem::take(&mut pending.zone) {
        Some(Work::Command(Command::DeviceZoneChanged))
    } else if std::mem::take(&mut pending.resume) {
        Some(Work::Command(Command::Resume))
    } else if let Some(activation) = pending.activations.pop_front() {
        Some(Work::Activation(activation))
    } else if let Some(control) = pending.controls.pop_front() {
        Some(Work::Control(control))
    } else if std::mem::take(&mut pending.events) {
        Some(Work::Command(Command::Events))
    } else {
        None
    }
}

#[derive(Default)]
struct RetryBackoff {
    attempt: u8,
}

impl RetryBackoff {
    fn next(&mut self) -> Duration {
        let seconds = 1u64 << self.attempt.min(3);
        self.attempt = self.attempt.saturating_add(1);
        Duration::from_secs(seconds)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use koru_native_notifications::RegistrationId;

    fn activation(suffix: usize) -> koru_native_notifications::Activation {
        koru_native_notifications::Activation::new(
            RegistrationId::new(format!("{suffix:064x}")).unwrap(),
            koru_native_notifications::Action::Open,
        )
    }

    #[test]
    fn retry_is_bounded_and_success_resets_it() {
        let mut retry = RetryBackoff::default();
        assert_eq!(retry.next(), Duration::from_secs(1));
        assert_eq!(retry.next(), Duration::from_secs(2));
        assert_eq!(retry.next(), Duration::from_secs(4));
        assert_eq!(retry.next(), Duration::from_secs(8));
        assert_eq!(retry.next(), Duration::from_secs(8));
        retry.reset();
        assert_eq!(retry.next(), Duration::from_secs(1));
    }

    #[test]
    fn native_routes_keep_default_and_explicit_agent_roots_distinct() {
        let channel = crate::paths::Paths::for_channel("/channel/root".into());
        let explicit = crate::paths::Paths::for_root("/test/root".into());

        let channel_root = notification_explicit_root(&channel);
        let explicit_root = notification_explicit_root(&explicit);
        assert_eq!(channel_root, None);
        assert_eq!(explicit_root.as_deref(), Some(explicit.root()));
        assert_eq!(
            koru_native_notifications::MacRootRoute::from_explicit_root(channel_root.as_deref())
                .unwrap(),
            koru_native_notifications::MacRootRoute::Default
        );
        assert_eq!(
            koru_native_notifications::MacRootRoute::from_explicit_root(explicit_root.as_deref())
                .unwrap(),
            koru_native_notifications::MacRootRoute::Root("/test/root".into())
        );
    }

    #[test]
    fn only_transient_failures_arm_automatic_retry() {
        use super::super::FailureCategory;

        for category in [
            FailureCategory::Store,
            FailureCategory::Events,
            FailureCategory::NativePlatform,
            FailureCategory::Ledger,
            FailureCategory::ProjectionUpdating,
        ] {
            assert!(automatic_retry(category), "{category:?}");
        }
        for category in [
            FailureCategory::NativeUnsupported,
            FailureCategory::NativePermission,
            FailureCategory::NativeAsset,
            FailureCategory::NativeInvalid,
            FailureCategory::TimeZone,
            FailureCategory::StaleActivation,
            FailureCategory::UnsupportedActivation,
        ] {
            assert!(!automatic_retry(category), "{category:?}");
        }
    }

    #[test]
    fn a_deadline_ledger_read_failure_is_reported_and_retried() {
        let (waker, seen) = crate::event::Waker::collecting();
        let mut retry = RetryBackoff::default();
        let mut last_failure = None;

        assert_eq!(
            wait_after_run(
                &mut retry,
                &mut last_failure,
                Err(SchedulerFailure::new(super::super::FailureCategory::Ledger)),
                &waker,
            ),
            Some(Duration::from_secs(1))
        );
        assert_eq!(last_failure, Some(super::super::FailureCategory::Ledger));
        assert_eq!(
            wait_after_run(
                &mut retry,
                &mut last_failure,
                Err(SchedulerFailure::new(super::super::FailureCategory::Ledger)),
                &waker,
            ),
            Some(Duration::from_secs(2))
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one diagnostic per failure category");
        assert!(matches!(
            &seen[0],
            crate::event::AgentEvent::ReminderWorkerChanged { report }
                if report.failure == Some(super::super::FailureCategory::Ledger)
        ));
    }

    #[test]
    fn coalescing_never_loses_shutdown_or_a_stronger_wake() {
        let signal = Signal::default();
        for _ in 0..1_000 {
            signal.raise(Command::Events);
        }
        signal.raise(Command::DeviceZoneChanged);
        assert!(matches!(
            signal.wait(Some(Duration::ZERO)),
            Some(Work::Command(Command::DeviceZoneChanged))
        ));
        assert!(matches!(
            signal.wait(Some(Duration::ZERO)),
            Some(Work::Command(Command::Events))
        ));
        signal.raise(Command::Events);
        signal.raise(Command::Shutdown);
        assert!(matches!(
            signal.wait(Some(Duration::ZERO)),
            Some(Work::Command(Command::Shutdown))
        ));
    }

    #[test]
    fn shutdown_releases_both_scheduler_and_live_tail_waiters() {
        let signal = Arc::new(Signal::default());
        let ready = Arc::new(std::sync::Barrier::new(3));
        let (tx, rx) = std::sync::mpsc::channel();

        let scheduler_signal = Arc::clone(&signal);
        let scheduler_ready = Arc::clone(&ready);
        let scheduler_tx = tx.clone();
        let scheduler = std::thread::spawn(move || {
            scheduler_ready.wait();
            let _ = scheduler_signal.wait(None);
            scheduler_tx.send("scheduler").unwrap();
        });
        let tail_signal = Arc::clone(&signal);
        let tail_ready = Arc::clone(&ready);
        let tail = std::thread::spawn(move || {
            tail_ready.wait();
            tail_signal.wait_until_stopping(None);
            tx.send("tail").unwrap();
        });
        ready.wait();
        signal.raise(Command::Shutdown);

        let mut released = [
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        ];
        released.sort_unstable();
        assert_eq!(released, ["scheduler", "tail"]);
        scheduler.join().unwrap();
        tail.join().unwrap();
    }

    #[test]
    fn notification_action_wakes_the_scheduler_while_the_live_tail_waits_for_shutdown() {
        let signal = Arc::new(Signal::default());
        let (tail_ready, tail_is_ready) = std::sync::mpsc::sync_channel(1);
        let tail_signal = Arc::clone(&signal);
        let tail = std::thread::spawn(move || {
            tail_ready.send(()).unwrap();
            tail_signal.wait_until_stopping(None);
        });
        tail_is_ready.recv().unwrap();
        // Give the shutdown-only waiter time to enter its condition wait first;
        // this is the ordering that consumed `notify_one` before the conditions
        // were separated.
        std::thread::sleep(Duration::from_millis(20));

        let (work_ready, work_is_ready) = std::sync::mpsc::sync_channel(1);
        let (worked, work_done) = std::sync::mpsc::sync_channel(1);
        let worker_signal = Arc::clone(&signal);
        let worker = std::thread::spawn(move || {
            work_ready.send(()).unwrap();
            worked
                .send(worker_signal.wait(Some(Duration::from_secs(2))))
                .unwrap();
        });
        work_is_ready.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));

        signal.enqueue_activation(activation(7)).unwrap();
        assert!(matches!(
            work_done.recv_timeout(Duration::from_millis(250)).unwrap(),
            Some(Work::Activation(actual)) if actual == activation(7)
        ));

        signal.raise(Command::Shutdown);
        worker.join().unwrap();
        tail.join().unwrap();
    }

    #[test]
    fn activation_queue_is_fifo_and_returns_an_overflow_payload() {
        let signal = Signal::default();
        for suffix in 0..64 {
            signal.enqueue_activation(activation(suffix)).unwrap();
        }
        assert_eq!(
            signal.enqueue_activation(activation(64)).unwrap_err(),
            activation(64)
        );
        for suffix in 0..64 {
            let Some(Work::Activation(actual)) = signal.wait(Some(Duration::ZERO)) else {
                panic!("queued activation missing")
            };
            assert_eq!(actual, activation(suffix));
        }
    }

    #[test]
    fn opaque_ingress_refuses_invalid_and_reports_a_full_fifo() {
        let signal = Arc::new(Signal::default());
        let ingress = ReminderActivationIngress {
            signal: Arc::clone(&signal),
        };
        assert_eq!(
            ingress.accept_opaque("not-an-envelope"),
            Err(ActivationIngressError::Invalid)
        );
        for suffix in 0..64 {
            ingress
                .accept_opaque(&activation(suffix).encode_opaque())
                .unwrap();
        }
        assert_eq!(
            ingress.accept_opaque(&activation(64).encode_opaque()),
            Err(ActivationIngressError::Busy)
        );
        let Some(Work::Activation(first)) = signal.wait(Some(Duration::ZERO)) else {
            panic!("queued activation missing")
        };
        assert_eq!(first, activation(0));
        assert_eq!(
            ingress.accept_opaque(&activation(64).encode_opaque()),
            Ok(())
        );
    }

    #[test]
    fn control_queue_is_bounded_and_recovers_after_one_is_taken() {
        let signal = Signal::default();
        for _ in 0..16 {
            let (reply, _) = std::sync::mpsc::sync_channel(1);
            signal
                .enqueue_control(ControlEnvelope {
                    request: NotificationControl::Inspect,
                    reply,
                })
                .unwrap();
        }
        let (reply, _) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            signal.enqueue_control(ControlEnvelope {
                request: NotificationControl::Inspect,
                reply,
            }),
            Err(NotificationControlQueueFull)
        );
        assert!(matches!(
            signal.wait(Some(Duration::ZERO)),
            Some(Work::Control(_))
        ));
        let (reply, _) = std::sync::mpsc::sync_channel(1);
        assert!(
            signal
                .enqueue_control(ControlEnvelope {
                    request: NotificationControl::Inspect,
                    reply,
                })
                .is_ok()
        );
    }

    #[test]
    fn irrelevant_live_events_do_not_wake_the_scheduler() {
        use koru_events::EventPayload as _;

        fn envelope(name: &'static str) -> koru_events::Envelope {
            koru_events::Draft::new(koru_events::EventName::from_static(name)).build(
                &koru_events::EmitContext::system(stringify!(reminder_test)),
                koru_events::EventId::mint(),
                0,
            )
        }

        let signal = Signal::default();
        wake_for_live_event(&signal, &envelope(koru_events::SearchOpened::NAME));
        assert!(signal.wait(Some(Duration::ZERO)).is_none());

        wake_for_live_event(
            &signal,
            &envelope(koru_events::catalog::ReminderRuleChanged::NAME),
        );
        assert!(matches!(
            signal.wait(Some(Duration::ZERO)),
            Some(Work::Command(Command::Events))
        ));
    }

    #[test]
    fn initial_event_tail_failure_retries_without_polling_after_success() {
        struct FakeSource {
            failures_left: usize,
            attempts: usize,
            active: Arc<AtomicBool>,
        }

        impl LiveEventWake for FakeSource {
            type Subscription = ();

            fn subscribe(&mut self, signal: Arc<Signal>) -> Result<Self::Subscription, ()> {
                self.attempts += 1;
                if self.failures_left > 0 {
                    self.failures_left -= 1;
                    Err(())
                } else {
                    self.active.store(true, Ordering::Release);
                    // Simulate an ordinary callback racing with installation.
                    signal.raise(Command::Events);
                    Ok(())
                }
            }
        }

        let mut source = FakeSource {
            failures_left: 2,
            attempts: 0,
            active: Arc::new(AtomicBool::new(false)),
        };
        let active = Arc::clone(&source.active);
        let mut waits = Vec::new();
        let mut reports = 0;
        let signal = Arc::new(Signal::default());
        let repaired_after_install = Arc::new(AtomicBool::new(false));
        let wait_signal = Arc::clone(&signal);
        let wait_repaired = Arc::clone(&repaired_after_install);
        drive_event_wake(
            &mut source,
            signal,
            |wait| {
                waits.push(wait);
                if wait.is_none() {
                    assert!(active.load(Ordering::Acquire));
                    assert!(matches!(
                        wait_signal.wait(Some(Duration::ZERO)),
                        Some(Work::Command(Command::Reconnect))
                    ));
                    assert!(matches!(
                        wait_signal.wait(Some(Duration::ZERO)),
                        Some(Work::Command(Command::Events))
                    ));
                    wait_repaired.store(true, Ordering::Release);
                }
                false
            },
            || reports += 1,
        );
        assert_eq!(source.attempts, 3);
        assert_eq!(
            waits,
            vec![
                Some(Duration::from_secs(1)),
                Some(Duration::from_secs(2)),
                None
            ]
        );
        assert_eq!(reports, 1, "one failure category, not one per retry");
        assert!(repaired_after_install.load(Ordering::Acquire));
    }
}
