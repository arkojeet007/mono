// SPDX-License-Identifier: MPL-2.0

//! Agent-owned service operations: sync control, the voice worker, and the
//! Store/account/profile work that has to happen off the answering thread.
//!
//! **Gateway supervision left this file in Plan 4** and lives in
//! [`crate::gateway_host`], which owns it the way `session_host.rs` owns the
//! sessions worker. What remains here reaches the gateway the way any other
//! consumer does — `gateway_host::ensure` — and never launches, adopts or
//! stops one. Product credentials still travel directly between clients and
//! gateway discovery; Store persistence is still a worker inside the gateway
//! child, never a sibling daemon.

use crate::Paths;

/// Everything that can go wrong on the way to a voice worker.
///
/// Shaped like [`GatewayServiceError`] and deliberately not shared with it: the
/// codes are what a shell branches on and what a log line carries, so
/// `gateway_unreachable` and `voice_unreachable` being one string would make a
/// dictation that could not start indistinguishable from a store that could
/// not.
#[derive(Debug)]
pub enum VoiceServiceError {
    /// This installation offers no dictation, and the honest answer is not
    /// that a worker failed to start. Its own variant so a shell can say
    /// *voice is off here* rather than *not answering*, which is a different
    /// sentence and a different remedy. The reason says which: a switch
    /// somebody set, or a platform this build has no voice runtime for yet.
    /// One code for both, so nothing that branches on it changes.
    Disabled(koru_voice::Off, String),
    NotFound(String),
    Launch(String),
    Unavailable(String),
    Incompatible(String),
}

impl VoiceServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled(..) => "voice_disabled",
            Self::NotFound(_) => "voice_not_found",
            Self::Launch(_) => "voice_launch_failed",
            Self::Unavailable(_) => "voice_unreachable",
            Self::Incompatible(_) => "voice_incompatible",
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::Disabled(koru_voice::Off::Switch, _) => {
                "Dictation is switched off on this installation.".into()
            }
            Self::Disabled(koru_voice::Off::Platform, _) => koru_i18n::t(
                koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
                koru_i18n::Key::VoiceNotOnLinuxYet,
            )
            .into(),
            Self::NotFound(_) => "The Koru voice worker was not found.".into(),
            Self::Launch(_) => "The Koru voice worker could not be started.".into(),
            Self::Unavailable(_) => "The Koru voice worker is not answering.".into(),
            Self::Incompatible(_) => {
                "The Koru voice worker is incompatible with this build.".into()
            }
        }
    }
}

