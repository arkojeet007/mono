// SPDX-License-Identifier: MPL-2.0

//! Cached admission and projection around the single set scheduler.
use super::scheduler::{Scheduler, SelectionReply};
use super::*;

type CancelReply = mpsc::SyncSender<Result<(), &'static str>>;
type CancelAnswer = mpsc::Receiver<Result<(), &'static str>>;

enum Command<C> {
    Activate(Option<C>),
    Select(BTreeSet<ProfileId>, C, SelectionReply),
    Refresh(C, C),
    Cancel(C, CancelReply),
    Retry(C, Option<ProfileId>),
}

struct Desired<C> {
    commands: VecDeque<Command<C>>,
    stop: bool,
}

impl<C> Default for Desired<C> {
    fn default() -> Self {
        Self {
            commands: VecDeque::new(),
            stop: false,
        }
    }
}

struct Visible<P: Port> {
    credential: Option<P::Credential>,
    owner_scope: Option<u64>,
    cached: BTreeMap<P::Scope, (P::Credential, Snapshot)>,
    snapshot: Snapshot,
}

pub struct Control<P: Port> {
    wake: mpsc::SyncSender<()>,
    desired: Arc<Mutex<Desired<P::Credential>>>,
    visible: Arc<Mutex<Visible<P>>>,
    foreground: Arc<AtomicBool>,
}

impl<P: Port> Control<P> {
    pub fn start(port: P, notify: impl Fn(Report) + Send + 'static) -> Self {
        Self::start_with_clock(port, Arc::new(SystemClock), notify)
    }

    pub fn start_with_clock(
        port: P,
        clock: Arc<dyn Clock>,
        notify: impl Fn(Report) + Send + 'static,
    ) -> Self {
        Self::start_with_clock_and_foreground(port, clock, true, notify)
    }

    pub fn start_with_clock_and_foreground(
        port: P,
        clock: Arc<dyn Clock>,
        foreground: bool,
        notify: impl Fn(Report) + Send + 'static,
    ) -> Self {
        Self::start_with_clock_and_spawn(port, notify, clock, foreground, |worker| {
            std::thread::Builder::new()
                .name("koru-enrollment".into())
                .spawn(worker)
                .map(|_| ())
        })
    }

    #[cfg(test)]
    pub(super) fn start_with(
        port: P,
        notify: impl Fn(Report) + Send + 'static,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> Self {
        Self::start_with_clock_and_spawn(port, notify, Arc::new(SystemClock), true, spawn)
    }

