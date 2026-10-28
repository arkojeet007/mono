// SPDX-License-Identifier: MPL-2.0

//! Starting the agent: bind, then serve, then hand the window loop over.
//!
//! ## The order is the readiness contract
//!
//! ```text
//! 1  bind the endpoint      nothing else may be the agent for this user+channel
//! 2  read the device        an identity to greet shells with
//! 3  start the accept loop  ...and only now is there something to talk to
//! 4  create the gateway supervision (it does not wait) and prove
//!    readiness on the window loop's worker
//! 5  acknowledge updater health, then consume the reopen record
//! ```
//!
//! A second agent fails at **1**, before it has read a file or spawned a thread.
//! A shell that connects between 1 and 3 waits at `accept` rather than being
//! told an endpoint that does not answer — the same property the store's
//! rendezvous has, and for the same reason: binding and accepting are two
//! moments and the gap between them is where a client is safe.
//!
//! **The updater reads that contract too.** When this launch is the restart that
//! follows an install, the token stays on [`Agent`] until the gateway worker has
//! proved the matching private bridge and required Store worker are ready. Only
//! then does the event loop acknowledge health and consume the reopen record.
//!
//! ## Why the loop is last and owns everything after it
//!
//! `run` never returns until the agent stops. Every producer started before it
//! holds a [`Waker`], which is the whole of what any of them needs — the winit
//! `EventLoopProxy` behind it is handed over in `attach`, before the first
//! event can be raised.

use std::sync::{Arc, Mutex};

use koru_device::TrustRepository;

use crate::event::{AgentEvent, Mailbox};
use crate::{Agent, Device, Paths, rendezvous};

/// What went wrong before there was a window to say it in.
#[derive(Debug)]
pub enum StartError {
    /// Another agent is already serving this user and channel. **Not a failure
    /// to the person**: they wanted an agent and there is one, so a second
    /// launch stands down quietly rather than complaining.
    AlreadyRunning,
    Failed(String),
}

/// A token-bearing restart under an explicit root belongs to that root's
/// updater transaction even after the rehearsal manifest has been consumed.
/// The successful target build is deliberately launched without the manifest,
/// so keying this choice on the manifest alone leaves it outside the lease and
/// health directories whose token it is meant to acknowledge.
fn rooted_update_paths(
    paths: &Paths,
    update_health_token: Option<&str>,
    rehearsal_manifest: Option<&std::path::Path>,
) -> Option<koru_update::UpdatePaths> {
    (paths.explicit_root().is_some()
        && (update_health_token.is_some() || rehearsal_manifest.is_some()))
    .then(|| koru_update::UpdatePaths::under(paths.root()))
}

/// Whether this launch deliberately participates in the machine-wide input
/// namespace. Ordinary Agents always do. An explicit root opts in only through
/// its interactive `--tray` launch; headless roots used by gates stay inert.
fn machine_input_enabled(paths: &Paths, show_tray: bool) -> bool {
    paths.owns_the_machine() || (paths.explicit_root().is_some() && show_tray)
}

/// Whether this Agent starts the Socials publisher: only while the build shows
/// Socials (`koru_features::socials`). The one background worker a product
/// flag gates — ADR-450's 2026-10-10 Socials amendment: nothing publishes
/// from a build that hides Socials, and a post already scheduled here waits,
/// untouched in its journal, until a build that shows Socials runs again.
fn should_start_socials_publisher(features: &koru_features::Features) -> bool {
    features.socials
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => f.write_str("an agent is already running for this channel"),
            Self::Failed(why) => write!(f, "the agent could not start: {why}"),
        }
    }
}