impl std::fmt::Display for VoiceServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled(_, message)
            | Self::NotFound(message)
            | Self::Launch(message)
            | Self::Unavailable(message)
            | Self::Incompatible(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for VoiceServiceError {}

/// The connect-or-launch decision lock for the voice worker.
///
/// Two windows pressing the chord at the same moment both reach here, and
/// without it both mint a launch id and spawn. The endpoint correctly admits
/// only one -- `Listener::bind` is the exclusion -- but the losing process
/// exits and the worker that answers carries the *other* launch's id, so the
/// loser reports `voice_incompatible` for a worker that is perfectly good.
///
/// **The gateway has no counterpart any more.** It had one — a process-wide
/// ensure lock around a connect-or-spawn decision — and Plan 4 deleted it with
/// the decision itself: the supervisor is the single owner of when a gateway
/// child exists, and `gateway_host::ensure` waits for it rather than racing to
/// start one. A dictation still must not wait behind a store that is opening,
/// which is why this lock stays the voice worker's own.
static VOICE_ENSURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A whole trust store is dozens of small records, so replicating the device
/// list needs no delta machinery: a revision change sends the full set, and
/// this beat heals whatever a disconnect dropped.
const TRUST_GOSSIP_ANTIENTROPY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct SyncControl {
    commands: std::sync::mpsc::Sender<PairingCommand>,
    events: std::sync::Arc<std::sync::Mutex<Vec<koru_sync_control::PairingEvent>>>,
    status: std::sync::Arc<std::sync::Mutex<koru_sync_control::ControlStatus>>,
    /// The control session currently open to the gateway, `None` between
    /// connections. Every Cloud request names it, and the gateway refuses a
    /// request naming a session it has since replaced.
    session: std::sync::Arc<std::sync::Mutex<Option<koru_sync_control::ControlSessionId>>>,
}

enum PairingCommand {
    RefreshAuthorization,
    /// One of control 7's Cloud operations, asked on the Cloud host's behalf
    /// and answered as the gateway answered it.
    Cloud {
        request: Box<koru_sync_control::ControlRequest>,
        reply: std::sync::mpsc::SyncSender<Result<koru_sync_control::ControlResponse, String>>,
    },
    Begin(Box<koru_pairing::PairingInvitationV1>),
    Cancel,
    Join(Box<koru_pairing::PairingJoinV1>),
    Complete {
        attempt_id: koru_sync_control::PairingAttemptId,
        prepared: Box<koru_pairing::PreparedPairingV1>,
    },
    SendTrustRecord {
        peer: koru_device::DeviceId,
        record: Box<koru_device::TrustRecord>,
    },
    CompleteTrustRecord {
        delivery_id: koru_sync_control::TrustDeliveryId,
        accepted: bool,
    },
    StateHistory {
        peer: koru_device::DeviceId,
        before: Option<i64>,
        reply: std::sync::mpsc::SyncSender<Result<koru_sync_control::ControlResponse, String>>,
    },
}

impl SyncControl {
    /// Wake the existing trust reread -> Store authorization replacement -> dial path.
    pub fn refresh_authorization(&self) {
        let _ = self.commands.send(PairingCommand::RefreshAuthorization);
    }

    pub fn status(&self) -> koru_sync_control::ControlStatus {
        self.status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// The control session open right now, if any.
    pub fn control_session(&self) -> Option<koru_sync_control::ControlSessionId> {
        *self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Ask the gateway one Cloud control question and wait for its answer.
    /// Blocking, bounded: the Cloud coordinator calls this from its own
    /// worker thread, never from the Agent's event loop.
    pub fn cloud_request(
        &self,
        request: koru_sync_control::ControlRequest,
    ) -> Result<koru_sync_control::ControlResponse, String> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(PairingCommand::Cloud {
                request: Box::new(request),
                reply,
            })
            .map_err(|_| "Store control is unavailable".to_owned())?;
        response
            .recv_timeout(std::time::Duration::from_secs(45))
            .map_err(|_| "Store control request timed out".to_owned())?
    }

    pub fn state_history(
        &self,
        peer: koru_device::DeviceId,
        before: Option<i64>,
    ) -> Result<koru_sync_control::ControlResponse, String> {
        let (reply, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(PairingCommand::StateHistory {
                peer,
                before,
                reply,
            })
            .map_err(|_| "Store control is unavailable".to_owned())?;
        response
            .recv_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| "Store history request timed out".to_owned())?
    }

    pub fn begin_pairing(&self, invitation: koru_pairing::PairingInvitationV1) {
        let _ = self
            .commands
            .send(PairingCommand::Begin(Box::new(invitation)));
    }

    pub fn cancel_pairing(&self) {
        let _ = self.commands.send(PairingCommand::Cancel);
    }

    pub fn join_pairing(&self, join: koru_pairing::PairingJoinV1) {
        let _ = self.commands.send(PairingCommand::Join(Box::new(join)));
    }

    pub fn complete_pairing(
        &self,
        attempt_id: koru_sync_control::PairingAttemptId,
        prepared: koru_pairing::PreparedPairingV1,
    ) {
        let _ = self.commands.send(PairingCommand::Complete {
            attempt_id,
            prepared: Box::new(prepared),
        });
    }

    pub fn take_pairing_events(&self) -> Vec<koru_sync_control::PairingEvent> {
        std::mem::take(
            &mut *self
                .events
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    pub fn send_trust_record(&self, peer: koru_device::DeviceId, record: koru_device::TrustRecord) {
        let _ = self.commands.send(PairingCommand::SendTrustRecord {
            peer,
            record: Box::new(record),
        });
    }

    pub fn complete_trust_record(
        &self,
        delivery_id: koru_sync_control::TrustDeliveryId,
        accepted: bool,
    ) {
        let _ = self.commands.send(PairingCommand::CompleteTrustRecord {
            delivery_id,
            accepted,
        });
    }
}

/// Keep the Store's volatile replication authority aligned with Agent trust.
/// All blocking discovery, HTTP and IPC stays on this worker.
pub fn start_sync_control(
    paths: Paths,
    local_device: koru_device::DeviceId,
    endpoint_secret: [u8; 32],
    network: koru_network_iroh::RuntimeHandle,
    waker: crate::event::Waker,
) -> SyncControl {
    let (commands, receiver) = std::sync::mpsc::channel();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let status = std::sync::Arc::new(std::sync::Mutex::new(
        koru_sync_control::ControlStatus::default(),
    ));
    let session = std::sync::Arc::new(std::sync::Mutex::new(None));
    let handle = SyncControl {
        commands,
        events: std::sync::Arc::clone(&events),
        status: std::sync::Arc::clone(&status),
        session: std::sync::Arc::clone(&session),
    };
    if cfg!(test) {
        return handle;
    }
    if std::thread::Builder::new()
        .name("koru-agent sync control".into())
        .spawn(move || {
            loop {
                if let Err(error) = run_sync_control(
                    &paths,
                    local_device,
                    endpoint_secret,
                    &network,
                    &receiver,
                    &events,
                    &status,
                    &session,
                    &waker,
                ) {
                    koru_ipc::ipc_debug!("sync control: {error}");
                }
                // The session ended with the connection: a Cloud request
                // made from here on names no session until the next Welcome.
                *session.lock().unwrap_or_else(|error| error.into_inner()) = None;
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        })
        .is_err()
    {
        koru_ipc::ipc_debug!("sync control: could not start worker");
    }
    handle
}

#[allow(clippy::too_many_arguments)]
fn run_sync_control(
    paths: &Paths,
    local_device: koru_device::DeviceId,
    endpoint_secret: [u8; 32],
    network: &koru_network_iroh::RuntimeHandle,
    pairing_commands: &std::sync::mpsc::Receiver<PairingCommand>,
    pairing_events: &std::sync::Arc<std::sync::Mutex<Vec<koru_sync_control::PairingEvent>>>,
    live_status: &std::sync::Arc<std::sync::Mutex<koru_sync_control::ControlStatus>>,
    live_session: &std::sync::Arc<std::sync::Mutex<Option<koru_sync_control::ControlSessionId>>>,
    waker: &crate::event::Waker,
) -> Result<(), String> {
    use koru_device::TrustRepository as _;
    use koru_sync_control::{
        AuthorizationSnapshot, AuthorizedPeer, CONTROL_FRAME_VERSION, CONTROL_PROTOCOL_VERSION,
        ControlGreeting, ControlHello, ControlRequest, ControlResponse, ControlSessionId,
        EndpointSecretBytes,
    };
    use std::collections::{BTreeMap, BTreeSet};

    // Supervision already owns when a gateway child exists. This coordinator
    // only waits for one and joins it; it never launches, adopts or stops one.
    crate::gateway_host::ensure(paths).map_err(|error| error.to_string())?;
    let endpoint = koru_ipc::Endpoint::for_service_root(
        koru_sync_control::CONTROL_SERVICE,
        crate::segment(),
        &paths.store_dir(),
    )
    .map_err(|error| error.to_string())?;
    let mut stream = koru_ipc::connect(&endpoint).map_err(|error| error.to_string())?;
    koru_ipc::set_timeouts(&stream, Some(std::time::Duration::from_secs(30)));
    if !koru_ipc::same_user(&stream) {
        return Err("Store control endpoint belongs to another user".into());
    }
    let session_id = ControlSessionId::new();
    let hello = ControlHello {
        protocol_version: CONTROL_PROTOCOL_VERSION,
        session_id,
        local_device,
    };
    koru_ipc::frame::write_frame(&mut stream, CONTROL_FRAME_VERSION, &hello)
        .map_err(|error| error.to_string())?;
    let (version, greeting) = koru_ipc::frame::read_frame::<_, ControlGreeting>(&mut stream)
        .map_err(|error| error.to_string())?;
    if version != CONTROL_FRAME_VERSION || !matches!(greeting, ControlGreeting::Welcome { .. }) {
        return Err(format!("Store refused sync control: {greeting:?}"));
    }
    *live_session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);

    let mut request_id = 0u64;
    let provision = ask_control(
        &mut stream,
        &mut request_id,
        ControlRequest::ProvisionEndpoint {
            secret: EndpointSecretBytes::new(endpoint_secret),
        },
    )?;
    if provision != ControlResponse::Ok {
        return Err(format!("endpoint provisioning was rejected: {provision:?}"));
    }

    // Gateway supervision and Store synchronization are two authorities. The
    // readiness credential above is deliberately control-only; reusing it for
    // the catalog makes every workspace refresh a 403 and leaves the peer
    // engine with nothing to authorize. Discover an ordinary renewable data
    // client separately, keeping lifecycle control out of Store requests.
    let client = koru_api_client::Client::connect_at(crate::segment(), paths.root())
        .map_err(|error| error.to_string())?;
    let mut sequence = 0u64;
    let mut previous: Option<Vec<AuthorizedPeer>> = None;
    let mut next_authorization = std::time::Instant::now();
    // Device-list gossip state: which peer was last sent which trust
    // revision, and when the next unconditional re-send is due.
    let mut gossiped: BTreeMap<koru_device::DeviceId, u64> = BTreeMap::new();
    let mut gossip_antientropy = std::time::Instant::now() + TRUST_GOSSIP_ANTIENTROPY;
    // Display names from the trust view, refreshed with each authorization
    // pass, so status log lines can say "Pixel8" beside the uuid — a person
    // debugging a mesh knows their devices by name, never by id.
    let mut peer_names: BTreeMap<koru_device::DeviceId, String> = BTreeMap::new();
    loop {
        if crate::network::current().is_some_and(crate::network::AgentNetwork::bridge_lost) {
            return Err("gateway network bridge closed".into());
        }
        match pairing_commands.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(command) => {
                if apply_pairing_command(&mut stream, &mut request_id, network, command)? {
                    next_authorization = std::time::Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        while let Ok(command) = pairing_commands.try_recv() {
            if apply_pairing_command(&mut stream, &mut request_id, network, command)? {
                next_authorization = std::time::Instant::now();
            }
        }
        if std::time::Instant::now() >= next_authorization {
            let workspace_rows = client
                .store()
                .workspaces()
                .map_err(|error| error.to_string())?;
            let exportable: BTreeMap<_, _> = workspace_rows
                .iter()
                .map(|workspace| (workspace.id, workspace.owner))
                .collect();
            let local_profiles: BTreeSet<_> = client
                .store()
                .profiles()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|profile| profile.id)
                .collect();
            let repository = crate::identity::SqliteTrustRepository::open(&paths.identity_db())
                .map_err(|error| error.to_string())?;
            let view = repository.trust_view().map_err(|error| error.to_string())?;
            let authorization = koru_device::Authorization::build(&view, &exportable);
            let devices = koru_device::Authorization::devices(&view);
            let deleted_profiles = view.deleted_profiles();
            let vias: BTreeMap<koru_device::DeviceId, Option<koru_device::DeviceId>> = devices
                .iter()
                .map(|peer| (peer.device_id, peer.via))
                .collect();
            let scopes: BTreeMap<koru_device::DeviceId, (bool, Vec<koru_store_core::ProfileId>)> =
                devices
                    .iter()
                    .map(|peer| {
                        let mut profiles = peer.authorized_profiles.clone();
                        let all_data = peer.all_data && deleted_profiles.is_empty();
                        if peer.all_data && !deleted_profiles.is_empty() {
                            profiles.extend(local_profiles.iter().copied());
                            profiles.retain(|profile| !deleted_profiles.contains(profile));
                        }
                        (peer.device_id, (all_data, profiles.into_iter().collect()))
                    })
                    .collect();
            let names: BTreeMap<koru_device::DeviceId, Option<String>> = devices
                .iter()
                .map(|peer| (peer.device_id, view.display_name_of(peer.device_id)))
                .collect();
            peer_names = names
                .iter()
                .filter_map(|(id, name)| name.clone().map(|name| (*id, name)))
                .collect();
            let builds: BTreeMap<koru_device::DeviceId, Option<koru_device::BuildPayload>> =
                devices
                    .iter()
                    .map(|peer| (peer.device_id, view.build_of(peer.device_id)))
                    .collect();
            let local_build = view.build_of(view.local_device.device_id);
            let mut grouped: BTreeMap<
                (
                    koru_device::RelationshipId,
                    koru_device::DeviceId,
                    koru_device::ReplicationEndpointId,
                ),
                Vec<koru_store_core::WorkspaceId>,
            > = BTreeMap::new();
            for peer in &devices {
                grouped
                    .entry((peer.relationship_id, peer.device_id, peer.endpoint_id))
                    .or_default();
            }
            for entry in &authorization.snapshot().entries {
                grouped
                    .entry((entry.relationship_id, entry.device_id, entry.endpoint_id))
                    .or_default()
                    .push(entry.workspace_id);
            }
            let peers: Vec<_> = grouped
                .into_iter()
                .map(
                    |((relationship_id, device_id, endpoint_id), mut workspaces)| {
                        workspaces.sort();
                        workspaces.dedup();
                        let (all_data, authorized_profiles) = scopes
                            .get(&device_id)
                            .cloned()
                            .unwrap_or((false, Vec::new()));
                        AuthorizedPeer {
                            relationship_id,
                            device_id,
                            endpoint_id,
                            all_data,
                            authorized_profiles,
                            workspaces,
                            via: vias.get(&device_id).copied().flatten(),
                            display_name: names.get(&device_id).cloned().flatten(),
                            build: builds.get(&device_id).cloned().flatten(),
                        }
                    },
                )
                .collect();

            if previous.as_ref() != Some(&peers) {
                sequence += 1;
                let snapshot = AuthorizationSnapshot {
                    session_id,
                    sequence,
                    local_device,
                    peers: peers.clone(),
                    local_build: local_build.clone(),
                };
                let response = ask_control(
                    &mut stream,
                    &mut request_id,
                    ControlRequest::ReplaceAuthorization {
                        snapshot: snapshot.clone(),
                    },
                )?;
                if response
                    != (ControlResponse::AuthorizationApplied {
                        session_id,
                        sequence,
                    })
                {
                    return Err(format!("authorization replacement failed: {response:?}"));
                }
                // Store installs the grant before the Agent can produce a
                // connection event for it. Otherwise a very fast local dial
                // can arrive at the opaque consumer while it still has no
                // principal to authorize, and that one-shot event is lost.
                network.replace_authorization(snapshot);
                for peer in &peers {
                    network.connect_peer(peer.device_id);
                    let _ = ask_control(
                        &mut stream,
                        &mut request_id,
                        ControlRequest::ConnectPeer {
                            peer: peer.device_id,
                        },
                    )?;
                }
                previous = Some(peers);
            }

            // Replicate the device list itself, the same way the data
            // replicates: the whole trust-record set — self-verifying, and
            // idempotent to apply — goes to every connected authorized peer
            // whenever the local revision moved, and to everyone again every
            // few minutes as anti-entropy for a peer that was offline during
            // a change. Applying a batch bumps the revision, so gossip
            // re-fires and the mesh converges transitively on its own.
            if std::time::Instant::now() >= gossip_antientropy {
                gossip_antientropy = std::time::Instant::now() + TRUST_GOSSIP_ANTIENTROPY;
                gossiped.clear();
            }
            let revision = view.effective_revision;
            let connected: BTreeSet<koru_device::DeviceId> = live_status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .peers
                .iter()
                .filter(|peer| {
                    matches!(
                        peer.state,
                        koru_sync_control::PeerState::ConnectedDirect
                            | koru_sync_control::PeerState::ConnectedRelay
                            | koru_sync_control::PeerState::ConnectedUnknown
                    )
                })
                .map(|peer| peer.device_id)
                .collect();
            let due: Vec<koru_device::DeviceId> = previous
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|peer| peer.device_id)
                .filter(|device| {
                    connected.contains(device) && gossiped.get(device) != Some(&revision)
                })
                .collect();
            if !due.is_empty()
                && let Ok(records) = repository.records()
                && !records.is_empty()
            {
                for device in due {
                    // A rejected or failed batch is retried on a later tick;
                    // neither is worth dropping the control session for.
                    let delivered = network.send_trust_records(device, records.clone());
                    match delivered.recv_timeout(std::time::Duration::from_secs(2)) {
                        Ok(true) => {
                            sync_log(format_args!(
                                "trust gossip to {}: sent {} records at revision {}",
                                device.to_storage(),
                                records.len(),
                                revision
                            ));
                            gossiped.insert(device, revision);
                        }
                        Ok(false) | Err(_) => sync_log(format_args!(
                            "trust gossip to {} deferred: peer not connected",
                            device.to_storage()
                        )),
                    }
                }
            }
            next_authorization = std::time::Instant::now() + std::time::Duration::from_secs(2);
        }

        let mut pending_events = network.take_pairing_events();
        let response = ask_control(&mut stream, &mut request_id, ControlRequest::PairingEvents)?;
        if let ControlResponse::PairingEvents { events } = response {
            pending_events.extend(events);
        }
        if !pending_events.is_empty() {
            pairing_events
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .extend(pending_events);
            waker.wake(crate::event::AgentEvent::PairingChanged);
        }
        if let ControlResponse::Status(status) =
            ask_control(&mut stream, &mut request_id, ControlRequest::Status)?
        {
            let mut current = live_status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *current != status {
                if current.peers != status.peers {
                    for peer in &status.peers {
                        sync_log(format_args!(
                            "device={} name={} state={:?} last_seen_millis={:?}",
                            peer.device_id.to_storage(),
                            peer_names.get(&peer.device_id).map_or("?", String::as_str),
                            peer.state,
                            peer.last_seen_millis
                        ));
                    }
                }
                if current.sync_event.as_ref().map(|event| event.sequence)
                    != status.sync_event.as_ref().map(|event| event.sequence)
                    && let Some(event) = &status.sync_event
                    && (!event.stage.starts_with("workspace_") || peer_object_logs_enabled())
                {
                    sync_log(format_args!(
                        "at_millis={} sequence={} direction={} stage={} peer={} peer_name={} workspace={} items={} error={}",
                        event.at_millis,
                        event.sequence,
                        sync_event_direction(&event.stage),
                        event.stage,
                        event.peer.to_storage(),
                        peer_names.get(&event.peer).map_or("?", String::as_str),
                        event
                            .workspace
                            .map_or_else(|| "-".into(), |id| id.to_storage()),
                        event
                            .items
                            .map_or_else(|| "-".into(), |items| items.to_string()),
                        event.error_code.as_deref().unwrap_or("-")
                    ));
                }
                *current = status;
            }
        }
    }
}

/// One line of the sync transcript, when it has been asked for.
///
/// **`--console` is not the switch, and used to be.** That conflated *where
/// output goes* with *how much of it there is*: a developer running the stack
/// in a terminal to watch one thing was handed every peer-state change and
/// every debounce reset, several lines per keystroke. The switch is
/// `KORU_SYNC_DEBUG`, shared with the shell's half of the same conversation so
/// that one variable produces both ends of it.
fn sync_log(args: std::fmt::Arguments<'_>) {
    koru_store_client::debug::write(args);
}

fn sync_event_direction(stage: &str) -> &'static str {
    if stage.starts_with("change_") {
        "local_to_peer"
    } else {
        "peer_to_local"
    }
}

fn peer_object_logs_enabled() -> bool {
    koru_env::switch(koru_env::name::KORU_PEER_OBJECT_LOGS)
}

fn apply_pairing_command(
    stream: &mut koru_ipc::Stream,
    request_id: &mut u64,
    network: &koru_network_iroh::RuntimeHandle,
    command: PairingCommand,
) -> Result<bool, String> {
    use koru_sync_control::ControlRequest;
    let command = match command {
        PairingCommand::RefreshAuthorization => return Ok(true),
        PairingCommand::StateHistory {
            peer,
            before,
            reply,
        } => {
            let response = ask_control(
                stream,
                request_id,
                ControlRequest::StateHistory { peer, before },
            );
            let _ = reply.send(response);
            return Ok(false);
        }
        PairingCommand::Cloud { request, reply } => {
            let response = ask_control(stream, request_id, *request);
            // A transport failure ends this control session, so the caller
            // hears it *and* the loop reconnects; a refusal is an answer.
            let failed = response.as_ref().err().cloned();
            let _ = reply.send(response);
            return match failed {
                Some(error) => Err(error),
                None => Ok(false),
            };
        }
        other => other,
    };
    let request = match command {
        PairingCommand::Begin(invitation) => {
            network.begin_pairing((*invitation).clone());
            ControlRequest::BeginPairing { invitation }
        }
        PairingCommand::Cancel => {
            network.cancel_pairing();
            ControlRequest::CancelPairing
        }
        PairingCommand::Join(join) => {
            network.join_pairing((*join).clone());
            ControlRequest::JoinPairing { join }
        }
        PairingCommand::Complete {
            attempt_id,
            prepared,
        } => {
            network.complete_pairing(attempt_id, (*prepared).clone());
            ControlRequest::CompletePairing {
                attempt_id,
                prepared,
            }
        }
        PairingCommand::SendTrustRecord { peer, record } => {
            network.send_trust_record(peer, (*record).clone());
            ControlRequest::SendTrustRecord { peer, record }
        }
        PairingCommand::CompleteTrustRecord {
            delivery_id,
            accepted,
        } => {
            network.complete_trust_record(delivery_id, accepted);
            ControlRequest::CompleteTrustRecord {
                delivery_id,
                accepted,
            }
        }
        PairingCommand::RefreshAuthorization => unreachable!(),
        PairingCommand::StateHistory { .. } => unreachable!(),
        PairingCommand::Cloud { .. } => unreachable!(),
    };
    match ask_control(stream, request_id, request)? {
        koru_sync_control::ControlResponse::Ok => Ok(false),
        other => Err(format!("pairing control rejected: {other:?}")),
    }
}

fn ask_control(
    stream: &mut koru_ipc::Stream,
    next_id: &mut u64,
    request: koru_sync_control::ControlRequest,
) -> Result<koru_sync_control::ControlResponse, String> {
    *next_id += 1;
    let id = *next_id;
    koru_ipc::frame::write_frame(
        stream,
        koru_sync_control::CONTROL_FRAME_VERSION,
        &koru_sync_control::RequestFrame { id, request },
    )
    .map_err(|error| error.to_string())?;
    let (version, response) =
        koru_ipc::frame::read_frame::<_, koru_sync_control::ResponseFrame>(stream)
            .map_err(|error| error.to_string())?;
    if version != koru_sync_control::CONTROL_FRAME_VERSION || response.id != id {
        return Err("Store control response did not match its request".into());
    }
    Ok(response.response)
}

// ------------------------------------------------------------- the voice worker

/// How long a launched worker has to bind its endpoint.
///
/// Shorter than the gateway's ten seconds, and that is a judgement about who is
/// waiting: nobody watches a store start, and a person holding a key down
/// watches this. A worker that has not bound in five seconds is not one whose
/// dictation was going to feel instant anyway.
const VOICE_START_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
const VOICE_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(25);

/// What a ready worker is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceReady {
    pub worker_version: String,
    pub process_id: u32,
    /// The launch this worker was started for, when it was supervised.
    pub supervisor_launch_id: Option<String>,
    /// Whether this call started it, or found one already there. A diagnostic:
    /// the ordinary steady state is `false`, because the worker outlives the
    /// dictation that started it.
    pub launched: bool,
}

/// **Start or adopt the voice worker beside this Agent.**
///
/// Shaped like the gateway supervision this file used to carry -- an ensure
/// lock, connect first, the binary resolved beside `current_exe()` and never
/// through `PATH`, `--root` passed through, `--supervised` with a bootstrap on
/// stdin, a bounded readiness poll, and launch verification -- and different
/// from it in three ways worth naming. **It is still a sibling launch**: the
/// voice worker is its own executable and is not a role of this image, so
/// Plan 4's move onto [`crate::gateway_host`] does not reach it.
///
/// **It is never called at launch.** The Agent starts a worker on the first
/// `EnsureService { service: Voice }`, which arrives when a person first
/// dictates. An installation with voice switched off pays nothing: no process,
/// no model load, no device.
///
/// **A `--root` Agent does not refuse it.** The remaining
/// `owns_the_machine()` refusals exist for replaceable machine-wide state such
/// as the Run key and `koru://` registration. An interactive root may opt into
/// OS-arbitrated shortcuts, and a rooted worker is what makes voice testable in
/// isolation. Its endpoint is qualified by the root, so
/// it cannot be mistaken for the worker somebody is dictating through.
///
/// **The launch is verified through `status`, not through the greeting.**
/// `koru_voice::WorkerGreeting::Welcome` deliberately carries no `launch_id`:
/// the contract crate is normative for the phone as well, and a phone hosting
/// the same session in-process has no launch to name. So the worker echoes the
/// id it was handed in `koru-voice-worker status`, and this reads it there -- one
/// extra round trip, inside a poll that was already happening. Written up as
/// Q-V34 in `docs/features/voice/archive/QUESTIONS.md`.
/// **The kill switch is read here and nowhere else in this process.**
/// `koru_voice::disabled()` is the one predicate four surfaces have to agree
/// about; this is supervision's reading of it, and it is taken before anything
/// is dialled so a switched-off installation does not even connect.
pub fn ensure_voice(paths: &Paths) -> Result<VoiceReady, VoiceServiceError> {
    let root = voice_service_root(paths);
    // The IPC/log root and the model cache are different coordinate systems.
    // A supervised service lives on Store's root, while models are shared
    // across channels unless an explicit channel root isolates them. Resolve
    // the cache exactly as the installing window does, then pin that exact
    // answer into the child environment so its service root cannot redirect
    // model discovery to `<store>/voice/models`.
    let model_root = koru_voice_models::ModelPaths::resolve(paths.explicit_root())
        .map_err(|error| VoiceServiceError::Unavailable(error.to_string()))?
        .root()
        .to_path_buf();
    ensure_voice_with(
        koru_voice::off(),
        || ask_voice(&root),
        |launch_id| launch_voice(&root, &model_root, launch_id),
    )
}

/// The one root used by every Voice client and by the supervised worker.
///
/// Agent's explicit root is the installation boundary; Voice is a Store-owned
/// service and therefore binds one level below it. Keeping this choice beside
/// supervision prevents a second host from accidentally addressing
/// `<root>` while the worker listens on `<root>/store`.
pub(crate) fn voice_service_root(paths: &Paths) -> std::path::PathBuf {
    paths.store_dir()
}

/// The decision, with the two effects handed in.
///
/// Separated so `two_concurrent_ensure_calls_launch_one_worker` can drive it
/// from two threads without a process being spawned on anybody's machine. What
/// it proves is the arrangement rather than the arithmetic: the lock is taken
/// here, and the connect-first check happens **inside** it, which is the whole
/// of why a second caller adopts rather than launching.
fn ensure_voice_with<Ask, Launch>(
    off: Option<koru_voice::Off>,
    ask: Ask,
    launch: Launch,
) -> Result<VoiceReady, VoiceServiceError>
where
    Ask: Fn() -> Result<Option<VoiceReady>, VoiceServiceError>,
    Launch: Fn(&str) -> Result<(), VoiceServiceError>,
{
    // **Before the lock and before the dial.** A switched-off installation must
    // not connect to anything, must not wait for anything, and must not queue
    // behind a decision it will refuse.
    match off {
        None => {}
        Some(koru_voice::Off::Switch) => {
            return Err(VoiceServiceError::Disabled(
                koru_voice::Off::Switch,
                format!(
                    "{} is set, so this installation offers no dictation",
                    koru_voice::DISABLE_VAR
                ),
            ));
        }
        // Not the switch's sentence: nobody set anything, and unsetting the
        // variable would not help.
        Some(koru_voice::Off::Platform) => {
            return Err(VoiceServiceError::Disabled(
                koru_voice::Off::Platform,
                "dictation is not built for this platform yet, so this installation offers no dictation".into(),
            ));
        }
    }
    let _decision = VOICE_ENSURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // **Connect first.** Whatever holds this endpoint is by construction the
    // worker for this channel and this root -- `Listener::bind` is the
    // exclusion, and the worker refuses a bootstrap naming another channel,
    // another root or another image before it ever binds. So adopting one is
    // right, and it is what makes a second concurrent caller free.
    if let Some(found) = ask()? {
        return Ok(found);
    }

    let launch_id = uuid::Uuid::new_v4().to_string();
    launch(&launch_id)?;

    let deadline = std::time::Instant::now() + VOICE_START_DEADLINE;
    loop {
        match ask()? {
            Some(found) => {
                // **Verification, not decoration.** Without it an Agent adopts
                // whatever was already running -- a worker from `.previous/`,
                // or one a second Agent started -- and calls it a successful
                // launch. It is a refusal rather than a retry because the
                // honest answer is that this Agent does not know what it is
                // talking to.
                if found.supervisor_launch_id.as_deref() != Some(launch_id.as_str()) {
                    return Err(VoiceServiceError::Incompatible(
                        "the voice endpoint belongs to a different launch instance".into(),
                    ));
                }
                tracing::debug!(
                    worker = found.worker_version.as_str(),
                    pid = found.process_id,
                    "the voice worker is up"
                );
                return Ok(VoiceReady {
                    launched: true,
                    ..found
                });
            }
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(VOICE_RETRY_EVERY);
            }
            None => {
                return Err(VoiceServiceError::Unavailable(format!(
                    "the voice worker did not start within {} seconds",
                    VOICE_START_DEADLINE.as_secs()
                )));
            }
        }
    }
}

/// Ask the worker on `root` what it is doing. `None` means nothing is
/// listening, which is an answer rather than a failure -- connecting *is* the
/// liveness check, and there is no file that could have gone stale.
fn ask_voice(root: &std::path::Path) -> Result<Option<VoiceReady>, VoiceServiceError> {
    match koru_voice_client::ask::status(root) {
        koru_voice_client::ask::Asked::Answered(report) => Ok(Some(VoiceReady {
            worker_version: report.worker_version.clone(),
            process_id: report.process_id,
            supervisor_launch_id: report.supervisor_launch_id.clone(),
            launched: false,
        })),
        koru_voice_client::ask::Asked::NotRunning => Ok(None),
        koru_voice_client::ask::Asked::Failed(why) => Err(VoiceServiceError::Unavailable(why)),
    }
}

/// Start `koru-voice-worker` beside this executable, with the bootstrap on its stdin.
///
/// **Stdin rather than an argument**: a command line is visible in every
/// process listing on the machine and a pipe between a parent and its child is
/// not. Nothing secret travels here yet, and the gateway's bootstrap carries a
/// capability -- putting the two on different channels would be one refactor
/// away from putting a credential on an argv.
fn launch_voice(
    root: &std::path::Path,
    model_root: &std::path::Path,
    launch_id: &str,
) -> Result<(), VoiceServiceError> {
    let binary = voice_binary()?;
    let image = std::fs::read(&binary).map_err(|error| {
        VoiceServiceError::Launch(format!(
            "could not fingerprint {}: {error}",
            binary.display()
        ))
    })?;
    let bootstrap = koru_voice::bootstrap::WorkerBootstrap {
        bootstrap_version: koru_voice::bootstrap::BOOTSTRAP_VERSION,
        launch_id: launch_id.to_string(),
        channel: crate::segment().into(),
        // The *fingerprints* travel and neither path does: a root is a
        // filesystem path, and docs/contributing/operations/LOGGING.md names those beside
        // credentials.
        root_fingerprint: koru_network_bridge::fingerprint(root.to_string_lossy().as_bytes()),
        installation_fingerprint: koru_network_bridge::fingerprint(image),
    };
    let mut command = voice_command(&binary, root, model_root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|error| {
        VoiceServiceError::Launch(format!("could not start {}: {error}", binary.display()))
    })?;
    let bytes = serde_json::to_vec(&bootstrap).map_err(|error| {
        VoiceServiceError::Launch(format!("could not encode the voice bootstrap: {error}"))
    })?;
    // Taken and dropped: the worker reads to end of stream, so a pipe left open
    // is a worker that never finishes reading its own bootstrap.
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        stdin.write_all(&bytes).map_err(|error| {
            VoiceServiceError::Launch(format!("could not deliver the voice bootstrap: {error}"))
        })?;
    }
    Ok(())
}