    pub(super) fn start_with_clock_and_spawn(
        port: P,
        notify: impl Fn(Report) + Send + 'static,
        clock: Arc<dyn Clock>,
        initial_foreground: bool,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> Self {
        let (wake, receiver) = mpsc::sync_channel(WAKE_CAPACITY);
        let desired = Arc::new(Mutex::new(Desired::<P::Credential>::default()));
        let visible = Arc::new(Mutex::new(Visible::<P> {
            credential: None,
            owner_scope: None,
            cached: BTreeMap::new(),
            snapshot: Snapshot::default(),
        }));
        let foreground = Arc::new(AtomicBool::new(initial_foreground));
        let worker_desired = desired.clone();
        let worker_visible = visible.clone();
        let worker_foreground = foreground.clone();
        let worker_wake = wake.clone();
        let result = spawn(Box::new(move || {
            let mut scheduler = match Scheduler::new(port, worker_wake) {
                Ok(scheduler) => scheduler,
                Err(error) => {
                    let report = failed(failure_code(error));
                    worker_visible
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .snapshot
                        .report = report.clone();
                    notify(report);
                    return;
                }
            };
            let mut handed_off = 0;
            // A durable cancel is acknowledged only once the snapshot callers
            // read shows it. The scheduler answers into a relay as soon as the
            // target is cleared — or later, when it had to load the account
            // first — and the caller's reply is sent after the publish below,
            // so `cancel_durable` returning `Ok` and `snapshot()` still naming
            // the cancelled target can no longer both be true.
            let mut cancels: Vec<(CancelAnswer, CancelReply)> = Vec::new();
            loop {
                let next = {
                    let mut desired = worker_desired
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    std::mem::take(&mut *desired)
                };
                if next.stop {
                    break;
                }
                scheduler.drain(clock.as_ref());
                for command in next.commands {
                    match command {
                        Command::Activate(credential) => scheduler.activate(credential),
                        Command::Select(profiles, credential, reply) => {
                            scheduler.select(profiles, credential, reply)
                        }
                        Command::Refresh(previous, next) => scheduler.refresh(previous, next),
                        Command::Retry(credential, profile) => {
                            scheduler.retry_for(&credential, profile)
                        }
                        Command::Cancel(credential, reply) => {
                            let (relay, answered) = mpsc::sync_channel(1);
                            scheduler.cancel_requested(credential, relay);
                            cancels.push((answered, reply));
                        }
                    }
                }
                let foreground = worker_foreground.load(Ordering::Acquire);
                if !foreground {
                    scheduler.interrupt();
                }
                scheduler.dispatch(clock.as_ref(), foreground);
                let revision = scheduler.take_revision();
                let report = {
                    let mut visible = worker_visible
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    for (credential, snapshot) in scheduler.snapshots() {
                        visible
                            .cached
                            .insert(P::scope(&credential), (credential, snapshot));
                    }
                    let mut snapshot = visible
                        .credential
                        .as_ref()
                        .and_then(|credential| {
                            visible
                                .cached
                                .get(&P::scope(credential))
                                .filter(|(previous, _)| P::same_authority(previous, credential))
                                .map(|(_, snapshot)| snapshot.clone())
                        })
                        .unwrap_or_default();
                    snapshot.owner_scope = visible.owner_scope;
                    visible.snapshot = snapshot;
                    visible.snapshot.report.clone()
                };
                // Disconnected means the scheduler dropped the answer; dropping
                // the reply too gives the caller the same unavailable error.
                cancels.retain(|(answered, reply)| match answered.try_recv() {
                    Ok(result) => {
                        let _ = reply.try_send(result);
                        false
                    }
                    Err(mpsc::TryRecvError::Empty) => true,
                    Err(mpsc::TryRecvError::Disconnected) => false,
                });
                let mut report = report;
                report.durable_revision = revision.filter(|revision| *revision > handed_off);
                if let Some(revision) = report.durable_revision {
                    handed_off = revision;
                }
                // Notification is never invoked under the cached-state lock.
                notify(report);
                let wait = scheduler
                    .deadline(foreground)
                    .map(|at| at.saturating_duration_since(clock.monotonic()));
                match wait {
                    Some(wait) => match receiver.recv_timeout(wait) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => {
                        if receiver.recv().is_err() {
                            break;
                        }
                    }
                }
            }
        }));
        if result.is_err() {
            visible
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .snapshot
                .report = failed(ENROLLMENT_UNAVAILABLE);
        }
        Self {
            wake,
            desired,
            visible,
            foreground,
        }
    }

    fn enqueue(&self, command: Command<P::Credential>) -> bool {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if desired.stop || desired.commands.len() >= 32 {
            return false;
        }
        desired.commands.push_back(command);
        drop(desired);
        self.signal()
    }

    pub fn select_profiles_durable(
        &self,
        profiles: BTreeSet<ProfileId>,
        credential: P::Credential,
    ) -> Result<Snapshot, &'static str> {
        let (reply, result) = mpsc::sync_channel(1);
        if !self.enqueue(Command::Select(profiles, credential, reply)) {
            return Err(ENROLLMENT_UNAVAILABLE);
        }
        // Only the account execution lane waits. Effects have their own bounded
        // deadlines; returning early would falsely refuse a still-owned CAS.
        result.recv().map_err(|_| ENROLLMENT_UNAVAILABLE)?
    }

    #[cfg(test)]
    pub fn select(&self, profile: ProfileId, credential: P::Credential) -> bool {
        self.activate(Some(credential.clone()));
        let (reply, _) = mpsc::sync_channel(1);
        self.enqueue(Command::Select(
            BTreeSet::from([profile]),
            credential,
            reply,
        ))
    }

    #[cfg(test)]
    pub fn select_parked(&self, profile: ProfileId, credential: P::Credential) -> bool {
        let (reply, _) = mpsc::sync_channel(1);
        self.enqueue(Command::Select(
            BTreeSet::from([profile]),
            credential,
            reply,
        ))
    }

    pub fn activate_owned(&self, credential: Option<P::Credential>, owner_scope: u64) -> bool {
        self.activate_inner(credential, Some(owner_scope))
    }

    pub fn activate(&self, credential: Option<P::Credential>) -> bool {
        self.activate_inner(credential, None)
    }

    fn activate_inner(&self, credential: Option<P::Credential>, owner_scope: Option<u64>) -> bool {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        {
            let visible = self
                .visible
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if visible.owner_scope == owner_scope && same::<P>(&visible.credential, &credential) {
                return true;
            }
        }
        if desired.stop || desired.commands.len() >= 32 {
            return false;
        }
        self.show(credential.clone(), owner_scope);
        desired.commands.push_back(Command::Activate(credential));
        drop(desired);
        self.signal()
    }

    pub fn update_authorization(&self, credential: P::Credential) -> bool {
        self.activate(Some(credential))
    }