/// Start the agent and hand it the window loop.
///
/// `update_health_token` is `Some` only when this launch is the restart the
/// updater performed after installing a build — it is the token off the command
/// line, already validated by [`crate::cli`], and what it buys is written up at
/// the call below.
pub fn run(
    paths: Paths,
    show_tray: bool,
    // **Currently inert, and worth knowing rather than hiding.** The agent
    // never allocated or attached a console of its own the way the store does;
    // this flag's only effect was to turn on the sync transcript, which is now
    // `KORU_SYNC_DEBUG`. The flag is still parsed and still documented, and
    // what it should mean -- the help says "stay out of the notification
    // area", which `show_tray` does not consult -- is a question of its own.
    _console: bool,
    update_health_token: Option<String>,
    rehearsal_manifest: Option<std::path::PathBuf>,
    // A `koru://` link this process was launched to follow, when no agent was
    // there to hand it to. Resolved once the loop can be woken rather than
    // here: the store may still have to be started.
    pending_link: Option<koru_link::KoruLink>,
    // The only value accepted from native process activation. It has already
    // been decoded and bounded; the Store target remains in the device ledger.
    pending_activation: Option<koru_native_notifications::Activation>,
) -> Result<(), StartError> {
    // A released Agent refuses an updater-owned installation before claiming
    // the rendezvous. `InstanceLease::acquire` is nonblocking: a Run-key or
    // stray launch cannot wait across the swap, reserve the endpoint, and beat
    // the updater's health-token launch after exclusive ownership is released.
    let update_lease = if koru_update::build_info().updates_enabled()
        && (paths.owns_the_machine()
            || rehearsal_manifest.is_some()
            || update_health_token.is_some())
    {
        let update_paths = rooted_update_paths(
            &paths,
            update_health_token.as_deref(),
            rehearsal_manifest.as_deref(),
        )
        .or_else(koru_update::UpdatePaths::discover)
        .ok_or_else(|| StartError::Failed("could not resolve Koru update paths".into()))?;
        let acquire = if update_health_token.is_some() {
            koru_update::InstanceLease::acquire_restarted
        } else {
            koru_update::InstanceLease::acquire
        };
        Some(
            acquire(&update_paths, &crate::app::install_dir()).map_err(|error| {
                StartError::Failed(format!(
                    "Koru Agent cannot start while this installation is being updated: {error}"
                ))
            })?,
        )
    } else {
        None
    };
    // 1. The claim. Before anything is read, written or spawned.
    let endpoint = crate::endpoint(&paths).map_err(|e| StartError::Failed(e.to_string()))?;
    let listener = match rendezvous::bind(endpoint) {
        Ok(listener) => listener,
        Err(koru_ipc::IpcError::AlreadyBound(_)) => return Err(StartError::AlreadyRunning),
        Err(e) => return Err(StartError::Failed(e.to_string())),
    };
    koru_ipc::ipc_debug!("agent: bound {}", listener.endpoint().name());

    // 2. Who this device is. A file that will not parse is replaced rather than
    //    fatal — see `device.rs`.
    //    A reset the last run was asked for happens first, before anything
    //    here has opened `agent.db` (see `IdentityManager::reset`).
    crate::identity::IdentityManager::reset_if_requested(&paths, now_millis());
    let sync_identity = crate::identity::IdentityManager::open(&paths, now_millis());
    let account_descriptor = sync_identity.as_ref().ok().and_then(|identity| {
        koru_account_wire::SignedDeviceDescriptorV1::try_from(identity.descriptor.clone()).ok()
    });
    let enrollment_identity = sync_identity.as_ref().ok().map(|identity| {
        (
            identity.descriptor.clone(),
            identity.signing_key.secret_bytes(),
        )
    });
    let lifecycle_identity = enrollment_identity.clone();
    let (device, sync_status) = match &sync_identity {
        Ok(identity) => {
            let view = crate::identity::SqliteTrustRepository::open(&paths.identity_db())
                .and_then(|repository| repository.trust_view());
            let (trusted_device_count, authorization_generation) = view
                .map(|view| (view.devices.len(), view.effective_revision))
                .unwrap_or((0, 0));
            (
                Device {
                    id: identity.descriptor.device_id.to_storage(),
                    name: identity.descriptor.display_name.clone(),
                    created_at_millis: identity.descriptor.created_at_millis,
                },
                koru_agent_wire::SyncStatus {
                    endpoint_id: Some(identity.descriptor.endpoint_id.to_storage()),
                    sync_ready: true,
                    sync_error: None,
                    trusted_device_count,
                    authorization_generation,
                },
            )
        }
        Err(error) => {
            koru_ipc::ipc_debug!("agent: sync identity unavailable: {error}");
            let device = Device::open(&paths.device_file(), now_millis())
                .map_err(|e| StartError::Failed(format!("device identity: {e}")))?;
            (
                device,
                koru_agent_wire::SyncStatus {
                    sync_ready: false,
                    sync_error: Some(error.code.to_owned()),
                    ..Default::default()
                },
            )
        }
    };
    let instance_id = uuid::Uuid::new_v4().to_string();

    // The loop owns the agent, and the accept thread needs to hand connections
    // to it before the loop has been told they exist. One mutex, held only for
    // the push.
    // Kept for the producers below; `Agent` takes the original.
    let paths_for_producers = paths.clone();
    let machine_input_enabled = machine_input_enabled(&paths_for_producers, show_tray);
    tracing::info!(
        target: "koru::startup",
        enabled = machine_input_enabled,
        rooted = paths_for_producers.explicit_root().is_some(),
        "global input ownership selected"
    );
    // The session worker — this same executable re-run as `--worker
    // sessions` — is ready before any Terminal window can be routed here.
    // Its guard owns ordered shutdown and the kill fallback; no terminal
    // message or runtime object enters the Agent.
    let session_host = Some(
        crate::session_host::Supervisor::start(&paths_for_producers)
            .map_err(|error| StartError::Failed(format!("session host: {error}")))?,
    );
    let session_lifecycle = session_host.as_ref().map(|host| host.lifecycle());
    let session_catalog = session_lifecycle
        .as_ref()
        .map(crate::session_host::Lifecycle::catalog)
        .transpose()
        .map_err(|error| StartError::Failed(format!("session host catalog: {error}")))?;
    let mut initial_agent = Agent::new(paths, device.id.clone(), instance_id.clone());
    initial_agent.set_machine_input_enabled(machine_input_enabled);
    if let (Some(lifecycle), Some(catalog)) = (session_lifecycle, session_catalog) {
        initial_agent.set_session_lifecycle(lifecycle, catalog);
    }
    initial_agent.set_update_lease(update_lease);
    // Retained until the asynchronous gateway ensure has proved the bridge and
    // Store. A token acknowledged here would let the updater delete its backup
    // while a required service was still broken.
    initial_agent.set_update_health_token(update_health_token);
    if let Some(manifest) = rehearsal_manifest {
        let controller = koru_update::UpdateController::rehearsal(
            &manifest,
            koru_update::UpdatePaths::under(paths_for_producers.root()),
        )
        .map_err(|error| StartError::Failed(format!("local update rehearsal: {error}")))?;
        initial_agent.set_rehearsal_updates(controller, manifest);
    }
    match sync_identity {
        Ok(identity) => initial_agent.set_sync_identity(
            identity,
            sync_status.trusted_device_count,
            sync_status.authorization_generation,
        ),
        Err(error) => initial_agent.set_sync_unavailable(error.code),
    }
    match crate::identity::profile_lifecycle::pending_deleted_profiles(&paths_for_producers) {
        Ok(profiles) => initial_agent.set_pending_profile_cleanup(profiles),
        Err(code) => {
            koru_ipc::ipc_debug!("agent: Profile cleanup markers unavailable: {code}")
        }
    }
    let agent = Arc::new(Mutex::new(initial_agent));
    // **Producers start below, and the loop does not exist until step 5.**
    // Everything raised in between is held here and replayed when the loop
    // arrives — see `event::Mailbox`. It used to be dropped.
    let mailbox = Mailbox::new();
    let waker = mailbox.waker();
    let mut enrollment_network = None;
    let mut enrollment_sync = None;
    // The tray's Agents menu source: a watcher on the session host's catalog
    // (see `crate::agents_menu`). Started only beside a serving host, on the
    // mailbox waker so a catalog read before the loop exists is held rather
    // than lost.
    if session_host.is_some()
        && let Ok(endpoint) = crate::session_endpoint(&paths_for_producers)
        && let Ok(mut agent) = agent.lock()
    {
        agent.set_agents_menu_kick(crate::agents_menu::watch(endpoint, waker.clone()));
    }
    let sync_identity = agent.lock().ok().and_then(|agent| {
        agent.sync_identity().map(|identity| {
            (
                identity.descriptor.device_id,
                identity.endpoint_secret.secret_bytes(),
            )
        })
    });
    if let Some((local_device, endpoint_secret)) = sync_identity {
        let network = crate::network::AgentNetwork::start(&paths_for_producers, endpoint_secret)
            .and_then(|network| network.install().cloned())
            .map_err(|error| StartError::Failed(format!("device network: {error}")))?;
        // **Startup does not wait for the gateway.** The supervision is
        // created here and returns at once; the first child is spawned on its
        // own control thread and readiness is awaited only by ensure callers,
        // on worker threads. There is deliberately no equivalent of the
        // sessions host's blocking first-child wait above: a person holding a
        // dictation chord must not wait behind a Store that is opening.
        //
        // It goes here, after the device network is installed, because every
        // launch registers its expected bridge with it — and before the sync
        // control worker below, which ensures a gateway at the top of every
        // attempt.
        crate::gateway_host::start(&paths_for_producers);
        // The delegate worker (D12), after the sessions and gateway workers
        // it reaches. Like the gateway's, its supervision returns at once;
        // the child waits for the gateway to publish on its own thread.
        if let Err(error) = crate::delegate_host::start(&paths_for_producers) {
            tracing::error!(
                target: "koru::delegate_host",
                %error,
                "the delegate worker could not be supervised; delegated runs are unavailable"
            );
        }
        let control = crate::service::start_sync_control(
            paths_for_producers.clone(),
            local_device,
            endpoint_secret,
            network.runtime().clone(),
            waker.clone(),
        );
        enrollment_network = Some(network.runtime().clone());
        enrollment_sync = Some(control.clone());
        if let Some((descriptor, signing_secret)) = &lifecycle_identity {
            let signing_key = koru_device::DeviceSigningKey::from_secret_bytes(*signing_secret);
            if let Err(code) = crate::identity::profile_lifecycle::resume_pending(
                &paths_for_producers,
                descriptor,
                &signing_key,
                &control,
            ) {
                koru_ipc::ipc_debug!("agent: Profile lifecycle resume deferred: {code}");
            }
        }
        if let Ok(mut agent) = agent.lock() {
            agent.set_sync_control(control);
        }
    }
    let account_origin = koru_env::cloud_origin();
    if account_origin.is_none() {
        // Said once, at start: without it the account capability never
        // exists, and every account request is answered *not configured*
        // here, with no network attempt — a build with no Cloud is not an
        // outage, and nothing retries it.
        tracing::info!(
            variable = koru_env::name::KORU_PUBLIC_API_ORIGIN,
            "account: Koru Cloud is not set up in this build (no origin in the environment or baked in); sign-in and Cloud sync are off"
        );
        if let Ok(mut agent) = agent.lock() {
            agent.set_cloud_configured(false);
        }
    }
    if crate::account::startup_capability(account_origin.as_deref(), account_descriptor.as_ref())
        && let (Some(origin), Some(descriptor)) = (account_origin, account_descriptor)
    {
        let lifecycle_sync = enrollment_sync.clone();
        let account = (|| {
            let context = koru_oauth_credentials::CredentialContext::new(
                crate::segment(),
                paths_for_producers.root(),
                descriptor.device_id,
            )
            .map_err(|_| "credential_scope")?;
            let entry = context.entry(&origin).map_err(|_| "account_origin")?;
            let clock: Arc<dyn koru_account_runtime::ports::Clock> =
                Arc::new(koru_account_runtime::ports::SystemClock);
            let entropy: Arc<dyn koru_account_runtime::ports::Entropy> =
                Arc::new(koru_account_runtime::ports::SystemEntropy);
            let local_profiles: Arc<dyn crate::account::lifecycle::LocalProfileInventory> =
                Arc::new(crate::account::inventory::GatewayProfileInventory::new(
                    &paths_for_producers,
                ));
            let cloud_profiles = Arc::clone(&local_profiles);
            let cloud_sync: Arc<dyn koru_account_runtime::account::cloud_sync::CloudSyncPort> =
                Arc::new(crate::account::cloud_sync::AgentCloudSync::new(
                    &paths_for_producers,
                ));
            let enrollment =
                if let (Some((local, signing_secret)), Some(network), Some(sync), Some(issuer)) = (
                    enrollment_identity,
                    enrollment_network,
                    enrollment_sync,
                    koru_env::value(koru_env::name::KORU_PUBLIC_API_ISSUER),
                ) {
                    let mut client = koru_account_client::AccountClient::new(&origin)
                        .map_err(|_| "account_origin")?;
                    if let Some(value) =
                        koru_env::value(koru_env::name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT)
                            .filter(|value| !value.trim().is_empty())
                    {
                        let policy = koru_account_session::RegionalCredentialPolicy::parse(&value)
                            .map_err(|_| "account_configuration_invalid")?;
                        if issuer != policy.issuance_issuer() {
                            return Err("account_configuration_invalid");
                        }
                        client = client
                            .with_regional_credentials(policy)
                            .map_err(|_| "account_configuration_invalid")?;
                    }
                    let port = crate::enrollment::ProductionPort::new(
                        client.clone(),
                        issuer.clone(),
                        paths_for_producers.identity_db(),
                        local.clone(),
                        signing_secret,
                        network.clone(),
                        Arc::clone(&clock),
                        Arc::clone(&entropy),
                    )?
                    .with_local_profiles(Arc::clone(&local_profiles))
                    .with_cloud_sync(Arc::clone(&cloud_sync));
                    let changed_waker = waker.clone();
                    let changed_sync = sync.clone();
                    let enrollment =
                        Arc::new(crate::enrollment::Control::start(port, move |report| {
                            if report.durable_revision.is_some() {
                                changed_sync.refresh_authorization();
                            }
                            changed_waker.wake(AgentEvent::AccountChanged);
                        }));
                    let responder_waker = waker.clone();
                    let responder_sync = sync.clone();
                    let responder_session =
                        Arc::new(crate::enrollment::ResponderSessionGate::new());
                    network.set_enrollment_handler(Some(Arc::new(
                        crate::enrollment::ProductionResponder::new(
                            client,
                            issuer,
                            Arc::clone(&local_profiles),
                            Arc::clone(&clock),
                            paths_for_producers.identity_db(),
                            local,
                            signing_secret,
                            Arc::clone(&responder_session),
                            move |_| {
                                responder_sync.refresh_authorization();
                                responder_waker.wake(AgentEvent::AccountChanged);
                            },
                        ),
                    )));
                    Some((enrollment, responder_session))
                } else {
                    None
                };
            let mut service = crate::account::ProductionAccountService::new(
                koru_account_client::AccountClient::new(&origin).map_err(|_| "account_origin")?,
                descriptor,
                koru_device::DeviceSigningKey::from_secret_bytes(
                    lifecycle_identity
                        .as_ref()
                        .ok_or("account_device_identity")?
                        .1,
                ),
                if cfg!(windows) {
                    koru_account_core::DevicePlatform::Windows
                } else if cfg!(target_os = "macos") {
                    koru_account_core::DevicePlatform::Macos
                } else {
                    koru_account_core::DevicePlatform::Unknown
                },
                env!("CARGO_PKG_VERSION").into(),
                crate::account::AccountPorts {
                    delivery: Arc::new(crate::account::delivery::LoopbackDelivery),
                    local_profiles,
                    lifecycle_repository: crate::account::lifecycle::LifecycleRepository::open(
                        &paths_for_producers.profile_lifecycle_db(),
                    )
                    .map_err(|_| "account_origin")?,
                    clock,
                    entropy,
                    cloud_sync: Some(cloud_sync),
                },
            );
            if let Some((enrollment, responder_session)) = &enrollment {
                service = service
                    .with_enrollment(enrollment.clone())
                    .with_responder_session(Arc::clone(responder_session));
            }
            service = service.with_directory_namespace(entry.namespace());
            // The Cloud coordinator, composed only when this machine has
            // everything it needs to dial a worker: a regional credential
            // policy naming the region and issuer the sync credential is
            // verified against, the device network, the gateway control
            // channel and the device signing key. Without
            // any of them the account works exactly as before and Cloud is
            // simply not attached.
            let cloud = crate::cloud_host::compose(
                &origin,
                lifecycle_sync.clone(),
                lifecycle_identity.as_ref().map(|(_, secret)| *secret),
                sync_identity.map(|(_, endpoint)| endpoint),
                cloud_profiles,
                waker.clone(),
            );
            if let Some(cloud) = &cloud {
                let driver: Arc<dyn koru_account_runtime::cloud::Driver> =
                    Arc::clone(cloud) as Arc<dyn koru_account_runtime::cloud::Driver>;
                service = service.with_cloud(driver);
            }
            let account_waker = waker.clone();
            let lifecycle_paths = paths_for_producers.clone();
            let lifecycle_identity = lifecycle_identity.clone();
            let lifecycle_sync = lifecycle_sync.clone();
            let mut control = crate::account::AccountControl::start_with_directory(
                service,
                Arc::new(koru_oauth_credentials::OsCredentialStore),
                entry,
                Some(Arc::new(
                    koru_account_runtime::account::directory::SqliteAccountDirectory::new(
                        paths_for_producers.identity_db(),
                    ),
                )),
                Arc::new(koru_account_runtime::ports::SystemClock),
                true,
                move |mut event| {
                    if let (
                        Some((descriptor, signing_secret)),
                        Some(sync),
                        crate::account::WorkerEvent::Result { result, .. },
                    ) = (&lifecycle_identity, &lifecycle_sync, &mut event)
                    {
                        let signing_key =
                            koru_device::DeviceSigningKey::from_secret_bytes(*signing_secret);
                        let mut publish =
                            |lifecycle: &mut koru_agent_wire::AccountProfileLifecycleRow| {
                                let result = if lifecycle.lifecycle_state
                                    == koru_agent_wire::AccountProfileLifecycleState::Deleted
                                {
                                    crate::identity::profile_lifecycle::publish_deleted(
                                        &lifecycle_paths,
                                        descriptor,
                                        &signing_key,
                                        sync,
                                        lifecycle,
                                    )
                                } else {
                                    crate::identity::profile_lifecycle::publish_active(
                                        &lifecycle_paths,
                                        descriptor,
                                        &signing_key,
                                        sync,
                                        lifecycle,
                                    )
                                };
                                if let Err(code) = result {
                                    lifecycle.failure_code = Some(code.into());
                                }
                            };
                        match result {
                            koru_agent_wire::AccountOperationResult::ProfileDeleted {
                                lifecycle,
                                ..
                            } => publish(lifecycle),
                            koru_agent_wire::AccountOperationResult::ProfileRestored {
                                lifecycle,
                                ..
                            } => publish(lifecycle),
                            koru_agent_wire::AccountOperationResult::ProfileLifecycleReconciliation {
                                profiles,
                            } => profiles.iter_mut().for_each(&mut publish),
                            _ => {}
                        }
                    }
                    account_waker.wake(crate::account::account_event(event));
                },
            );
            if let Some((enrollment, responder_session)) = enrollment {
                control.set_enrollment_control(enrollment);
                control.set_responder_session_gate(responder_session);
            }
            if let Some(cloud) = cloud {
                let driver: Arc<dyn koru_account_runtime::cloud::Driver> = cloud;
                control.set_cloud_control(driver);
            }
            Ok::<_, &'static str>(control)
        })();
        match account {
            Ok(control) => {
                if let Ok(mut agent) = agent.lock() {
                    agent.set_account_control(control);
                }
                // A Workspace created after Cloud sync was switched on must
                // reach the Cloud without anyone reopening the switch, so the
                // remembered choice is re-applied on the same cadence the
                // Cloud coordinator already runs at. It cannot ride the
                // coordinator's own heartbeat because that thread is composed
                // before the account worker exists, and it holds only a weak
                // reference so it ends with the Agent.
                let reconcile = Arc::downgrade(&agent);
                if std::thread::Builder::new()
                    .name("koru-agent cloud sync reconcile".into())
                    .spawn(move || {
                        loop {
                            std::thread::sleep(crate::cloud_host::HEARTBEAT);
                            let Some(agent) = reconcile.upgrade() else {
                                return;
                            };
                            if let Ok(agent) = agent.lock() {
                                agent.reconcile_cloud_sync();
                            }
                        }
                    })
                    .is_err()
                {
                    tracing::warn!(
                        "cloud sync: no reconcile thread; a Workspace created later waits for the switch to be reopened"
                    );
                }
            }
            Err(code) => tracing::warn!(code, "account capability unavailable"),
        }
    }
    // `set_event_waker` starts the gateway ensure worker. Install the Agent
    // network first: the gateway's private bridge handshake needs it, and a
    // race here used to turn a healthy post-update launch into a readiness
    // failure merely because the worker won the scheduler.
    if let Ok(mut agent) = agent.lock() {
        agent.set_event_waker(waker.clone());
    }
    // Reminder authority, journal replay and native work belong to their own
    // producer. Start it before winit so an early result is held by Mailbox;
    // only the small content-free report crosses into the loop.
    let native_activation_ready = Arc::new(std::sync::atomic::AtomicBool::new(
        koru_native_notifications::windows_package_identity_available()
            && crate::notification_activation::com_broker_available(),
    ));
    let reminder_worker = crate::reminders::ReminderWorker::start_runtime(
        paths_for_producers.clone(),
        waker.clone(),
        Arc::clone(&native_activation_ready),
    );
    let socials_worker = if should_start_socials_publisher(koru_features::features()) {
        Some(koru_socials_publisher::Worker::start(
            paths_for_producers
                .explicit_root()
                .map(std::path::Path::to_path_buf),
            paths_for_producers.agent_dir(),
            uuid::Uuid::parse_str(&device.id)
                .map_err(|_| StartError::Failed("invalid publishing device identity".into()))?,
        ))
    } else {
        None
    };
    if let Ok(mut agent) = agent.lock() {
        agent.set_reminder_activation_ingress(reminder_worker.activation_ingress());
        agent.set_reminder_control_ingress(reminder_worker.control_ingress());
    }
    if let Some(activation) = pending_activation {
        tracing::info!(
            target: "koru::notification_activation",
            registration = activation.registration.as_str(),
            action = ?activation.action,
            "new Agent is transferring its startup notification action to the reminder queue"
        );
        reminder_worker.activate(activation).map_err(|_| {
            StartError::Failed("the notification activation queue is unexpectedly full".into())
        })?;
    }
    // Resolving a document link may ensure the gateway too, so it follows the
    // same network-before-service order. The waker already exists and holds the
    // result until the window loop attaches.
    if let Some(link) = pending_link {
        match link {
            koru_link::KoruLink::Document { document, node } => crate::service::resolve_link(
                paths_for_producers.clone(),
                waker.clone(),
                koru_store_core::ids::DocumentId::from_uuid(document),
                node.map(|node| node.to_string()),
            ),
            koru_link::KoruLink::Chat { chat, block } => {
                if let Err(error) = crate::ai_chat::open(
                    paths_for_producers.explicit_root(),
                    &crate::ai_chat::Open::Chat { chat, block },
                ) {
                    tracing::error!(error = %error, "AI chat link could not be launched");
                }
            }
            koru_link::KoruLink::Node(_) => {}
        }
    }

    // 3. Answer the endpoint. On a thread, because `accept` blocks and the loop
    //    may never block.
    {
        let agent = Arc::clone(&agent);
        let waker = waker.clone();
        let device_id = device.id.clone();
        let instance_id = instance_id.clone();
        let sync_status = sync_status.clone();
        std::thread::Builder::new()
            .name("koru-agent rendezvous".into())
            .spawn(move || {
                rendezvous::serve(
                    listener,
                    device_id,
                    instance_id,
                    sync_status,
                    waker,
                    move |connection| {
                        if let Ok(mut agent) = agent.lock() {
                            agent.adopt(connection);
                        }
                    },
                )
            })
            .map_err(|e| StartError::Failed(format!("could not answer the endpoint: {e}")))?;
    }

    // 4. The shortcut. Its own thread, because `RegisterHotKey` delivers to
    //    the thread that registered and `GetMessageW` blocks. A headless root
    //    remains inert; an interactive `--root ... --tray` launch explicitly
    //    joins the same OS-arbitrated namespace as an ordinary Agent.
    let hotkey = crate::hotkey::start(&paths_for_producers, machine_input_enabled, waker.clone());
    if let Ok(mut agent) = agent.lock() {
        agent.set_hotkey_control(hotkey);
    }

    // 5. The loop. Never returns until the agent stops. The tray is created
    //    *inside* it, on the winit thread, which is what `tray-icon` requires.
    let segment = crate::segment().to_string();
    let tray_waker = waker.clone();
    let tray_root = paths_for_producers
        .explicit_root()
        .map(std::path::Path::to_path_buf);
    let reminder_wakes = reminder_worker.wake_ingress();
    let delegate_wakes = crate::delegate_host::current().map(|host| host.wake_ingress());
    #[cfg(target_os = "macos")]
    start_macos_reminder_wakes(&reminder_wakes, delegate_wakes.as_ref());
    let ended = koru_window::run(move |_engine| Host {
        _session_host: session_host,
        _reminder_worker: reminder_worker,
        _socials_worker: socials_worker,
        reminder_wakes,
        delegate_wakes,
        agent,
        mailbox,
        tray: None,
        tray_segment: segment,
        tray_waker,
        show_tray,
        tray_root,
    });
    // **The loop has ended, so there is nobody left to answer.** A quit asks
    // the gateway to stop on its own lifecycle thread and the winit thread
    // never waits for it — but the *process* must, briefly, or the Job Object
    // kills the Store a few milliseconds into a shutdown it had agreed to.
    // Bounded; the job object is still the fallback.
    crate::app::finish_quit_stop();
    ended.map_err(|e| StartError::Failed(e.to_string()))
}