/// Build the child command while keeping its service and model roots explicit.
/// Separated from the spawn so a test can prove the regression-sensitive
/// environment without starting a microphone-capable process.
fn voice_command(
    binary: &std::path::Path,
    root: &std::path::Path,
    model_root: &std::path::Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(binary);
    command
        .arg("--root")
        .arg(root)
        .arg("--supervised")
        // `--root` names the worker endpoint and its log directory. It must
        // not accidentally become the model cache too; the installing window
        // resolved this same rule from the channel root, not Store's child.
        .env(koru_env::name::KORU_VOICE_MODELS, model_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

/// **Beside this exe, never from `PATH`.** The nine are built, signed and
/// shipped as one unit, and a different-channel `koru-voice-worker` on somebody's
/// `PATH` could belong to any of them.
fn voice_binary() -> Result<std::path::PathBuf, VoiceServiceError> {
    let exe = std::env::current_exe().map_err(|error| {
        VoiceServiceError::Launch(format!("could not find this executable: {error}"))
    })?;
    let directory = exe
        .parent()
        .ok_or_else(|| VoiceServiceError::Launch("this executable has no directory".into()))?;
    let candidate = directory.join(koru_update::VOICE_EXE);
    if !candidate.exists() {
        return Err(VoiceServiceError::NotFound(format!(
            "{} is not beside this executable",
            candidate.display()
        )));
    }
    Ok(candidate)
}

/// Ask the voice worker to end, on a thread, and never wait for it.
///
/// On a thread because `ask::stop` is a round trip that a worker mid-utterance
/// can hold for a moment, and this is called from the gesture that quits: a
/// tray icon that sat there after somebody clicked *Quit* is the failure being
/// avoided. Silent when nothing is running -- *stop* names a state to reach.
pub fn request_voice_stop(paths: &Paths) {
    if cfg!(test) {
        return;
    }
    let root = paths.store_dir();
    let _ = std::thread::Builder::new()
        .name("koru-agent voice stop".into())
        .spawn(move || {
            if let koru_voice_client::ask::Asked::Failed(why) = koru_voice_client::ask::stop(&root)
            {
                koru_ipc::ipc_debug!("voice: the worker would not stop: {why}");
            }
        });
}

/// Durably compare and set a Profile's exact Koru identity off the Agent loop.
pub fn bind_local_profile_account(
    paths: Paths,
    waker: crate::event::Waker,
    requester: (crate::registry::ConnectionId, u64),
    profile: koru_store_core::ids::ProfileId,
    expected: Option<koru_store_core::ProfileAccountBinding>,
    new: koru_store_core::ProfileAccountBinding,
) {
    let failure_waker = waker.clone();
    let profile_id = profile.to_storage();
    let failure_profile_id = profile_id.clone();
    let spawned = std::thread::Builder::new()
        .name("koru-agent bind local profile account".into())
        .spawn(move || {
            let result = store_client(&paths, Some(&waker))
                .map_err(|message| ("store_unreachable".to_owned(), message))
                .and_then(|client| {
                    client
                        .store()
                        .bind_profile_account(profile, expected, new)
                        .map_err(|error| (error.code().as_str().to_owned(), error.to_string()))
                })
                .map(|outcome| match outcome {
                    koru_api_client::ProfileAccountBindingOutcome::Bound => {
                        koru_agent_wire::LocalProfileAccountBindingOutcome::Bound
                    }
                    koru_api_client::ProfileAccountBindingOutcome::AlreadyBound => {
                        koru_agent_wire::LocalProfileAccountBindingOutcome::AlreadyBound
                    }
                });
            waker.wake(crate::event::AgentEvent::LocalProfileAccountBound {
                requester,
                profile_id,
                result,
            });
        });
    if let Err(error) = spawned {
        failure_waker.wake(crate::event::AgentEvent::LocalProfileAccountBound {
            requester,
            profile_id: failure_profile_id,
            result: Err(("internal".into(), error.to_string())),
        });
    }
}

/// Mark one empty Profile removed in the Store catalog without blocking the Agent loop.
pub fn remove_local_profile(
    paths: Paths,
    waker: crate::event::Waker,
    requester: (crate::registry::ConnectionId, u64),
    profile: koru_store_core::ids::ProfileId,
) {
    let failure_waker = waker.clone();
    let profile_id = profile.to_storage();
    let failure_profile_id = profile_id.clone();
    let spawned = std::thread::Builder::new()
        .name("koru-agent remove local profile".into())
        .spawn(move || {
            let result = store_client(&paths, Some(&waker))
                .map_err(|message| ("store_unreachable".to_owned(), message))
                .and_then(|client| {
                    client
                        .store()
                        .remove_profile_local(profile)
                        .map_err(|error| (error.code().as_str().to_owned(), error.to_string()))
                })
                .map(|outcome| match outcome {
                    koru_api_client::LocalProfileRemoval::Removed => {
                        koru_agent_wire::LocalProfileRemovalOutcome::Removed
                    }
                    koru_api_client::LocalProfileRemoval::AlreadyRemoved => {
                        koru_agent_wire::LocalProfileRemovalOutcome::AlreadyRemoved
                    }
                });
            waker.wake(crate::event::AgentEvent::LocalProfileRemoved {
                requester,
                profile_id,
                result,
            });
        });
    if let Err(error) = spawned {
        failure_waker.wake(crate::event::AgentEvent::LocalProfileRemoved {
            requester,
            profile_id: failure_profile_id,
            result: Err(("internal".into(), error.to_string())),
        });
    }
}

/// Write the Store's current metadata-only diagnostic report beside the
/// Agent, then open it with the platform's associated viewer.
/// Resolve a `koru://` link's document to a place, then hand it back to the
/// loop as a routable target.
///
/// **On a thread, because `locate_document` is a blocking HTTP call** and the
/// agent's loop must not wait on the store to answer a menu-less background
/// task. It also may need to *start* the store, which is unbounded.
pub fn resolve_link(
    paths: Paths,
    waker: crate::event::Waker,
    document: koru_store_core::ids::DocumentId,
    node: Option<String>,
) {
    let _ = std::thread::Builder::new()
        .name("koru-agent link".into())
        .spawn(move || {
            let resolved = (|| -> Result<koru_agent_wire::Target, String> {
                let client = store_client(&paths, Some(&waker))?;
                let location = client
                    .locate_document(document)
                    .map_err(|error| error.to_string())?;
                Ok(koru_agent_wire::Target {
                    profile: location.profile.to_storage(),
                    workspace: location.workspace.to_storage(),
                    document: Some(location.document.to_storage()),
                    node,
                    trashed: false,
                    // **Never any.** Match text places a caret and a transient
                    // flash for a *search* hit; a link carries no such evidence
                    // and inventing some would highlight text nobody looked for.
                    match_text: None,
                    // A link reuses a window. Opening a new one for every click
                    // in a browser is how a person ends up with nine.
                    new_window: false,
                })
            })();
            match resolved {
                Ok(target) => waker.wake(crate::event::AgentEvent::LinkResolved {
                    target: Box::new(target),
                }),
                Err(error) => {
                    // A person clicked a link and something has to say why
                    // nothing happened. Silence is the worst outcome here.
                    show_error("That Koru link could not be opened", &error);
                }
            }
        });
}

pub fn open_store_stats(paths: Paths, waker: Option<crate::event::Waker>) {
    let _ = std::thread::Builder::new()
        .name("koru-agent store stats".into())
        .spawn(move || {
            let result = (|| -> Result<std::path::PathBuf, String> {
                let client = store_client(&paths, waker.as_ref())?;
                let report = client.debug_store().map_err(|error| error.to_string())?;
                let destination = paths.agent_dir().join("store-stats.json");
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                let bytes =
                    serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
                std::fs::write(&destination, bytes).map_err(|error| error.to_string())?;
                open_location(&destination).map_err(|error| error.to_string())?;
                Ok(destination)
            })();
            if let Err(error) = result {
                show_error("Store stats could not be opened", &error);
            }
        });
}

/// Fire-and-forget: publish one event through the gateway, on its own
/// thread. The event loop never waits on it, and a bus that is not there
/// simply means the observation is not made — the update itself must never
/// degrade because its funnel could not be measured.
pub fn emit_event(paths: Paths, draft: koru_events::Draft) {
    let _ = std::thread::Builder::new()
        .name("koru-agent events emit".into())
        .spawn(move || {
            // Through the agent's own supervision, so an event that arrives
            // before the gateway does starts one rather than being lost —
            // the post-update `app.updated` is exactly that case.
            if let Ok(client) = store_client(&paths, None)
                && client.events().available()
            {
                let _ = client.events().send(draft);
            }
        });
}

/// Choose a destination and ask the live Store to create the backup while it
/// holds its own serialization lock.
#[cfg(windows)]
pub fn backup_store(paths: Paths, waker: Option<crate::event::Waker>) {
    let _ = std::thread::Builder::new()
        .name("koru-agent Store backup".into())
        .spawn(move || {
            let suggested = format!(
                "Koru Store {}.koru-store",
                koru_store_core::model::Timestamp::now().millis()
            );
            let Some(destination) = rfd::FileDialog::new()
                .set_title("Backup the Koru Store")
                .set_file_name(&suggested)
                .add_filter("Koru Store backup", &["koru-store"])
                .save_file()
            else {
                return;
            };
            let result = store_client(&paths, waker.as_ref()).and_then(|client| {
                client
                    .backup_store(&destination)
                    .map_err(|error| error.to_string())
            });
            match result {
                Ok(path) => {
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Info)
                        .set_title("Store backup complete")
                        .set_description(path.display().to_string())
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                }
                Err(error) => show_error("The Store could not be backed up", &error),
            }
        });
}

#[cfg(not(windows))]
pub fn backup_store(_paths: Paths, _waker: Option<crate::event::Waker>) {}

/// One of the store's diagnostic reports, for the `debug` subcommand.
///
/// Raw JSON, pretty-printed by the caller: the debug routes are a diagnostic
/// surface that gains fields whenever something new is worth seeing, and the
/// report may be pasted into a bug report, so it is metadata — ids, counts,
/// version vectors — and never document content.
pub fn store_debug(paths: &Paths, what: &str) -> Result<serde_json::Value, String> {
    store_client(paths, None)?
        .debug(what)
        .map_err(|error| error.to_string())
}

pub(crate) fn store_client(
    paths: &Paths,
    waker: Option<&crate::event::Waker>,
) -> Result<koru_api_client::Client, String> {
    let gateway = crate::gateway_host::ensure(paths).map_err(|error| {
        if let Some(waker) = waker {
            waker.wake(crate::event::AgentEvent::StoreAvailability {
                warning: Some(error.user_message()),
                running: false,
            });
        }
        error.to_string()
    })?;
    let _ = gateway;
    let client = koru_api_client::Client::connect_at(crate::segment(), paths.root())
        .map_err(|error| error.to_string())?;
    if let Some(waker) = waker {
        waker.wake(crate::event::AgentEvent::StoreAvailability {
            warning: None,
            running: true,
        });
    }
    Ok(client)
}

/// Wait for the supervised gateway and open its public generated contract.
/// The wait is bounded but real, so it never runs on the tray loop.
pub fn open_gateway_docs(paths: Paths) {
    let _ = std::thread::Builder::new()
        .name("koru-agent gateway docs".into())
        .spawn(move || {
            let result = (|| {
                let gateway =
                    crate::gateway_host::ensure(&paths).map_err(|error| error.to_string())?;
                let url = format!("{}/docs", gateway.api_endpoint.trim_end_matches('/'));
                koru_window::open_uri(&url).map_err(|error| error.to_string())
            })();
            if let Err(error) = result {
                show_error("Koru API Gateway docs could not be opened", &error);
            }
        });
}

#[cfg(windows)]
fn show_error(title: &str, error: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(error)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(not(windows))]
fn show_error(title: &str, error: &str) {
    eprintln!("{title}: {error}");
}

#[cfg(windows)]
pub fn open_location(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("explorer.exe")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
pub fn open_location(path: &std::path::Path) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests;