    pub fn refresh_authorization(&self, previous: P::Credential, next: P::Credential) -> bool {
        if !P::refresh_continuity(&previous, &next) {
            return false;
        }
        if !self.enqueue(Command::Refresh(previous.clone(), next.clone())) {
            return false;
        }
        let mut visible = self
            .visible
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if visible
            .credential
            .as_ref()
            .is_some_and(|current| P::same_authority(current, &previous))
        {
            // No old revision's cached report can appear under the new one.
            visible.credential = Some(next);
            visible.snapshot = Snapshot {
                owner_scope: visible.owner_scope,
                ..Snapshot::default()
            };
        }
        true
    }

    fn show(&self, credential: Option<P::Credential>, owner_scope: Option<u64>) {
        let mut visible = self
            .visible
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut snapshot = credential
            .as_ref()
            .and_then(|credential| {
                visible
                    .cached
                    .get(&P::scope(credential))
                    .filter(|(previous, _)| P::same_authority(previous, credential))
                    .map(|(_, snapshot)| snapshot.clone())
            })
            .unwrap_or_default();
        snapshot.owner_scope = owner_scope;
        visible.credential = credential;
        visible.owner_scope = owner_scope;
        visible.snapshot = snapshot;
    }

    pub fn retry_for(&self, credential: P::Credential, profile: Option<ProfileId>) -> bool {
        self.enqueue(Command::Retry(credential, profile))
    }
    pub fn retry_profile(&self, profile: Option<ProfileId>) -> bool {
        let credential = self
            .visible
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .credential
            .clone();
        credential.is_some_and(|credential| self.retry_for(credential, profile))
    }
    pub fn retry(&self) -> bool {
        self.retry_profile(None)
    }
    pub fn wake(&self) -> bool {
        self.signal()
    }
    pub fn set_foreground(&self, foreground: bool) {
        if self.foreground.swap(foreground, Ordering::AcqRel) != foreground {
            self.signal();
        }
    }

    pub fn cancel_durable(&self) -> Result<(), &'static str> {
        let credential = self
            .visible
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .credential
            .clone();
        if let Some(credential) = credential {
            self.cancel_durable_for(credential)
        } else {
            Ok(())
        }
    }

    pub fn cancel_durable_for(&self, credential: P::Credential) -> Result<(), &'static str> {
        let (reply, result) = mpsc::sync_channel(1);
        if !self.enqueue(Command::Cancel(credential, reply)) {
            return Err(ENROLLMENT_UNAVAILABLE);
        }
        result
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| ENROLLMENT_UNAVAILABLE)?
    }

    pub fn report(&self) -> Report {
        self.snapshot().report
    }
    pub fn snapshot(&self) -> Snapshot {
        self.visible
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .snapshot
            .clone()
    }
    fn signal(&self) -> bool {
        matches!(
            self.wake.try_send(()),
            Ok(()) | Err(mpsc::TrySendError::Full(()))
        )
    }
}

fn same<P: Port>(previous: &Option<P::Credential>, next: &Option<P::Credential>) -> bool {
    match (previous, next) {
        (Some(previous), Some(next)) => P::same_authority(previous, next),
        (None, None) => true,
        _ => false,
    }
}

impl<P: Port> Drop for Control<P> {
    fn drop(&mut self) {
        self.desired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stop = true;
        let _ = self.wake.try_send(());
    }
}

impl<P: Port<Credential = EnrollmentAuthorization>> Driver for Control<P> {
    fn select_profiles(
        &self,
        profiles: BTreeSet<ProfileId>,
        authority: EnrollmentAuthorization,
    ) -> Result<Snapshot, &'static str> {
        self.select_profiles_durable(profiles, authority)
    }
    fn retry(&self, authority: EnrollmentAuthorization, profile: Option<ProfileId>) -> bool {
        self.retry_for(authority, profile)
    }
    fn cancel_durable(&self) -> Result<(), &'static str> {
        Control::cancel_durable(self)
    }
    fn cancel_durable_for(&self, authority: EnrollmentAuthorization) -> Result<(), &'static str> {
        Control::cancel_durable_for(self, authority)
    }
    fn update_authorization(&self, authority: EnrollmentAuthorization) -> bool {
        Control::update_authorization(self, authority)
    }
    fn activate_owned(&self, authority: Option<EnrollmentAuthorization>, owner_scope: u64) -> bool {
        Control::activate_owned(self, authority, owner_scope)
    }
    fn refresh_authorization(
        &self,
        previous: EnrollmentAuthorization,
        next: EnrollmentAuthorization,
    ) -> bool {
        Control::refresh_authorization(self, previous, next)
    }
    fn set_foreground(&self, foreground: bool) {
        Control::set_foreground(self, foreground)
    }
    fn wake(&self) -> bool {
        Control::wake(self)
    }
    fn snapshot(&self) -> Snapshot {
        Control::snapshot(self)
    }
}