#[cfg(target_os = "macos")]
fn start_macos_reminder_wakes(
    wakes: &crate::reminders::ReminderWakeIngress,
    delegate: Option<&crate::delegate_host::DelegateWakeIngress>,
) {
    use block2::RcBlock;
    use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
    use objc2_foundation::{NSNotificationCenter, NSSystemTimeZoneDidChangeNotification};

    let zone_wakes = wakes.clone();
    let zone_delegate = delegate.cloned();
    let zone_block = RcBlock::new(move |_| {
        zone_wakes.wake(crate::reminders::Command::DeviceZoneChanged);
        if let Some(delegate) = &zone_delegate {
            delegate.wake();
        }
    });
    let center = NSNotificationCenter::defaultCenter();
    let zone_observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSSystemTimeZoneDidChangeNotification),
            None,
            None,
            &zone_block,
        )
    };

    let resume_wakes = wakes.clone();
    let resume_delegate = delegate.cloned();
    let resume_block = RcBlock::new(move |_| {
        resume_wakes.wake(crate::reminders::Command::Resume);
        if let Some(delegate) = &resume_delegate {
            delegate.wake();
        }
    });
    let workspace_center = NSWorkspace::sharedWorkspace().notificationCenter();
    let wake_observer = unsafe {
        workspace_center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidWakeNotification),
            None,
            None,
            &resume_block,
        )
    };

    // Both notification centers retain their observer tokens. They intentionally
    // live for this process: the captured sender becomes inert during shutdown,
    // and tearing observers down while AppKit is dispatching is the larger race.
    std::mem::forget(zone_observer);
    std::mem::forget(wake_observer);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QUESTIONS #6, reversed by the maintainers 2026-10-10: a build that hides
    /// Socials starts no publisher; one that shows it does.
    #[test]
    fn the_socials_publisher_starts_only_with_socials_shown() {
        use koru_features::Features;
        assert!(!should_start_socials_publisher(&Features::ALL_OFF));
        assert!(should_start_socials_publisher(&Features::ALL_ON));
        let only_socials = Features {
            socials: true,
            ..Features::ALL_OFF
        };
        assert!(should_start_socials_publisher(&only_socials));
        let all_but_socials = Features {
            socials: false,
            ..Features::ALL_ON
        };
        assert!(!should_start_socials_publisher(&all_but_socials));
        koru_features::testing::with(Features::ALL_OFF, || {
            assert!(!should_start_socials_publisher(koru_features::features()));
        });
    }

    #[test]
    fn only_an_interactive_root_opts_into_machine_input() {
        let rooted = Paths::for_root("rooted".into());
        let ordinary = Paths::for_channel("ordinary".into());
        assert!(!machine_input_enabled(&rooted, false));
        assert!(machine_input_enabled(&rooted, true));
        assert!(machine_input_enabled(&ordinary, false));
        assert!(machine_input_enabled(&ordinary, true));
    }

    #[test]
    fn rooted_health_restart_keeps_the_rooted_transaction_without_a_manifest() {
        let root = std::path::PathBuf::from("rooted-rehearsal");
        let paths = Paths::for_root(root.clone());
        let token = "0123456789abcdef0123456789abcdef";

        let update_paths = rooted_update_paths(&paths, Some(token), None).unwrap();

        assert_eq!(
            update_paths,
            koru_update::UpdatePaths::under(root.as_path())
        );
    }

    #[test]
    fn store_recovery_is_a_full_reminder_reconnect_wake() {
        assert_eq!(
            reminder_command(&AgentEvent::StoreAvailability {
                warning: None,
                running: true,
            }),
            Some(crate::reminders::Command::Reconnect)
        );
        assert_eq!(
            reminder_command(&AgentEvent::StoreAvailability {
                warning: Some("offline".into()),
                running: false,
            }),
            None
        );
    }
}

/// The loop's view of the agent.
///
/// A thin wrapper that takes the mutex for the length of a call, because the
/// accept thread pushes connections in and the loop reads them out. Every
/// method is one lock and no I/O — the blocking is all on the connection
/// threads, which is the rule the whole design turns on.
///
/// ## Every `WindowApp` method must be forwarded, and forgetting is silent
///
/// `resize_direction` / `resized` / `paint_before_showing` were once missing:
/// the overlay could only move (caption was forwarded), never grow, and height
/// never reached `windows.conf`. The trait defaults hide the omission.
///
/// **This is hand-written delegation over a trait full of defaults**, so a
/// method added to [`Agent`] and not added here does not fail to compile: it
/// falls through to the default and is simply never called. Four did — the
/// overlay would not dismiss on losing focus, its search reader was never
/// drained, and the wheel and the button release went nowhere — and every one
/// of them presented as a feature that did nothing rather than as an error.
///
/// The compiler cannot help, because `Agent`'s inherent methods and the trait's
/// defaults have the same names and either resolves. When adding a method to
/// `Agent`, add it here in the same edit.
struct Host {
    /// Dropped first when the Agent loop ends, so session trees stop before
    /// process-owned window/tray resources disappear.
    _session_host: Option<crate::session_host::Supervisor>,
    /// Blocking Store/event/timezone/native work stays on this producer. It is
    /// dropped with the host and never serviced from `wake`/`maintain`.
    _reminder_worker: crate::reminders::ReminderWorker,
    /// `None` while `ENABLE_SOCIALS` is off ([`should_start_socials_publisher`]).
    _socials_worker: Option<koru_socials_publisher::Worker>,
    /// Read by the Windows message hook; on macOS the wake sources were
    /// started from `start_macos_reminder_wakes` before this host existed.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    reminder_wakes: crate::reminders::ReminderWakeIngress,
    /// The same: the Windows message hook reads it; macOS started its wake
    /// sources before the host existed.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    delegate_wakes: Option<crate::delegate_host::DelegateWakeIngress>,
    agent: Arc<Mutex<Agent>>,
    /// What every producer raises into. Holds what arrives before the loop
    /// does, and is told where to deliver in [`attach`](koru_window::WindowApp::attach).
    mailbox: Arc<Mailbox>,
    /// Created in [`attach`](koru_window::WindowApp::attach), because that
    /// runs on the winit thread and `tray-icon` requires the icon to be built
    /// on the thread that pumps for it. Held here so it is dropped — and the
    /// icon removed — when the loop ends.
    tray: Option<crate::tray::Tray>,
    tray_segment: String,
    tray_waker: crate::event::Waker,
    /// Off under an explicit root unless its interactive launcher opted in: a
    /// gate filling the notification area with icons is its own answer.
    show_tray: bool,
    /// Named only for an explicitly rooted interactive agent. Ordinary agents
    /// keep the compact channel tooltip.
    tray_root: Option<std::path::PathBuf>,
}

/// A single lock wait or event dispatch longer than this is written down,
/// unconditionally. The loop went dark for 72 and then 112 seconds during
/// real installs with nothing to say where the time went; one second is far
/// above anything a healthy callback does and far below what a person
/// notices as a hang, so a line here is always news
/// ([PRINCIPLES](../../../guide/contributing/PRINCIPLES.md) — the flag nobody had set is
/// the diagnostic that was off during the incident).
const STALL_FLOOR: std::time::Duration = std::time::Duration::from_secs(1);

/// Store's existing supervisor signal is the observable reconnect lifecycle
/// for reminders. The live event tail remains only a wake hint; after Store
/// recovery this stronger command forces a full authoritative snapshot even
/// when no later event arrives.
fn reminder_command(event: &AgentEvent) -> Option<crate::reminders::Command> {
    matches!(event, AgentEvent::StoreAvailability { running: true, .. })
        .then_some(crate::reminders::Command::Reconnect)
}

/// `agent.lock()`, timed. The loop and the producer threads (rendezvous
/// accept, sync control) share this mutex, so a long wait names *contention*
/// — some other thread holding the agent; the dispatch timer in
/// [`Host::wake`] names work the loop did itself. Between them, a dark
/// stretch in the routing log becomes attributable. The line carries the
/// callsite and a duration, never content.
#[track_caller]
fn locked(agent: &Mutex<Agent>) -> std::sync::LockResult<std::sync::MutexGuard<'_, Agent>> {
    let started = std::time::Instant::now();
    let guard = agent.lock();
    let waited = started.elapsed();
    if waited >= STALL_FLOOR {
        let at = std::panic::Location::caller();
        tracing::warn!(
            target: "koru::stall",
            waited_ms = waited.as_millis() as u64,
            at = %at,
            "agent mutex wait"
        );
    }
    guard
}

macro_rules! on_agent {
    ($self:expr, |$agent:ident| $body:expr) => {
        on_agent!($self, |mut $agent| $body)
    };
    ($self:expr, |mut $agent:ident| $body:expr) => {
        match locked(&$self.agent) {
            #[allow(unused_mut)]
            Ok(mut $agent) => $body,
            // A poisoned mutex means a producer panicked. The agent carries no
            // durable state — the registry describes what is running, and what
            // is running has not changed — so carrying on is better than taking
            // the tray and the hotkey down with it.
            Err(_) => Default::default(),
        }
    };
}

impl koru_window::WindowApp for Host {
    type Event = AgentEvent;

    fn attach(&mut self, proxy: winit::event_loop::EventLoopProxy<Self::Event>) {
        // Before the loop starts, so anything already spawned can reach it —
        // **and so can anything that already spoke.** The endpoint has been
        // bound and answered since steps 1 and 3, so a shell can have finished
        // its handshake and sent a request before this line runs; the mailbox
        // held those and delivers them now.
        //
        // This used to install an `Option<EventLoopProxy>` and discard whatever
        // had been raised while it was `None`, on the reasoning that there was
        // no loop to receive it. There was no loop, but there was a *shell*, and
        // what it lost was its opening `Hello`. See `event::Mailbox`.
        self.mailbox.attach(move |event| {
            // **A send that fails is not an error.** It means the loop has
            // ended, which is the ordinary way this process stops.
            let _ = proxy.send_event(event);
        });
        // The agent is tray-shaped on every platform; on macOS that means an
        // accessory — see `Host::accessory` below, which is where the loop
        // asks. The icon is still set for the surfaces that remain (the
        // overlay's own chrome, and the Dock tile that would come back if the
        // policy were ever reverted — without it, a bare binary wears the
        // generic black `exec` tile). A no-op off macOS, and here because
        // `attach` is the winit thread, which AppKit requires.
        koru_window::set_dock_icon();
        // On the winit thread, and only now — the handlers wake through the
        // proxy that has just been installed.
        if self.show_tray {
            self.tray = crate::tray::Tray::show(
                &self.tray_segment,
                self.tray_root.as_deref(),
                self.tray_waker.clone(),
            );
            // **The windows learn whether there is an icon to hide behind**,
            // which only this process can say. Windows that connected before
            // the loop began are told now; later ones on `Hello`.
            let present = self.tray.as_ref().is_some_and(crate::tray::Tray::present);
            if let Ok(mut agent) = self.agent.lock() {
                agent.set_notification_area(present);
            }
            if let Some(tray) = &mut self.tray
                && let Ok(agent) = self.agent.lock()
            {
                tray.set_workspaces(&agent.open_workspaces());
                tray.set_agent_sessions(agent.agents_catalog());
                // **The registration may already have happened.** The hotkey
                // thread starts before the loop does, so its answer can be sat
                // on the agent before this menu exists — and there is no
                // guarantee of another wake to deliver it afterwards.
                tray.set_shortcut(agent.shortcut());
                tray.set_dictation_shortcut(agent.dictation_shortcut());
                tray.set_ai_shortcut(agent.ai_shortcut());
            }
            if !present {
                // Not fatal. An agent that refused to run because it could not
                // draw a picture would be a worse outcome than the one it was
                // avoiding — but somebody whose only way to quit has just
                // vanished should be told.
                eprintln!(
                    "koru-agent: there is no notification-area icon, so use {} to stop this \
                     agent.",
                    if cfg!(windows) {
                        "Task Manager"
                    } else {
                        "`koru-agent stop`"
                    }
                );
            }
        }
    }

    fn window(&self) -> winit::window::WindowAttributes {
        on_agent!(self, |agent| agent.window())
    }
    fn window_activation(&self) -> koru_window::WindowActivation {
        on_agent!(self, |agent| agent.window_activation())
    }
    fn window_generation(&self) -> u64 {
        on_agent!(self, |agent| agent.window_generation())
    }
    fn wants_window(&self) -> bool {
        on_agent!(self, |agent| agent.wants_window())
    }
    fn accessory(&self) -> bool {
        // The Windows agent is tray-only; the macOS agent is the same shape:
        // status item and overlay, no Dock tile. An accessory can still show
        // windows and still become active (`activate_app` works on one), which
        // is all the hotkey overlay needs.
        true
    }
    #[cfg(target_os = "windows")]
    fn windows_message_hook(&self) -> Option<Box<dyn FnMut(*const std::ffi::c_void) -> bool>> {
        let wakes = self.reminder_wakes.clone();
        let delegate = self.delegate_wakes.clone();
        Some(Box::new(move |raw| {
            use windows::Win32::UI::WindowsAndMessaging::{
                MSG, PBT_APMRESUMEAUTOMATIC, WM_POWERBROADCAST, WM_TIMECHANGE,
            };
            let message = unsafe { &*(raw.cast::<MSG>()) };
            if message.message == WM_TIMECHANGE {
                wakes.wake(crate::reminders::Command::DeviceZoneChanged);
                if let Some(delegate) = &delegate {
                    delegate.wake();
                }
            } else if message.message == WM_POWERBROADCAST
                && message.wParam.0 as u32 == PBT_APMRESUMEAUTOMATIC
            {
                wakes.wake(crate::reminders::Command::Resume);
                if let Some(delegate) = &delegate {
                    delegate.wake();
                }
            }
            false
        }))
    }
    fn should_exit(&self) -> bool {
        on_agent!(self, |agent| agent.should_exit())
    }
    fn close_requested(&mut self) {
        on_agent!(self, |agent| agent.close_requested())
    }
    fn is_caption(&self, x: f32, y: f32) -> bool {
        on_agent!(self, |agent| agent.is_caption(x, y))
    }
    fn cursor_icon(&self, x: f32, y: f32) -> winit::window::CursorIcon {
        on_agent!(self, |agent| agent.cursor_icon(x, y))
    }
    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<winit::window::ResizeDirection> {
        on_agent!(self, |agent| agent
            .resize_direction(width, height, x, y, maximised))
    }
    fn resized(&mut self, size: winit::dpi::PhysicalSize<u32>, scale_factor: f64) {
        on_agent!(self, |agent| agent.resized(size, scale_factor))
    }
    fn scale_factor_changed(&mut self, scale_factor: f64) -> bool {
        on_agent!(self, |agent| agent.scale_factor_changed(scale_factor))
    }
    fn paint_before_showing(&self) -> bool {
        on_agent!(self, |agent| agent.paint_before_showing())
    }
    fn moved(&mut self, position: winit::dpi::PhysicalPosition<i32>) {
        on_agent!(self, |agent| agent.moved(position))
    }
    fn wake(&mut self, event: Self::Event) -> bool {
        if let Some(command) = reminder_command(&event) {
            // Flag-only, bounded and non-blocking; Store reconciliation stays
            // on the reminder worker.
            self._reminder_worker.wake(command);
        }
        // The kind before the dispatch consumes the event — the stall line
        // names what the loop was inside, never what it carried.
        let kind = event.kind();
        let (
            changed,
            workspaces,
            agents_catalog,
            store_running,
            shortcut,
            dictation_shortcut,
            ai_shortcut,
            voice,
        ) = match locked(&self.agent) {
            Ok(mut agent) => {
                let started = std::time::Instant::now();
                let changed = agent.wake(event);
                let took = started.elapsed();
                if took >= STALL_FLOOR {
                    tracing::warn!(
                        target: "koru::stall",
                        took_ms = took.as_millis() as u64,
                        kind,
                        "agent event dispatch stalled"
                    );
                }
                (
                    changed,
                    agent.open_workspaces(),
                    agent.agents_catalog().clone(),
                    agent.store_running(),
                    agent.shortcut().clone(),
                    agent.dictation_shortcut().clone(),
                    agent.ai_shortcut(),
                    Some(agent.voice().clone()),
                )
            }
            Err(_) => (
                false,
                Vec::new(),
                crate::agents_menu::AgentsCatalog::default(),
                None,
                crate::tray::Shortcut::Unknown,
                crate::tray::Shortcut::Unknown,
                false,
                // Nothing is known through a poisoned lock, and a row
                // relabelled on no information is worse than a stale one.
                None,
            ),
        };
        if let Some(tray) = &mut self.tray {
            tray.set_workspaces(&workspaces);
            tray.set_agent_sessions(&agents_catalog);
            if let Some(store_running) = store_running {
                tray.set_store_running(store_running);
            }
            tray.set_shortcut(&shortcut);
            tray.set_dictation_shortcut(&dictation_shortcut);
            tray.set_ai_shortcut(ai_shortcut);
            // A held value and an enum compare -- the row is only re-labelled
            // when the state has actually moved. Deciding it is the agent's
            // and happens off this path; see `Agent::voice`.
            if let Some(voice) = &voice {
                tray.set_voice(voice);
            }
            // Read back rather than tracked: registration is per-user machine
            // state that another build -- or a person with regedit -- can
            // change underneath this process.
            tray.set_url_scheme(&crate::tray::url_scheme_state());
            // The same read-back rule for the relay switch: the network's
            // cached answer, so the tick cannot disagree with what the
            // runtime was last told to do. A cached atomic, never a disk.
            tray.set_relay(crate::tray::relay_state());
            // And the reading beneath it: the runtime's own relay-connection
            // state, tracked event-driven in the network status snapshot —
            // this is one cheap clone per wake, never a poll loop.
            tray.set_relay_status(crate::tray::relay_connection());
        }
        changed
    }
    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        on_agent!(self, |agent| agent.pointer_moved(x, y))
    }
    fn click(
        &mut self,
        engine: &mut koru_draw::text::TextEngine,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool {
        on_agent!(self, |agent| agent.click(engine, x, y, ctrl))
    }
    fn double_click(&mut self, engine: &mut koru_draw::text::TextEngine, x: f32, y: f32) -> bool {
        on_agent!(self, |agent| agent.double_click(engine, x, y))
    }
    fn key(
        &mut self,
        engine: &mut koru_draw::text::TextEngine,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
    ) -> bool {
        on_agent!(self, |agent| agent.key(engine, key, text, mods))
    }
    fn physical_key_pressed(
        &mut self,
        engine: &mut koru_draw::text::TextEngine,
        physical: winit::keyboard::PhysicalKey,
        key: &winit::keyboard::Key,
        text: Option<&str>,
        mods: winit::keyboard::ModifiersState,
        repeat: bool,
    ) -> bool {
        on_agent!(self, |agent| agent
            .physical_key_pressed(engine, physical, key, text, mods, repeat))
    }
    fn physical_key_released(&mut self, physical: winit::keyboard::PhysicalKey) -> bool {
        on_agent!(self, |agent| agent.physical_key_released(physical))
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        engine: &mut koru_draw::text::TextEngine,
        w: f32,
        h: f32,
    ) {
        on_agent!(self, |agent| agent.draw(scene, engine, w, h))
    }
    fn focus_changed(&mut self, focused: bool) -> bool {
        on_agent!(self, |agent| agent.focus_changed(focused))
    }
    fn system_theme_changed(&mut self, _dark: bool) -> bool {
        // winit's value is only a wake-up. Windows has reported it inverted;
        // Agent re-reads the same OS preference used at cold start.
        on_agent!(self, |agent| agent.system_theme_changed())
    }
    fn maintenance_deadline(&self) -> Option<std::time::Instant> {
        on_agent!(self, |agent| agent.maintenance_deadline())
    }
    fn maintain(&mut self, now: std::time::Instant) -> bool {
        on_agent!(self, |agent| agent.maintain(now))
    }
    fn animating(&self) -> bool {
        on_agent!(self, |agent| agent.animating())
    }
    fn scroll_by(&mut self, delta: f32, mods: winit::keyboard::ModifiersState) -> bool {
        on_agent!(self, |agent| agent.scroll_by(delta, mods))
    }
    fn mouse_up(&mut self) -> bool {
        on_agent!(self, |agent| agent.mouse_up())
    }
    fn take_window_request(&mut self) -> Option<koru_window::WindowRequest> {
        on_agent!(self, |agent| agent.take_window_request())
    }
    fn take_key_quarantine(&mut self) -> Vec<winit::keyboard::PhysicalKey> {
        on_agent!(self, |agent| agent.take_key_quarantine())
    }
}

/// Epoch milliseconds. Read once, at start-up, for the device file's stamp.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
