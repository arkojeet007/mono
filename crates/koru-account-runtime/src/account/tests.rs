// SPDX-License-Identifier: MPL-2.0

use super::*;
mod cloud;
mod cloud_sync;

#[test]
fn rate_limited_account_calls_remain_retryable_and_do_not_revoke_session() {
    let error = ClientError::RateLimited {
        retry_after_millis: 42_000,
    };
    assert!(!refresh_was_revoked(&error));
    let mapped = map_client_error(error);
    assert_eq!(mapped.code, "account_unavailable");
    assert!(mapped.retryable);
    assert!(!mapped.clear_credential);
}
mod enrollment_sets;
mod native;
use crate::account::lifecycle::profile_availability;
use koru_account_client::{HttpMethod, HttpRequest, HttpResponse, Transport, TransportError};
use koru_account_core::DeviceProfileState;
use koru_account_wire::{
    AccountSummaryV1, AuthIdentitySummaryV1, AuthStartResponseV1, DeviceListResponseV1,
    DeviceProfileSummaryV1, DeviceSummaryV1, NativeExchangeResponseV1, ProfileListResponseV1,
    SessionSummaryV1,
};
use koru_device::{DeviceDescriptorV1, DeviceId, DeviceSigningKey, ReplicationEndpointSecret};
use koru_oauth_credentials::{CredentialContext, Error, SessionCredentialStore};
use std::io::{Read as _, Write as _};

struct InspectionResponse(u16, Vec<u8>);
impl Transport for InspectionResponse {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        assert_eq!(request.path_and_query, "/v1/oauth/native/session/inspect");
        Ok(HttpResponse {
            status: self.0,
            body: self.1.clone(),
        })
    }
}

#[test]
fn only_coherent_inspection_unauthenticated_refusal_allows_legacy_retirement() {
    for (status, body, rejected) in [
        (
            401,
            serde_json::json!({"code":"unauthenticated","message":"refused"}).to_string(),
            true,
        ),
        (
            503,
            serde_json::json!({"code":"unavailable","message":"unavailable"}).to_string(),
            false,
        ),
        (401, "truncated".into(), false),
        (200, "truncated".into(), false),
        (
            401,
            serde_json::json!({"code":"permission_denied","message":"refused"}).to_string(),
            false,
        ),
    ] {
        let mut service = ProductionAccountService::with_client(
            AccountClient::with_transport(Arc::new(InspectionResponse(status, body.into_bytes()))),
            signed_descriptor(),
            "test".into(),
        );
        let credential = SessionCredential::new(
            "80000000-0000-4000-8000-000000000008".into(),
            "current-secret".into(),
        )
        .unwrap();
        let error = service.inspect_credential(&credential).unwrap_err();
        assert_eq!(error.code == "credential_rejected", rejected);
        assert!(service.access.is_none());
        assert!(service.durable_owner.is_none());
    }
}

struct RemovalProbe(Arc<AtomicUsize>);
impl EnrollmentAccountControl for RemovalProbe {
    fn activate_owned(
        &self,
        _: Option<crate::enrollment::EnrollmentAuthorization>,
        _: u64,
    ) -> bool {
        false
    }
    fn refresh_authorization(
        &self,
        _: crate::enrollment::EnrollmentAuthorization,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> bool {
        false
    }
    fn set_foreground(&self, _: bool) {}
    fn wake(&self) -> bool {
        false
    }
    fn snapshot(&self) -> crate::enrollment::Snapshot {
        crate::enrollment::Snapshot::default()
    }
    fn cancel_durable_for(
        &self,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> Result<(), &'static str> {
        self.cancel_durable()
    }
    fn select_profiles(
        &self,
        _: std::collections::BTreeSet<ProfileId>,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> Result<crate::enrollment::Snapshot, &'static str> {
        Ok(crate::enrollment::Snapshot::default())
    }
    fn update_authorization(&self, _: crate::enrollment::EnrollmentAuthorization) -> bool {
        true
    }
    fn retry(&self, _: crate::enrollment::EnrollmentAuthorization, _: Option<ProfileId>) -> bool {
        true
    }
    fn cancel_durable(&self) -> Result<(), &'static str> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn logout_cancels_only_enrollment_of_the_account_actually_removed() {
    let a = crate::identity::AccountOwner {
        account: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
        auth_identity: "20000000-0000-4000-8000-000000000002".parse().unwrap(),
    };
    let b = crate::identity::AccountOwner {
        account: "10000000-0000-4000-8000-000000000003".parse().unwrap(),
        ..a
    };
    for removed in [None, Some(a), Some(b)] {
        let cleared = Arc::new(AtomicUsize::new(0));
        let mut service = ProductionAccountService::with_client(
            AccountClient::with_transport(Arc::new(NoCallbackTransport)),
            signed_descriptor(),
            "test".into(),
        )
        .with_enrollment_driver(Arc::new(RemovalProbe(cleared.clone())))
        .with_directory_namespace("a".repeat(64));
        service.durable_owner = Some(a);
        service.credential_removed(removed);
        service
            .execute(
                1,
                AccountCommand::Logout,
                None,
                &Cancel(Arc::new(AtomicBool::new(false))),
                &mut |_| {},
            )
            .unwrap();
        assert_eq!(
            cleared.load(Ordering::SeqCst),
            usize::from(removed == Some(a))
        );
    }
}

fn device_with_profile(
    device_id: DeviceId,
    profile_id: ProfileId,
    revoked: bool,
) -> DeviceSummaryV1 {
    DeviceSummaryV1 {
        version: Version1,
        device_id,
        display_name: "Test device".into(),
        platform: DevicePlatform::Windows,
        app_version: "test".into(),
        enrollment_protocol_version: None,
        peer_protocol_version: None,
        endpoint_id_hex: String::new(),
        profile_states: vec![DeviceProfileSummaryV1 {
            version: Version1,
            profile_id,
            state: DeviceProfileState::Provisioned,
        }],
        last_seen_at_millis: 0,
        revoked,
    }
}

pub(super) fn current_device_list() -> DeviceListResponseV1 {
    let descriptor = signed_descriptor();
    DeviceListResponseV1 {
        version: Version1,
        devices: vec![DeviceSummaryV1 {
            version: Version1,
            device_id: descriptor.device_id,
            display_name: descriptor.display_name,
            platform: current_platform(),
            app_version: "test".into(),
            enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
            peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
            endpoint_id_hex: String::new(),
            profile_states: Vec::new(),
            last_seen_at_millis: 1,
            revoked: false,
        }],
        truncated: false,
    }
}

#[test]
fn onboarding_projects_local_and_other_sources_without_filtering() {
    let profile = ProfileId::new();
    let local = DeviceId::new();
    let other = DeviceId::new();
    let account = KoruIdentityId::mint();
    let inventory = koru_store_wire::ProfileLifecycleInventory {
        profiles: vec![koru_store_wire::ProfileLifecycleInventoryItem {
            profile_id: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            account_binding: format!("koru:{account}"),
            removed_at_millis: None,
        }],
    };
    let only_local = vec![device_with_profile(local, profile, false)];
    assert_eq!(
        profile_availability(&inventory, account, &only_local, local, profile),
        (true, false)
    );

    let mut with_other = only_local;
    with_other.push(device_with_profile(other, profile, true));
    assert_eq!(
        profile_availability(&inventory, account, &with_other, local, profile),
        (true, false)
    );

    with_other[1].revoked = false;
    assert_eq!(
        profile_availability(&inventory, account, &with_other, local, profile),
        (true, true)
    );

    assert_eq!(
        profile_availability(&inventory, account, &[], local, profile),
        (true, false),
        "a hosted Profile remains a choice even when no device can supply content"
    );
}

#[test]
fn accepted_callback_attempts_to_close_and_refused_callback_stays_visible() {
    let options = wait_options("expected");
    assert!(options.success_html.contains("window.close()"));
    assert!(options.success_html.contains("close this tab"));
    assert!(!options.error_html.contains("window.close()"));
    assert!(options.error_html.contains("try again"));
}

fn entry() -> CredentialEntry {
    CredentialContext::new(
        "nightly",
        std::path::Path::new("test-root"),
        DeviceId::parse("a6258102-2534-4fab-bd4d-40717b066f29").unwrap(),
    )
    .unwrap()
    .entry("https://account.example.test")
    .unwrap()
}
pub(super) fn terminal_result(
    events: &mpsc::Receiver<WorkerEvent>,
    requester: u64,
    generation: u64,
) -> AccountOperationResult {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let event = events
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .expect("the account worker publishes a terminal result");
        if let WorkerEvent::Result {
            requester: seen_requester,
            generation: seen_generation,
            result,
            ..
        } = event
            && seen_requester == requester
            && seen_generation == generation
        {
            return result;
        }
    }
}

struct Blocking;
impl AccountService for Blocking {
    fn execute(
        &mut self,
        generation: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        cancel: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until && !cancel.cancelled() {
            std::thread::yield_now();
        }
        if cancel.cancelled() {
            return Err(ServiceError::terminal("cancelled"));
        }
        Ok(ServiceOutcome {
            state: AccountState {
                generation,
                stage: AccountStage::SignedIn,
                capability: true,
                ..Default::default()
            },
            credential: CredentialChange::Keep,
            result: AccountOperationResult::SignedIn,
            follow_up: None,
        })
    }
}
struct BrokenVault;
impl CredentialStore for BrokenVault {
    fn load(
        &self,
        _: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, Error> {
        Ok(None)
    }
    fn store(
        &self,
        _: &CredentialEntry,
        _: koru_oauth_credentials::SecretString,
    ) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
    fn clear(&self, _: &CredentialEntry) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
}
struct ClearFailsOnce {
    failed: AtomicBool,
    inner: SessionCredentialStore,
}
impl CredentialStore for ClearFailsOnce {
    fn load(
        &self,
        entry: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, Error> {
        self.inner.load(entry)
    }
    fn store(
        &self,
        entry: &CredentialEntry,
        secret: koru_oauth_credentials::SecretString,
    ) -> Result<(), Error> {
        self.inner.store(entry, secret)
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), Error> {
        if !self.failed.swap(true, Ordering::AcqRel) {
            return Err(Error::Unavailable);
        }
        self.inner.clear(entry)
    }
}
struct CredentialService;
impl AccountService for CredentialService {
    fn execute(
        &mut self,
        generation: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        Ok(ServiceOutcome {
            state: AccountState {
                generation,
                stage: AccountStage::SignedIn,
                capability: true,
                ..Default::default()
            },
            credential: CredentialChange::Replace(
                SessionCredential::new("session".into(), "refresh".into()).unwrap(),
            ),
            result: AccountOperationResult::SignedIn,
            follow_up: None,
        })
    }
}
struct RestoreProbe(mpsc::Sender<(String, [u8; 32])>);
impl AccountService for RestoreProbe {
    fn execute(
        &mut self,
        generation: u64,
        _: AccountCommand,
        restored: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        let restored = restored.unwrap();
        self.0
            .send((
                restored.session_id.clone(),
                refresh_retry_key(&restored.refresh_token),
            ))
            .unwrap();
        Ok(ServiceOutcome {
            state: AccountState {
                generation,
                stage: AccountStage::SignedIn,
                capability: true,
                ..Default::default()
            },
            credential: CredentialChange::Keep,
            result: AccountOperationResult::SignedIn,
            follow_up: None,
        })
    }
}
#[test]
fn retry_key_is_exact_stable_and_domain_separated() {
    assert_eq!(
        refresh_retry_key("seeded-token"),
        refresh_retry_key("seeded-token")
    );
    assert_ne!(
        refresh_retry_key("seeded-token"),
        refresh_retry_key("other-token")
    );
}

#[test]
fn startup_capability_requires_configured_origin_and_durable_identity() {
    let descriptor = signed_descriptor();
    assert!(startup_capability(
        Some("https://account.example.test"),
        Some(&descriptor)
    ));
    assert!(!startup_capability(None, Some(&descriptor)));
    assert!(!startup_capability(
        Some("https://account.example.test"),
        None
    ));
}
#[test]
fn cancellation_is_requester_owned_out_of_band_and_visible() {
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        Blocking,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        move |event| sent.send(event).unwrap(),
    );
    let generation = control.enqueue(7, AccountCommand::Retry).unwrap();
    assert_eq!(
        control.enqueue(8, AccountCommand::Cancel { generation }),
        Err(EnqueueError::NotOwner)
    );
    std::thread::sleep(Duration::from_millis(10));
    assert_ne!(control.state().stage, AccountStage::Cancelled);
    control
        .enqueue(7, AccountCommand::Cancel { generation })
        .unwrap();
    assert_eq!(
        terminal_result(&events, 7, generation),
        AccountOperationResult::Cancelled
    );
    assert_eq!(control.state().stage, AccountStage::Cancelled);
}

struct CommittedBeforeReturn(mpsc::Sender<AccountCommand>);
impl AccountService for CommittedBeforeReturn {
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        _: Option<&SessionCredential>,
        cancel: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        // This notification is the deterministic stand-in for the hosted
        // transaction commit. The call deliberately does not return until
        // the test has raced cancellation after that irreversible point.
        self.0.send(command.clone()).unwrap();
        while !cancel.cancelled() {
            std::thread::yield_now();
        }
        let result = if matches!(command, AccountCommand::StartGoogle { .. }) {
            AccountOperationResult::SignedIn
        } else {
            AccountOperationResult::Profile {
                profile: AccountProfileRow {
                    koru_identity_id: "20000000-0000-4000-8000-000000000001".into(),
                    profile_id: "10000000-0000-4000-8000-000000000001".into(),
                    name: None,
                    locale: None,
                    auth_identities: Vec::new(),
                },
            }
        };
        Ok(ServiceOutcome {
            state: AccountState {
                generation,
                stage: AccountStage::SignedIn,
                capability: true,
                ..Default::default()
            },
            credential: CredentialChange::Keep,
            result,
            follow_up: None,
        })
    }
}

#[test]
fn cancel_after_remote_commit_preserves_each_mutation_completion() {
    let commands = [
        AccountCommand::StartGoogle {
            locale: "en-US".into(),
        },
        AccountCommand::CreateProfile {
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            owner_display_name: "Seeded owner".into(),
            name: "seeded".into(),
            locale: Some("en-US".into()),
        },
        AccountCommand::AttachProfile {
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            name_snapshot: None,
            locale_snapshot: None,
        },
        AccountCommand::AddGoogle {
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            locale: "en-US".into(),
        },
        AccountCommand::RebindProfile {
            profile_id: "10000000-0000-4000-8000-000000000001".into(),
            erased_identity_id: "20000000-0000-4000-8000-000000000002".into(),
        },
    ];

    for command in commands {
        let (commit_sent, committed) = mpsc::channel();
        let (event_sent, events) = mpsc::channel();
        let control = AccountControl::start(
            CommittedBeforeReturn(commit_sent),
            Arc::new(SessionCredentialStore::default()),
            entry(),
            move |event| event_sent.send(event).unwrap(),
        );
        let generation = control.enqueue(17, command.clone()).unwrap();
        assert_eq!(
            committed.recv_timeout(Duration::from_secs(1)).unwrap(),
            command
        );
        control
            .enqueue(17, AccountCommand::Cancel { generation })
            .unwrap();

        let until = Instant::now() + Duration::from_secs(1);
        let mut completion = None;
        while Instant::now() < until && completion.is_none() {
            if let Ok(WorkerEvent::Result {
                requester: 17,
                generation: seen,
                result,
                ..
            }) = events.recv_timeout(Duration::from_millis(20))
                && seen == generation
            {
                completion = Some(result);
            }
        }
        assert!(
            matches!(
                completion,
                Some(AccountOperationResult::SignedIn)
                    | Some(AccountOperationResult::Profile { .. })
            ),
            "late cancellation hid the committed mutation"
        );

        let until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < until {
            if let Ok(WorkerEvent::Result {
                generation: seen, ..
            }) = events.recv_timeout(Duration::from_millis(10))
                && seen == generation
            {
                panic!("one generation published more than one terminal result");
            }
        }
    }
}

#[test]
fn bounded_queue_refuses_without_blocking() {
    let control = AccountControl::start(
        Blocking,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        |_| {},
    );
    control.enqueue(1, AccountCommand::Retry).unwrap();
    assert!((0..32).any(|_| control.enqueue(1, AccountCommand::Retry).is_err()));
    assert!(control.enqueue(1, AccountCommand::Logout).is_ok());
}

struct Unavailable;
impl AccountService for Unavailable {
    fn execute(
        &mut self,
        _: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        Err(ServiceError::retryable("account_unavailable"))
    }
}

#[test]
fn service_outage_is_explicit_and_has_bounded_inspectable_retry() {
    let control = AccountControl::start(
        Unavailable,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        |_| {},
    );
    control.enqueue(9, AccountCommand::Retry).unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().retry_at_millis.is_none() && Instant::now() < until {
        std::thread::yield_now();
    }
    let state = control.state();
    assert_eq!(state.stage, AccountStage::Failed);
    assert!(state.capability);
    assert_eq!(state.failure_code.as_deref(), Some("account_unavailable"));
    assert!(state.retry_at_millis.is_some());
    assert_eq!(state.queue_depth, 0);
    assert!(
        control.requester_active(9),
        "retry-held envelope still owns its request"
    );
    assert!(
        !control.requester_active(8),
        "ownership is requester-specific"
    );
}

struct RemoveCredential;
impl AccountService for RemoveCredential {
    fn execute(
        &mut self,
        _: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        Ok(ServiceOutcome {
            state: AccountState {
                capability: true,
                stage: AccountStage::SignedOut,
                ..Default::default()
            },
            credential: CredentialChange::Remove,
            result: AccountOperationResult::SignedOut,
            follow_up: None,
        })
    }
}

#[test]
fn requester_ownership_finishes_even_when_terminal_vault_failure_has_no_result() {
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        RemoveCredential,
        Arc::new(ClearFailsOnce {
            failed: AtomicBool::new(false),
            inner: SessionCredentialStore::default(),
        }),
        entry(),
        move |event| {
            let _ = sent.send(event);
        },
    );
    let generation = control.enqueue(7, AccountCommand::Retry).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while control.requester_active(7) && Instant::now() < until {
        std::thread::yield_now();
    }
    assert!(!control.requester_active(7));
    let state = control.state();
    assert_eq!(state.generation, generation);
    assert_eq!(state.stage, AccountStage::Failed);
    assert_eq!(state.retry_at_millis, None);
    assert!(
        !events
            .try_iter()
            .any(|event| matches!(event, WorkerEvent::Result { .. }))
    );
}

#[test]
fn hosted_recovery_backs_off_to_one_fixed_ten_minute_deadline() {
    assert_eq!(
        hosted_retry_delay(1, Duration::ZERO),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        hosted_retry_delay(2, Duration::from_secs(60)),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        hosted_retry_delay(3, Duration::from_secs(3 * 60)),
        Some(Duration::from_secs(4 * 60))
    );
    assert_eq!(
        hosted_retry_delay(4, Duration::from_secs(7 * 60)),
        Some(Duration::from_secs(3 * 60))
    );
    assert_eq!(hosted_retry_delay(5, Duration::from_secs(10 * 60)), None);
}

struct RetryProbe(mpsc::Sender<Instant>);
impl AccountService for RetryProbe {
    fn execute(
        &mut self,
        _: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        self.0.send(Instant::now()).unwrap();
        Err(ServiceError::retryable("account_unavailable"))
    }
}

#[test]
fn explicit_retry_accelerates_scheduled_backoff() {
    let (sent, calls) = mpsc::channel();
    let control = AccountControl::start(
        RetryProbe(sent),
        Arc::new(SessionCredentialStore::default()),
        entry(),
        |_| {},
    );
    control.enqueue(10, AccountCommand::Retry).unwrap();
    calls.recv_timeout(Duration::from_secs(1)).unwrap();
    control.enqueue(10, AccountCommand::Retry).unwrap();
    calls
        .recv_timeout(Duration::from_millis(250))
        .expect("explicit Retry should not wait for the one-second backoff");
}

struct RefreshingRetryProbe {
    started: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
impl AccountService for RefreshingRetryProbe {
    fn execute(
        &mut self,
        generation: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        progress: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        progress(ServiceProgress::State(AccountState {
            generation,
            stage: AccountStage::Refreshing,
            capability: true,
            ..Default::default()
        }));
        self.started.send(()).unwrap();
        self.release.recv().unwrap();
        Err(ServiceError::retryable("account_unavailable"))
    }
}

#[test]
fn retry_does_not_replace_the_visible_failure_with_refreshing() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let control = AccountControl::start(
        RefreshingRetryProbe {
            started: started_tx,
            release: release_rx,
        },
        Arc::new(SessionCredentialStore::default()),
        entry(),
        |_| {},
    );
    control.enqueue(10, AccountCommand::Retry).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    release_tx.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().failure_code.as_deref() != Some("account_unavailable")
        && Instant::now() < until
    {
        std::thread::yield_now();
    }

    control.enqueue(10, AccountCommand::Retry).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(control.state().stage, AccountStage::Failed);
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some("account_unavailable")
    );
    release_tx.send(()).unwrap();
}
#[test]
fn restart_automatically_refreshes_with_exact_retry_identity() {
    let entry = entry();
    let store = Arc::new(SessionCredentialStore::default());
    let record = SessionCredential::new("session-one".into(), "rotating-refresh".into()).unwrap();
    store.store(&entry, record.encode().unwrap()).unwrap();
    let (sent, received) = mpsc::channel();
    let _control = AccountControl::start(RestoreProbe(sent), store, entry, |_| {});
    let (session, retry_key) = received.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(session, "session-one");
    assert_eq!(retry_key, refresh_retry_key("rotating-refresh"));
}
#[test]
fn vault_failure_never_falls_back() {
    let control = AccountControl::start(CredentialService, Arc::new(BrokenVault), entry(), |_| {});
    control.enqueue(7, AccountCommand::Retry).unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().stage != AccountStage::Failed && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some("credential_unavailable")
    );
}
#[test]
fn disconnect_cancels_its_listener_generation() {
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        Blocking,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        move |event| sent.send(event).unwrap(),
    );
    let generation = control.enqueue(41, AccountCommand::Retry).unwrap();
    control.disconnect(41);
    assert_eq!(
        terminal_result(&events, 41, generation),
        AccountOperationResult::Cancelled
    );
    assert_eq!(control.state().stage, AccountStage::Cancelled);
}

struct PendingThenFailedLogout {
    cancelled: Arc<AtomicBool>,
    started: mpsc::Sender<()>,
}
impl AccountService for PendingThenFailedLogout {
    fn execute(
        &mut self,
        _: u64,
        command: AccountCommand,
        _: Option<&SessionCredential>,
        cancel: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        if matches!(command, AccountCommand::Logout) {
            return Err(ServiceError::retryable("account_unavailable"));
        }
        self.started.send(()).unwrap();
        while !cancel.cancelled() {
            std::thread::yield_now();
        }
        self.cancelled.store(true, Ordering::Release);
        Err(ServiceError::terminal("cancelled"))
    }
}

#[test]
fn logout_cancels_pending_work_and_clears_vault_despite_remote_failure() {
    let entry = entry();
    let store = Arc::new(SessionCredentialStore::default());
    store
        .store(
            &entry,
            SessionCredential::new("session".into(), "refresh".into())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = mpsc::channel();
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        PendingThenFailedLogout {
            cancelled: Arc::clone(&cancelled),
            started: started_tx,
        },
        store.clone(),
        entry.clone(),
        move |event| sent.send(event).unwrap(),
    );
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the restored credential begins its startup refresh");
    let generation = control.enqueue(4, AccountCommand::Logout).unwrap();
    assert_eq!(
        terminal_result(&events, 4, generation),
        AccountOperationResult::SignedOut
    );
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some("remote_revocation_pending")
    );
    assert!(cancelled.load(Ordering::Acquire));
    assert!(store.load(&entry).unwrap().is_none());
}

#[test]
fn every_uncertain_enrollment_clear_is_a_local_logout_persistence_failure() {
    for code in [
        crate::enrollment::ENROLLMENT_UNAVAILABLE,
        crate::enrollment::PERSISTENCE_FAILED,
    ] {
        let error = Result::<(), _>::Err(code)
            .map_err(|_| ServiceError::logout_incomplete(crate::enrollment::PERSISTENCE_FAILED))
            .unwrap_err();
        assert_eq!(error.code, crate::enrollment::PERSISTENCE_FAILED);
        assert!(error.clear_credential);
    }
}
struct DurableEnrollmentCancelFails;
impl AccountService for DurableEnrollmentCancelFails {
    fn execute(
        &mut self,
        _: u64,
        command: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        if matches!(command, AccountCommand::Logout) {
            Err(ServiceError::logout_incomplete(
                crate::enrollment::PERSISTENCE_FAILED,
            ))
        } else {
            Ok(ServiceOutcome {
                state: AccountState {
                    capability: true,
                    stage: AccountStage::SignedIn,
                    ..Default::default()
                },
                credential: CredentialChange::Keep,
                result: AccountOperationResult::SignedIn,
                follow_up: None,
            })
        }
    }
}

#[test]
fn logout_clears_vault_but_reports_failed_when_enrollment_target_clear_fails() {
    let entry = entry();
    let store = Arc::new(SessionCredentialStore::default());
    store
        .store(
            &entry,
            SessionCredential::new("session".into(), "refresh".into())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        DurableEnrollmentCancelFails,
        store.clone(),
        entry.clone(),
        move |event| sent.send(event).unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().stage != AccountStage::SignedIn && Instant::now() < until {
        std::thread::yield_now();
    }
    let generation = control.enqueue(6, AccountCommand::Logout).unwrap();
    assert_eq!(
        terminal_result(&events, 6, generation),
        AccountOperationResult::Failed {
            code: crate::enrollment::PERSISTENCE_FAILED.into(),
        }
    );
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some(crate::enrollment::PERSISTENCE_FAILED)
    );
    assert!(store.load(&entry).unwrap().is_none());
}
#[test]
fn logout_never_announces_signed_out_until_vault_erasure_succeeds() {
    let entry = entry();
    let store = Arc::new(ClearFailsOnce {
        failed: AtomicBool::new(false),
        inner: SessionCredentialStore::default(),
    });
    store
        .store(
            &entry,
            SessionCredential::new("session".into(), "refresh".into())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    let control = AccountControl::start(CredentialService, store.clone(), entry.clone(), |_| {});
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().stage != AccountStage::SignedIn && Instant::now() < until {
        std::thread::yield_now();
    }
    let generation = control.enqueue(5, AccountCommand::Logout).unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().generation != generation && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::Failed);
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some("credential_unavailable")
    );
    assert!(store.load(&entry).unwrap().is_some());
    let until = Instant::now() + Duration::from_secs(3);
    while control.state().stage != AccountStage::SignedOut && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert!(store.load(&entry).unwrap().is_none());
}

fn device_challenge_response(request: &HttpRequest) -> HttpResponse {
    let request: NativeExchangeRequestV1 = serde_json::from_slice(&request.body).unwrap();
    let challenge = koru_account_core::DeviceOwnershipChallenge {
        flow_id: AuthFlowId::mint(),
        parent_flow_id: Some(request.grant.0.split_once('.').unwrap().0.parse().unwrap()),
        device: koru_account_core::AccountDevice {
            descriptor: DeviceDescriptorV1::try_from(signed_descriptor()).unwrap(),
            registration: koru_account_core::AccountDeviceRegistration {
                koru_identity_id: KoruIdentityId::from_str("30000000-0000-4000-8000-000000000003")
                    .unwrap(),
                platform: current_platform(),
                app_version: "test".into(),
                enrollment_protocol_version: Some(koru_pairing::ENROLLMENT_VERSION),
                peer_protocol_version: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                registered_at_millis: 1,
                last_seen_at_millis: 1,
                revoked_at_millis: None,
            },
        },
        expires_at_millis: 2_000_000_000_000,
        nonce: [1; 32],
    };
    HttpResponse {
        status: 200,
        body: serde_json::to_vec(&koru_account_wire::DeviceOwnershipChallengeResponseV1 {
            version: Version1,
            challenge: base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&challenge).unwrap()),
        })
        .unwrap(),
    }
}

struct NativeFlowTransport;

#[test]
fn swapped_device_challenge_is_refused_before_signing_or_exchange() {
    struct SwappedChallenge;
    impl Transport for SwappedChallenge {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            assert!(request.path_and_query.ends_with("/native/device-challenge"));
            let mut response: koru_account_wire::DeviceOwnershipChallengeResponseV1 =
                serde_json::from_slice(&device_challenge_response(&request).body).unwrap();
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&response.challenge)
                .unwrap();
            let mut challenge: koru_account_core::DeviceOwnershipChallenge =
                serde_json::from_slice(&bytes).unwrap();
            challenge.parent_flow_id = Some(AuthFlowId::mint());
            response.challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&challenge).unwrap());
            Ok(HttpResponse {
                status: 200,
                body: serde_json::to_vec(&response).unwrap(),
            })
        }
    }
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(SwappedChallenge)),
        signed_descriptor(),
        "test".into(),
    );
    service.pending_exchange = Some(Pending {
        command: AccountCommand::StartGoogle {
            locale: "en".into(),
        },
        request: NativeExchangeRequestV1 {
            version: Version1,
            flow_id: AuthFlowId::mint(),
            grant: OneUseGrant("50000000-0000-4000-8000-000000000005.one-use".into()),
            client_state: OAuthState("state".into()),
            pkce_verifier: PkceVerifier("verifier".into()),
            device_ownership_proof: None,
        },
    });
    assert!(service.finish_exchange(1).is_err());
    assert!(
        service
            .pending_exchange
            .as_ref()
            .unwrap()
            .request
            .device_ownership_proof
            .is_none()
    );
}

impl Transport for NativeFlowTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query.ends_with("/native/device-challenge") {
            return Ok(device_challenge_response(&request));
        }
        // Apple's start answers exactly as Google's does, with Apple's page.
        let apple = request.path_and_query.ends_with("/apple/native/start");
        let body = if apple || request.path_and_query.ends_with("/google/native/start") {
            let start: GoogleNativeStartRequestV1 = serde_json::from_slice(&request.body)
                .map_err(|_| TransportError::InvalidRequest)?;
            std::thread::spawn(move || {
                let mut stream =
                    std::net::TcpStream::connect(("127.0.0.1", start.callback_port)).unwrap();
                write!(stream, "GET {0}?state={1}&outcome=completed&grant=50000000-0000-4000-8000-000000000005.one-use&return_target=account&expires_at_millis=2000000000000 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH, start.client_state.0).unwrap();
            });
            serde_json::to_vec(&AuthStartResponseV1 {
                version: Version1,
                flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
                authorization_url: if apple {
                    "https://appleid.apple.com/auth/authorize".into()
                } else {
                    "https://accounts.google.com/authorize".into()
                },
                expires_at_millis: 2_000_000_000_000,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/native/exchange") {
            serde_json::to_vec(&NativeExchangeResponseV1 {
                version: Version1,
                tokens: TokenPairV1 {
                    version: Version1,
                    session_id: AccountSessionId::from_str("20000000-0000-4000-8000-000000000002")
                        .unwrap(),
                    access_token: AccessToken("access-memory-only".into()),
                    access_expires_at_millis: 2_000_000_000_000,
                    refresh_token: koru_account_wire::RefreshToken("refresh-vault-only".into()),
                    refresh_idle_expires_at_millis: 2_000_000_000_000,
                    refresh_absolute_expires_at_millis: 2_000_000_000_000,
                },
                account: AccountSummaryV1 {
                    version: Version1,
                    koru_identity_id: KoruIdentityId::from_str(
                        "30000000-0000-4000-8000-000000000003",
                    )
                    .unwrap(),
                    display_name: "Seeded owner".into(),
                    auth_identities: Vec::new(),
                    terms_version_at_creation: "test".into(),
                    created_at_millis: 1,
                },
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/refresh") {
            serde_json::to_vec(&TokenPairV1 {
                version: Version1,
                session_id: AccountSessionId::from_str("20000000-0000-4000-8000-000000000002")
                    .unwrap(),
                access_token: AccessToken("refreshed-access".into()),
                access_expires_at_millis: 2_000_000_000_000,
                refresh_token: koru_account_wire::RefreshToken("rotated-refresh".into()),
                refresh_idle_expires_at_millis: 2_000_000_000_000,
                refresh_absolute_expires_at_millis: 2_000_000_000_000,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/profiles") {
            serde_json::to_vec(&ProfileListResponseV1 {
                version: Version1,
                profiles: vec![ProfileSummaryV1 {
                    version: Version1,
                    koru_identity_id: KoruIdentityId::mint(),
                    profile_id: ProfileId::parse("40000000-0000-4000-8000-000000000004").unwrap(),
                    name_snapshot: Some("Seeded profile".into()),
                    locale_snapshot: Some("en-US".into()),
                    auth_identities: Vec::new(),
                    lifecycle_revision: koru_account_core::ProfileLifecycleRevision::INITIAL,
                }],
            })
            .unwrap()
        } else {
            return Err(TransportError::InvalidRequest);
        };
        Ok(HttpResponse { status: 200, body })
    }
}

struct CollisionFlowTransport {
    mismatch: bool,
    original_state: Mutex<Option<String>>,
    exchanged: AtomicBool,
}

impl CollisionFlowTransport {
    fn new(mismatch: bool) -> Self {
        Self {
            mismatch,
            original_state: Mutex::new(None),
            exchanged: AtomicBool::new(false),
        }
    }
}

fn send_callback(port: u16, query: String) {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH,
        query
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
}

fn seeded_tokens(refresh: &str) -> TokenPairV1 {
    TokenPairV1 {
        version: Version1,
        session_id: AccountSessionId::from_str("20000000-0000-4000-8000-000000000002").unwrap(),
        access_token: AccessToken("access-memory-only".into()),
        access_expires_at_millis: 2_000_000_000_000,
        refresh_token: koru_account_wire::RefreshToken(refresh.into()),
        refresh_idle_expires_at_millis: 2_000_000_000_000,
        refresh_absolute_expires_at_millis: 2_000_000_000_000,
    }
}

impl Transport for CollisionFlowTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query.ends_with("/native/device-challenge") {
            return Ok(device_challenge_response(&request));
        }
        let body = if request.path_and_query.ends_with("/google/native/start") {
            let start: GoogleNativeStartRequestV1 = serde_json::from_slice(&request.body)
                .map_err(|_| TransportError::InvalidRequest)?;
            *self
                .original_state
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(start.client_state.0.clone());
            let mismatch = self.mismatch;
            std::thread::spawn(move || {
                send_callback(
                    start.callback_port,
                    format!(
                        "state={}&outcome=collision_required&collision_flow_id=10000000-0000-4000-8000-000000000001&reauthentication_flow_id=10000000-0000-4000-8000-000000000009&authorization_url=https%3A%2F%2Faccounts.google.com%2Fselect&expires_at_millis=2000000000000&reauthentication_state=phase-two-state",
                        start.client_state.0
                    ),
                );
                let outcome = if mismatch {
                    "state=phase-two-state&outcome=account_mismatch".to_owned()
                } else {
                    "state=phase-two-state&outcome=completed&grant=60000000-0000-4000-8000-000000000006.phase-two-grant&return_target=account&expires_at_millis=2000000000000".to_owned()
                };
                send_callback(start.callback_port, outcome);
            });
            serde_json::to_vec(&AuthStartResponseV1 {
                version: Version1,
                flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
                authorization_url: "https://accounts.google.com/authorize".into(),
                expires_at_millis: 2_000_000_000_000,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/native/exchange") {
            let exchange: NativeExchangeRequestV1 = serde_json::from_slice(&request.body)
                .map_err(|_| TransportError::InvalidRequest)?;
            assert_eq!(
                exchange.flow_id,
                AuthFlowId::from_str("10000000-0000-4000-8000-000000000009").unwrap()
            );
            assert_eq!(
                Some(exchange.client_state.0),
                self.original_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()
            );
            assert_eq!(
                exchange.grant.0,
                "60000000-0000-4000-8000-000000000006.phase-two-grant"
            );
            assert!(!exchange.pkce_verifier.0.is_empty());
            self.exchanged.store(true, Ordering::Release);
            serde_json::to_vec(&NativeExchangeResponseV1 {
                version: Version1,
                tokens: seeded_tokens("collision-refresh"),
                account: AccountSummaryV1 {
                    version: Version1,
                    koru_identity_id: KoruIdentityId::from_str(
                        "30000000-0000-4000-8000-000000000003",
                    )
                    .unwrap(),
                    display_name: "Seeded owner".into(),
                    auth_identities: Vec::new(),
                    terms_version_at_creation: "test".into(),
                    created_at_millis: 1,
                },
            })
            .unwrap()
        } else {
            return Err(TransportError::InvalidRequest);
        };
        Ok(HttpResponse { status: 200, body })
    }
}

struct NoCallbackTransport;
impl Transport for NoCallbackTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if !request.path_and_query.ends_with("/google/native/start") {
            return Err(TransportError::InvalidRequest);
        }
        let _: GoogleNativeStartRequestV1 =
            serde_json::from_slice(&request.body).map_err(|_| TransportError::InvalidRequest)?;
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&AuthStartResponseV1 {
                version: Version1,
                flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
                authorization_url: "https://accounts.google.com/authorize".into(),
                expires_at_millis: 2_000_000_000_000,
            })
            .unwrap(),
        })
    }
}

struct RecordingRefreshTransport(Mutex<Vec<Vec<u8>>>);
impl Transport for RecordingRefreshTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if !request.path_and_query.ends_with("/refresh") {
            return Err(TransportError::InvalidRequest);
        }
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.body);
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&seeded_tokens("rotated-after-vault-retry")).unwrap(),
        })
    }
}

struct StaleRegistrationTransport(AtomicBool);
impl Transport for StaleRegistrationTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = match (request.method, request.path_and_query.as_str()) {
            (HttpMethod::Get, "/v1/account/devices") => {
                let mut list = current_device_list();
                list.devices[0].peer_protocol_version =
                    Some(koru_sync_control::PEER_PROTOCOL_VERSION - 1);
                serde_json::to_vec(&list).unwrap()
            }
            (HttpMethod::Post, "/v1/account/devices/challenge") => {
                let registration: RegisterDeviceRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert!(registration.device_ownership_proof.is_none());
                let challenge = koru_account_core::DeviceOwnershipChallenge {
                    flow_id: AuthFlowId::mint(),
                    parent_flow_id: None,
                    device: koru_account_core::AccountDevice {
                        descriptor: DeviceDescriptorV1::try_from(registration.descriptor).unwrap(),
                        registration: koru_account_core::AccountDeviceRegistration {
                            koru_identity_id: "30000000-0000-4000-8000-000000000003"
                                .parse()
                                .unwrap(),
                            platform: registration.platform,
                            app_version: registration.app_version,
                            enrollment_protocol_version: registration.enrollment_protocol_version,
                            peer_protocol_version: registration.peer_protocol_version,
                            registered_at_millis: 1,
                            last_seen_at_millis: 1,
                            revoked_at_millis: None,
                        },
                    },
                    expires_at_millis: 2_000_000_000_000,
                    nonce: [9; 32],
                };
                serde_json::to_vec(&koru_account_wire::DeviceOwnershipChallengeResponseV1 {
                    version: Version1,
                    challenge: base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(serde_json::to_vec(&challenge).unwrap()),
                })
                .unwrap()
            }
            (HttpMethod::Post, "/v1/account/devices") => {
                let registration: RegisterDeviceRequestV1 =
                    serde_json::from_slice(&request.body).unwrap();
                assert!(registration.device_ownership_proof.is_some());
                self.0.store(true, Ordering::Release);
                let mut list = current_device_list();
                serde_json::to_vec(&list.devices.remove(0)).unwrap()
            }
            route => panic!("unexpected stale registration route: {route:?}"),
        };
        Ok(HttpResponse { status: 200, body })
    }
}

#[test]
fn saved_session_refreshes_stale_protocol_registration_without_another_login() {
    let transport = Arc::new(StaleRegistrationTransport(AtomicBool::new(false)));
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    service.durable_owner = Some(crate::identity::AccountOwner {
        account: "30000000-0000-4000-8000-000000000003".parse().unwrap(),
        auth_identity: "40000000-0000-4000-8000-000000000004".parse().unwrap(),
    });
    service
        .refresh_device_registration(&AccessToken("fresh-access".into()))
        .unwrap();
    assert!(transport.0.load(Ordering::Acquire));
}

/// Every `koru_account` line `run` logs on this thread, as `message k=v ...`.
///
/// **One global subscriber, not a scoped `with_default`.** `tracing` caches
/// each callsite's interest for the whole process. A test thread that first
/// reached a callsite while no capture was installed could store "never"
/// for it just as another thread's scoped capture rebuilt the cache, and that
/// capture then lost the line: on a busy runner
/// `restore_logs_which_stored_slot_was_refused_without_a_value` lost its
/// first line this way. The global subscriber answers `sometimes` for this
/// target, so no callsite is ever cached as off and every event asks
/// `enabled`, which looks for a capture on the current thread.
pub(super) fn account_log_lines(run: impl FnOnce()) -> Vec<String> {
    use std::cell::RefCell;
    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }
    struct Capture;
    struct Line(String);
    impl tracing::field::Visit for Line {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!(" {}={:?}", field.name(), value));
        }
    }
    impl tracing::Subscriber for Capture {
        fn register_callsite(
            &self,
            metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            if metadata.target() == "koru_account" {
                tracing::subscriber::Interest::sometimes()
            } else {
                tracing::subscriber::Interest::never()
            }
        }
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            metadata.target() == "koru_account" && LINES.with(|lines| lines.borrow().is_some())
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = Line(String::new());
            event.record(&mut line);
            LINES.with(|lines| {
                if let Some(lines) = lines.borrow_mut().as_mut() {
                    lines.push(line.0.trim().to_owned());
                }
            });
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(Capture)
            .expect("no other global subscriber in this test binary");
    });
    // A callsite whose registration raced the install above can still hold
    // "never"; after the install nothing can store that again, so one rebuild
    // here settles it before `run` logs anything.
    tracing::callsite::rebuild_interest_cache();
    LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
    run();
    LINES
        .with(|lines| lines.borrow_mut().take())
        .unwrap_or_default()
}

#[test]
fn refresh_and_restore_log_rotation_and_access_age_without_a_value() {
    let transport = Arc::new(RecordingRefreshTransport(Mutex::new(Vec::new())));
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport),
        signed_descriptor(),
        "test".into(),
    );
    let presented = SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "presented-refresh".into(),
    )
    .unwrap();
    let lines = account_log_lines(|| {
        let rotated = service.refresh(&presented).unwrap();
        service.credential_restored(&rotated, None);
    });
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        lines[0],
        "message=credential refresh answered rotated=true access_expires_at_millis=2000000000000"
    );
    // The service reads the system clock here; only the shape is fixed.
    assert!(
        lines[1].starts_with(
            "message=credential restored access_held=true access_remaining_millis=Some("
        ) && lines[1].ends_with(") access_expired=false"),
        "{}",
        lines[1]
    );
    for line in &lines {
        for secret in [
            "presented-refresh",
            "rotated-after-vault-retry",
            "access-memory-only",
            "20000000-0000-4000-8000-000000000002",
        ] {
            assert!(!line.contains(secret), "{line}");
        }
    }
}

#[test]
fn restored_session_without_access_is_due_for_background_refresh() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NoCallbackTransport)),
        signed_descriptor(),
        "test".into(),
    );
    service.durable_owner = Some(crate::identity::AccountOwner {
        account: "30000000-0000-4000-8000-000000000003".parse().unwrap(),
        auth_identity: "40000000-0000-4000-8000-000000000004".parse().unwrap(),
    });
    service.durable_session_id = Some("saved-session".into());

    assert!(service.access_refresh_at_millis().is_some());
}

struct AckLossExchangeTransport {
    lose_first: bool,
    starts: AtomicUsize,
    exchanges: Mutex<Vec<Vec<u8>>>,
}
impl AckLossExchangeTransport {
    fn new(lose_first: bool) -> Self {
        Self {
            lose_first,
            starts: AtomicUsize::new(0),
            exchanges: Mutex::new(Vec::new()),
        }
    }
}
impl Transport for AckLossExchangeTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if request.path_and_query.ends_with("/native/device-challenge") {
            return Ok(device_challenge_response(&request));
        }
        if request.path_and_query.ends_with("/google/native/start") {
            self.starts.fetch_add(1, Ordering::AcqRel);
            let start: GoogleNativeStartRequestV1 = serde_json::from_slice(&request.body)
                .map_err(|_| TransportError::InvalidRequest)?;
            std::thread::spawn(move || {
                send_callback(
                    start.callback_port,
                    format!(
                        "state={}&outcome=completed&grant=50000000-0000-4000-8000-000000000005.one-use&return_target=account&expires_at_millis=2000000000000",
                        start.client_state.0
                    ),
                );
            });
            return Ok(HttpResponse {
                status: 200,
                body: serde_json::to_vec(&AuthStartResponseV1 {
                    version: Version1,
                    flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
                    authorization_url: "https://accounts.google.com/authorize".into(),
                    expires_at_millis: 2_000_000_000_000,
                })
                .unwrap(),
            });
        }
        if request.path_and_query.ends_with("/native/exchange") {
            let mut exchanges = self
                .exchanges
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            exchanges.push(request.body);
            if self.lose_first && exchanges.len() == 1 {
                return Err(TransportError::Unavailable);
            }
            return Ok(HttpResponse {
                status: 200,
                body: serde_json::to_vec(&NativeExchangeResponseV1 {
                    version: Version1,
                    tokens: seeded_tokens("ack-loss-refresh"),
                    account: AccountSummaryV1 {
                        version: Version1,
                        koru_identity_id: KoruIdentityId::from_str(
                            "30000000-0000-4000-8000-000000000003",
                        )
                        .unwrap(),
                        display_name: "Seeded owner".into(),
                        auth_identities: Vec::new(),
                        terms_version_at_creation: "test".into(),
                        created_at_millis: 1,
                    },
                })
                .unwrap(),
            });
        }
        Err(TransportError::InvalidRequest)
    }
}

fn signed_descriptor() -> SignedDeviceDescriptorV1 {
    let signing = DeviceSigningKey::from_secret_bytes([42; 32]);
    let endpoint = ReplicationEndpointSecret::from_secret_bytes([43; 32]);
    SignedDeviceDescriptorV1::try_from(DeviceDescriptorV1::new(
        DeviceId::parse("a6258102-2534-4fab-bd4d-40717b066f29").unwrap(),
        "test device".into(),
        &signing,
        endpoint.endpoint_id(),
        1,
    ))
    .unwrap()
}

struct EnrollmentRetryProbe(AtomicUsize);
impl EnrollmentAccountControl for EnrollmentRetryProbe {
    fn activate_owned(
        &self,
        _: Option<crate::enrollment::EnrollmentAuthorization>,
        _: u64,
    ) -> bool {
        false
    }
    fn refresh_authorization(
        &self,
        _: crate::enrollment::EnrollmentAuthorization,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> bool {
        false
    }
    fn set_foreground(&self, _: bool) {}
    fn wake(&self) -> bool {
        false
    }
    fn snapshot(&self) -> crate::enrollment::Snapshot {
        crate::enrollment::Snapshot::default()
    }
    fn cancel_durable_for(
        &self,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> Result<(), &'static str> {
        self.cancel_durable()
    }
    fn select_profiles(
        &self,
        _: std::collections::BTreeSet<ProfileId>,
        _: crate::enrollment::EnrollmentAuthorization,
    ) -> Result<crate::enrollment::Snapshot, &'static str> {
        Ok(crate::enrollment::Snapshot::default())
    }

    fn update_authorization(&self, _: crate::enrollment::EnrollmentAuthorization) -> bool {
        true
    }

    fn retry(&self, _: crate::enrollment::EnrollmentAuthorization, _: Option<ProfileId>) -> bool {
        self.0.fetch_add(1, Ordering::AcqRel);
        true
    }

    fn cancel_durable(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

#[test]
fn enrollment_retry_wakes_coordinator_without_refreshing_hosted_session() {
    let probe = Arc::new(EnrollmentRetryProbe(AtomicUsize::new(0)));
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    )
    .with_enrollment_driver(probe.clone());
    let authority = enrollment_sets::authority_a();
    let restored = SessionCredential::new(authority.session_id.clone(), "refresh".into()).unwrap();
    enrollment_sets::acknowledge(&mut service, authority);
    let outcome = service
        .execute(
            91,
            AccountCommand::RetryEnrollment { profile_id: None },
            Some(&restored),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();

    assert_eq!(probe.0.load(Ordering::Acquire), 1);
    assert_eq!(outcome.state.stage, AccountStage::SignedIn);
    assert!(matches!(outcome.credential, CredentialChange::Keep));
}
#[test]
fn sign_in_with_apple_opens_apple_and_finishes_through_the_same_loopback_and_vault() {
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    let store = Arc::new(SessionCredentialStore::default());
    let entry = entry();
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(service, store.clone(), entry.clone(), move |event| {
        sent.send(event).unwrap();
    });
    let generation = control
        .enqueue(
            74,
            AccountCommand::StartApple {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let mut opened = None;
    let mut completed = false;
    while Instant::now() < until && !completed {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
            match event {
                WorkerEvent::Browser {
                    requester: 74,
                    generation: seen,
                    url,
                    ..
                } if seen == generation => opened = Some(url.0),
                WorkerEvent::Result {
                    requester: 74,
                    generation: seen,
                    result: AccountOperationResult::SignedIn,
                    ..
                } if seen == generation => completed = true,
                _ => {}
            }
        }
    }
    assert_eq!(
        opened.as_deref(),
        Some("https://appleid.apple.com/auth/authorize")
    );
    assert!(completed);
    let record = SessionCredential::decode(&store.load(&entry).unwrap().unwrap()).unwrap();
    assert_eq!(record.refresh_token, "refresh-vault-only");
}

#[test]
fn composed_native_flow_uses_real_client_loopback_worker_and_vault() {
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    let store = Arc::new(SessionCredentialStore::default());
    let entry = entry();
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(service, store.clone(), entry.clone(), move |event| {
        sent.send(event).unwrap();
    });
    let generation = control
        .enqueue(
            73,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let mut browser = false;
    let mut completed = false;
    while Instant::now() < until && !completed {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
            match event {
                WorkerEvent::Browser {
                    requester: 73,
                    generation: seen,
                    ..
                } if seen == generation => browser = true,
                WorkerEvent::Result {
                    requester: 73,
                    generation: seen,
                    result: AccountOperationResult::SignedIn,
                    ..
                } if seen == generation => completed = true,
                _ => {}
            }
        }
    }
    assert!(browser);
    assert!(completed);
    let list_generation = control.enqueue(73, AccountCommand::ListProfiles).unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().profile_count != 1 && Instant::now() < until {
        if let Ok(WorkerEvent::Result { generation, .. }) =
            events.recv_timeout(Duration::from_millis(20))
        {
            if generation == list_generation {
                break;
            }
        }
    }
    assert_eq!(control.state().profile_count, 1);
    let record = SessionCredential::decode(&store.load(&entry).unwrap().unwrap()).unwrap();
    assert_eq!(record.refresh_token, "refresh-vault-only");
}

#[test]
fn composed_collision_reuses_one_port_and_original_exchange_binding() {
    let transport = Arc::new(CollisionFlowTransport::new(false));
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    let store = Arc::new(SessionCredentialStore::default());
    let entry = entry();
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(service, store.clone(), entry.clone(), move |event| {
        sent.send(event).unwrap();
    });
    let generation = control
        .enqueue(
            81,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let mut browser_attempts = Vec::new();
    let mut signed_in = false;
    while Instant::now() < until && !signed_in {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(20)) {
            match event {
                WorkerEvent::Browser {
                    requester: 81,
                    generation: seen,
                    attempt,
                    ..
                } if seen == generation => browser_attempts.push(attempt),
                WorkerEvent::Result {
                    requester: 81,
                    generation: seen,
                    result: AccountOperationResult::SignedIn,
                    ..
                } if seen == generation => signed_in = true,
                _ => {}
            }
        }
    }
    assert_eq!(browser_attempts, [1, 2]);
    assert!(signed_in);
    assert!(transport.exchanged.load(Ordering::Acquire));
    let record = SessionCredential::decode(&store.load(&entry).unwrap().unwrap()).unwrap();
    assert_eq!(record.refresh_token, "collision-refresh");
}

#[test]
fn composed_collision_account_mismatch_stops_after_stage_two() {
    let transport = Arc::new(CollisionFlowTransport::new(true));
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        service,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        move |event| sent.send(event).unwrap(),
    );
    let generation = control
        .enqueue(
            82,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let mut failure = None;
    while Instant::now() < until && failure.is_none() {
        if let Ok(WorkerEvent::Result {
            requester: 82,
            generation: seen,
            result: AccountOperationResult::Failed { code },
            ..
        }) = events.recv_timeout(Duration::from_millis(20))
        {
            if seen == generation {
                failure = Some(code);
            }
        }
    }
    assert_eq!(failure.as_deref(), Some("authentication_failed"));
    assert!(!transport.exchanged.load(Ordering::Acquire));
}

#[test]
fn composed_loopback_wait_is_cancelled_out_of_band() {
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NoCallbackTransport)),
        signed_descriptor(),
        "test".into(),
    );
    let (sent, events) = mpsc::channel();
    let control = AccountControl::start(
        service,
        Arc::new(SessionCredentialStore::default()),
        entry(),
        move |event| sent.send(event).unwrap(),
    );
    let generation = control
        .enqueue(
            83,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    let mut browser_seen = false;
    while Instant::now() < until && !browser_seen {
        browser_seen = matches!(
            events.recv_timeout(Duration::from_millis(20)),
            Ok(WorkerEvent::Browser {
                requester: 83,
                generation: seen,
                ..
            }) if seen == generation
        );
    }
    assert!(browser_seen);
    control
        .enqueue(83, AccountCommand::Cancel { generation })
        .unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    let mut cancelled = false;
    while Instant::now() < until && !cancelled {
        cancelled = matches!(
            events.recv_timeout(Duration::from_millis(20)),
            Ok(WorkerEvent::Result {
                requester: 83,
                generation: seen,
                result: AccountOperationResult::Cancelled,
                ..
            }) if seen == generation
        );
    }
    assert!(cancelled);
}

#[test]
fn exchange_ack_loss_retries_exact_request_without_a_second_browser_start() {
    let transport = Arc::new(AckLossExchangeTransport::new(true));
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let command = AccountCommand::StartGoogle {
        locale: "en-US".into(),
    };
    let mut browsers = 0;
    let first = service.execute(91, command.clone(), None, &cancel, &mut |progress| {
        if matches!(progress, ServiceProgress::Browser(_)) {
            browsers += 1;
        }
    });
    assert!(matches!(
        first,
        Err(ServiceError {
            retryable: true,
            ..
        })
    ));
    let second = service
        .execute(91, command, None, &cancel, &mut |progress| {
            if matches!(progress, ServiceProgress::Browser(_)) {
                browsers += 1;
            }
        })
        .unwrap();
    assert!(matches!(second.credential, CredentialChange::Replace(_)));
    assert_eq!(browsers, 1);
    assert_eq!(transport.starts.load(Ordering::Acquire), 1);
    let exchanges = transport
        .exchanges
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(exchanges.len(), 2);
    assert_eq!(exchanges[0], exchanges[1]);
    let request: NativeExchangeRequestV1 = serde_json::from_slice(&exchanges[0]).unwrap();
    let proof_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(request.device_ownership_proof.unwrap())
        .unwrap();
    let proof: koru_account_core::DeviceOwnershipProof =
        serde_json::from_slice(&proof_bytes).unwrap();
    assert!(proof.verify_signature());
}

struct FailOnceStore {
    failed: AtomicBool,
    inner: SessionCredentialStore,
}
impl CredentialStore for FailOnceStore {
    fn load(
        &self,
        entry: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, Error> {
        self.inner.load(entry)
    }
    fn store(
        &self,
        entry: &CredentialEntry,
        secret: koru_oauth_credentials::SecretString,
    ) -> Result<(), Error> {
        if !self.failed.swap(true, Ordering::AcqRel) {
            return Err(Error::Unavailable);
        }
        self.inner.store(entry, secret)
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), Error> {
        self.inner.clear(entry)
    }
}

#[test]
fn vault_ack_loss_retry_reuses_exchange_without_a_second_browser() {
    let transport = Arc::new(AckLossExchangeTransport::new(false));
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    let store = Arc::new(FailOnceStore {
        failed: AtomicBool::new(false),
        inner: SessionCredentialStore::default(),
    });
    let entry = entry();
    let control = AccountControl::start(service, store.clone(), entry.clone(), |_| {});
    control
        .enqueue(
            92,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while control.state().failure_code.as_deref() != Some("credential_unavailable")
        && Instant::now() < until
    {
        std::thread::yield_now();
    }
    control.enqueue(92, AccountCommand::Retry).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while control.state().stage != AccountStage::SignedIn && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::SignedIn);
    assert_eq!(transport.starts.load(Ordering::Acquire), 1);
    let exchanges = transport
        .exchanges
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(exchanges.len(), 1);
    assert!(store.load(&entry).unwrap().is_some());
}

#[test]
fn rotated_refresh_is_replayed_exactly_until_vault_write_succeeds() {
    let transport = Arc::new(RecordingRefreshTransport(Mutex::new(Vec::new())));
    let service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport.clone()),
        signed_descriptor(),
        "test".into(),
    );
    let entry = entry();
    let inner = SessionCredentialStore::default();
    inner
        .store(
            &entry,
            SessionCredential::new(
                "20000000-0000-4000-8000-000000000002".into(),
                "presented-refresh".into(),
            )
            .unwrap()
            .encode()
            .unwrap(),
        )
        .unwrap();
    let store = Arc::new(FailOnceStore {
        failed: AtomicBool::new(false),
        inner,
    });
    let control = AccountControl::start(service, store.clone(), entry.clone(), |_| {});
    let until = Instant::now() + Duration::from_secs(3);
    while control.state().stage != AccountStage::SignedIn && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::SignedIn);
    let requests = transport
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert_eq!(requests.len(), 1);
    let record = SessionCredential::decode(&store.load(&entry).unwrap().unwrap()).unwrap();
    assert_eq!(record.refresh_token, "rotated-after-vault-retry");
}

#[test]
fn cancel_and_logout_abandon_correlated_pending_mutations() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    let first_profile = ProfileId::parse("40000000-0000-4000-8000-000000000004").unwrap();
    service.pending_link = Some(Pending {
        command: AccountCommand::AddGoogle {
            profile_id: first_profile.to_string(),
            locale: "en-US".into(),
        },
        request: LinkGoogleContinueRequestV1 {
            version: Version1,
            flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
            proof_grant: OneUseGrant("seeded-proof".into()),
        },
    });
    service.cancelled(11);
    assert!(service.pending_link.is_none());

    service.pending_rebind = Some(Pending {
        command: AccountCommand::RebindProfile {
            profile_id: first_profile.to_string(),
            erased_identity_id: "30000000-0000-4000-8000-000000000003".into(),
        },
        request: RebindProfileRequestV1 {
            version: Version1,
            profile_id: first_profile,
            erased_koru_identity_id: KoruIdentityId::from_str(
                "30000000-0000-4000-8000-000000000003",
            )
            .unwrap(),
            fresh_auth_grant: OneUseGrant("seeded-proof".into()),
            explicit_rebind: true,
            name_snapshot: None,
            locale_snapshot: None,
        },
    });
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let failure = match service.execute(12, AccountCommand::Logout, None, &cancel, &mut |_| {}) {
        Err(failure) => failure,
        Ok(_) => panic!("logout without a restored session must be revoked"),
    };
    assert_eq!(failure.code, "session_revoked");
    assert!(failure.clear_credential);
    assert!(service.pending_exchange.is_none());
    assert!(service.pending_link.is_none());
    assert!(service.pending_rebind.is_none());
}

fn callback(query: &str) -> Callback {
    let listener =
        CallbackListener::bind(koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH).unwrap();
    let port = listener.local_addr().port();
    let query = query.to_owned();
    std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH,
            query
        )
        .unwrap();
    });
    listener
        .wait(
            &WaitOptions {
                expected_state: "seeded",
                timeout: Duration::from_secs(1),
                poll: Duration::from_millis(1),
                max_request_bytes: 4096,
                success_html: "",
                error_html: "",
            },
            &|| false,
        )
        .map(convert_callback)
        .unwrap()
}

#[test]
fn browser_and_callback_security_fields_are_exact_and_bounded() {
    assert!(checked_browser_url("https://accounts.google.com/o/oauth2/auth".into()).is_ok());
    assert!(checked_browser_url("https://appleid.apple.com/auth/authorize".into()).is_ok());
    for url in [
        "https://appleid.apple.com.evil.invalid/auth/authorize",
        "http://appleid.apple.com/auth/authorize",
        "https://accounts.google.com.evil.invalid/auth",
        "https://accounts.google.com:444/auth",
        "https://user@accounts.google.com/auth",
    ] {
        assert_eq!(
            checked_browser_url(url.into()).unwrap_err().code,
            "invalid_browser_url"
        );
    }
    assert_eq!(
        checked_browser_url(format!("https://accounts.google.com/{}", "x".repeat(8192)))
            .unwrap_err()
            .code,
        "invalid_browser_url"
    );
    let valid = callback(
        "state=seeded&outcome=completed&grant=grant&return_target=account&expires_at_millis=2000000000000",
    );
    assert!(matches!(
        parse_callback(&valid, false),
        Ok(ParsedCallback::Completed(_))
    ));
    for query in [
        "state=seeded&outcome=completed&grant=grant&grant=other&return_target=account&expires_at_millis=2000000000000",
        "state=seeded&outcome=completed&grant=grant&return_target=account&expires_at_millis=1",
        "state=seeded&outcome=completed&grant=grant&return_target=account&expires_at_millis=2000000000000&authorization_url=https%3A%2F%2Faccounts.google.com",
    ] {
        assert!(parse_callback(&callback(query), false).is_err());
    }
    let rejected = callback("state=seeded&outcome=account_mismatch");
    assert!(matches!(
        parse_callback(&rejected, false),
        Ok(ParsedCallback::Rejected)
    ));
}

struct SessionIdentityTransport;
impl Transport for SessionIdentityTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let koru_identity_id =
            KoruIdentityId::from_str("30000000-0000-4000-8000-000000000003").unwrap();
        let authenticated_by =
            AuthIdentityId::from_str("40000000-0000-4000-8000-000000000004").unwrap();
        let body = if request.path_and_query.ends_with("/session") {
            serde_json::to_vec(&SessionSummaryV1 {
                version: Version1,
                session_id: AccountSessionId::from_str("20000000-0000-4000-8000-000000000002")
                    .unwrap(),
                koru_identity_id,
                auth_identity_id: authenticated_by,
                device_id: None,
                expires_at_millis: 2_000_000_000_000,
                recent_auth_at_millis: 1,
            })
            .unwrap()
        } else if request.path_and_query.ends_with("/account") {
            serde_json::to_vec(&AccountSummaryV1 {
                version: Version1,
                koru_identity_id,
                display_name: "Seeded owner".into(),
                auth_identities: vec![
                    AuthIdentitySummaryV1 {
                        version: Version1,
                        auth_identity_id: AuthIdentityId::from_str(
                            "50000000-0000-4000-8000-000000000005",
                        )
                        .unwrap(),
                        provider: koru_account_core::AuthProvider::Google,
                        verified_email_snapshot: Some("decoy@example.test".into()),
                    },
                    AuthIdentitySummaryV1 {
                        version: Version1,
                        auth_identity_id: authenticated_by,
                        provider: koru_account_core::AuthProvider::Google,
                        verified_email_snapshot: Some("signed-in@example.test".into()),
                    },
                ],
                terms_version_at_creation: "test".into(),
                created_at_millis: 1,
            })
            .unwrap()
        } else {
            return Err(TransportError::InvalidRequest);
        };
        Ok(HttpResponse { status: 200, body })
    }
}

#[test]
fn inspect_session_uses_exact_authenticated_identity_not_a_profile_or_first_email() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(SessionIdentityTransport)),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("access-memory-only".into()));
    service.access_expires_at_millis = Some(2_000_000_000_000);
    let restored = SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "refresh-vault-only".into(),
    )
    .unwrap();
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let outcome = service
        .execute(
            9,
            AccountCommand::InspectAccountSession,
            Some(&restored),
            &cancel,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(
        outcome.result,
        AccountOperationResult::SignedInIdentity {
            provider: "google".into(),
            verified_email: "signed-in@example.test".into(),
        }
    );

    let mut signed_out = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(SessionIdentityTransport)),
        signed_descriptor(),
        "test".into(),
    );
    let error = match signed_out.execute(
        10,
        AccountCommand::InspectAccountSession,
        None,
        &cancel,
        &mut |_| {},
    ) {
        Ok(_) => panic!("signed-out inspection returned an identity"),
        Err(error) => error,
    };
    assert_eq!(error.code, "authentication_required");
}

#[test]
fn callback_timeout_maps_to_a_stable_terminal_result() {
    let mut listener = CallbackListener::bind(koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH)
        .unwrap()
        .into_two_stage();
    let error = listener
        .wait_stage(
            &WaitOptions {
                expected_state: "seeded",
                timeout: Duration::from_millis(5),
                poll: Duration::from_millis(1),
                max_request_bytes: 4096,
                success_html: "",
                error_html: "",
            },
            &|| false,
        )
        .unwrap_err();
    assert_eq!(map_callback_error(error).code, "callback_timeout");
}

#[test]
fn ipc_text_is_bounded_before_any_hosted_call() {
    assert_eq!(validate_locale("").unwrap_err().code, "invalid_locale");
    assert_eq!(
        validate_locale(&"x".repeat(129)).unwrap_err().code,
        "invalid_locale"
    );
    assert_eq!(
        validate_name("\n").unwrap_err().code,
        "invalid_profile_name"
    );
    assert_eq!(
        validate_name(&"x".repeat(513)).unwrap_err().code,
        "invalid_profile_name"
    );
}

#[test]
fn expired_access_refreshes_and_persists_before_follow_up() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("expired".into()));
    service.access_expires_at_millis = Some(1);
    let restored = SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "presented-refresh".into(),
    )
    .unwrap();
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let outcome = service
        .execute(
            8,
            AccountCommand::ListProfiles,
            Some(&restored),
            &cancel,
            &mut |_| {},
        )
        .unwrap();
    assert!(matches!(outcome.credential, CredentialChange::Replace(_)));
    assert!(matches!(
        outcome.follow_up,
        Some(AccountCommand::ListProfiles)
    ));
}

#[test]
fn pending_proof_refreshes_access_before_exact_replay() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("expired".into()));
    service.access_expires_at_millis = Some(1);
    service.pending_link = Some(Pending {
        command: AccountCommand::AddGoogle {
            profile_id: "40000000-0000-4000-8000-000000000004".into(),
            locale: "en-US".into(),
        },
        request: LinkGoogleContinueRequestV1 {
            version: Version1,
            flow_id: AuthFlowId::from_str("10000000-0000-4000-8000-000000000001").unwrap(),
            proof_grant: OneUseGrant("seeded-proof".into()),
        },
    });
    let restored = SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "presented-refresh".into(),
    )
    .unwrap();
    let outcome = service
        .execute(
            13,
            AccountCommand::Retry,
            Some(&restored),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();
    assert!(matches!(outcome.credential, CredentialChange::Replace(_)));
    assert_eq!(outcome.follow_up, Some(AccountCommand::Retry));
    assert!(service.pending_link.is_some());
}

struct RevokedRefreshTransport;
impl Transport for RevokedRefreshTransport {
    fn send(&self, _: HttpRequest) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status: 401,
            body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
                retry_after_millis: None,
                code: koru_account_wire::ErrorCode::parse(
                    koru_account_wire::ErrorCode::UNAUTHENTICATED,
                )
                .unwrap(),
                message: "revoked".into(),
            })
            .unwrap(),
        })
    }
}

#[test]
fn known_refresh_revocation_disables_inbound_enrollment() {
    let session = Arc::new(crate::enrollment::ResponderSessionGate::new());
    let account = "10000000-0000-4000-8000-000000000001".parse().unwrap();
    session.enable_account(account, "20000000-0000-4000-8000-000000000002".into());
    let other = "10000000-0000-4000-8000-000000000002".parse().unwrap();
    session.enable_account(other, "20000000-0000-4000-8000-000000000003".into());
    let other_ticket = session.ticket(other).unwrap();
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(RevokedRefreshTransport)),
        signed_descriptor(),
        "test".into(),
    )
    .with_responder_session(session.clone());
    let restored = SessionCredential::new(
        "20000000-0000-4000-8000-000000000002".into(),
        "revoked-refresh".into(),
    )
    .unwrap();

    service.durable_owner = Some(crate::identity::AccountOwner {
        account,
        auth_identity: "30000000-0000-4000-8000-000000000001".parse().unwrap(),
    });
    service.durable_session_id = Some(restored.session_id.clone());

    assert_eq!(
        service.refresh(&restored).unwrap_err().code,
        "session_revoked"
    );
    assert!(session.ticket(account).is_err());
    assert!(
        session.ensure(other_ticket).is_ok(),
        "A's revoked refresh leaves B eligible"
    );
}

struct RevokedRefresh;
impl AccountService for RevokedRefresh {
    fn execute(
        &mut self,
        _: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        Err(ServiceError::revoked())
    }
}

#[test]
fn terminal_refresh_replay_clears_stale_vault_record() {
    let entry = entry();
    let store = Arc::new(SessionCredentialStore::default());
    store
        .store(
            &entry,
            SessionCredential::new("session".into(), "refresh".into())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    let control = AccountControl::start(RevokedRefresh, store.clone(), entry.clone(), |_| {});
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().stage != AccountStage::SignedOut && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(
        control.state().failure_code.as_deref(),
        Some("session_revoked")
    );
    assert!(store.load(&entry).unwrap().is_none());
}

#[test]
fn revoked_refresh_retries_required_vault_erasure_before_finishing() {
    let entry = entry();
    let inner = SessionCredentialStore::default();
    inner
        .store(
            &entry,
            SessionCredential::new("session".into(), "revoked-refresh".into())
                .unwrap()
                .encode()
                .unwrap(),
        )
        .unwrap();
    let store = Arc::new(ClearFailsOnce {
        failed: AtomicBool::new(false),
        inner,
    });
    let control = AccountControl::start(RevokedRefresh, store.clone(), entry.clone(), |_| {});
    let until = Instant::now() + Duration::from_secs(1);
    while control.state().failure_code.as_deref() != Some("credential_unavailable")
        && Instant::now() < until
    {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::Failed);
    assert!(store.load(&entry).unwrap().is_some());
    let until = Instant::now() + Duration::from_secs(3);
    while control.state().stage != AccountStage::SignedOut && Instant::now() < until {
        std::thread::yield_now();
    }
    assert_eq!(control.state().stage, AccountStage::SignedOut);
    assert!(store.load(&entry).unwrap().is_none());
}

struct PreconditionErrorTransport(&'static str);
impl Transport for PreconditionErrorTransport {
    fn send(&self, _: HttpRequest) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status: 428,
            body: serde_json::to_vec(&koru_account_wire::DeveloperErrorV1 {
                retry_after_millis: None,
                code: koru_account_wire::ErrorCode::parse(self.0).unwrap(),
                message: "backend-pii-sentinel@example.test".into(),
            })
            .unwrap(),
        })
    }
}

struct DeleteReplayTransport {
    attempts: AtomicUsize,
    operations: Mutex<Vec<koru_account_core::ProfileLifecycleMutationId>>,
    fail_first: bool,
    cancel_after_commit: Option<Arc<AtomicBool>>,
}

struct StaticInventory(koru_store_wire::ProfileLifecycleInventory);

impl lifecycle::LocalProfileInventory for StaticInventory {
    fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
        Ok(self.0.clone())
    }
}

struct ReconcileTransport {
    identity: KoruIdentityId,
    lifecycle: koru_account_core::ProfileLifecycle,
    resolved: AtomicUsize,
}

impl Transport for ReconcileTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let body = if request.path_and_query == "/v1/account" {
            serde_json::to_vec(&AccountSummaryV1 {
                version: Version1,
                koru_identity_id: self.identity,
                display_name: "PII not projected".into(),
                auth_identities: Vec::new(),
                terms_version_at_creation: "test".into(),
                created_at_millis: 1,
            })
            .unwrap()
        } else if request.path_and_query == "/v1/account/profiles/lifecycle" {
            let request: ResolveProfileLifecyclesRequestV1 = serde_json::from_slice(&request.body)
                .map_err(|_| TransportError::InvalidRequest)?;
            assert_eq!(request.profile_ids, [self.lifecycle.profile_id]);
            self.resolved.fetch_add(1, Ordering::SeqCst);
            serde_json::to_vec(&koru_account_wire::ResolveProfileLifecyclesResponseV1 {
                version: Version1,
                lifecycles: vec![self.lifecycle.clone()],
            })
            .unwrap()
        } else {
            return Err(TransportError::InvalidRequest);
        };
        Ok(HttpResponse { status: 200, body })
    }
}

#[test]
fn reconciliation_resolves_only_profiles_bound_to_the_signed_in_identity() {
    let identity = KoruIdentityId::mint();
    let profile = ProfileId::new();
    let other_account_profile = ProfileId::new();
    let operation = koru_account_core::ProfileLifecycleMutationId::mint();
    let transport = Arc::new(ReconcileTransport {
        identity,
        lifecycle: koru_account_core::ProfileLifecycle {
            profile_id: profile,
            koru_identity_id: identity,
            state: koru_account_core::ProfileLifecycleState::Deleted,
            revision: koru_account_core::ProfileLifecycleRevision::new(3),
            operation_id: Some(operation),
            lifecycle_at_millis: Some(3),
        },
        resolved: AtomicUsize::new(0),
    });
    let inventory = koru_store_wire::ProfileLifecycleInventory {
        profiles: vec![
            koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
                account_binding: format!("koru:{identity}"),
                removed_at_millis: None,
            },
            koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: koru_store_core::ProfileId::from_uuid(other_account_profile.as_uuid()),
                account_binding: format!("koru:{}", KoruIdentityId::mint()),
                removed_at_millis: None,
            },
        ],
    };
    let mut service = deletion_service(transport.clone())
        .with_local_profile_inventory(Arc::new(StaticInventory(inventory)));
    let outcome = service
        .execute(
            1,
            AccountCommand::ReconcileProfileLifecycles,
            None,
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();
    let AccountOperationResult::ProfileLifecycleReconciliation { profiles } = outcome.result else {
        panic!("expected reconciliation result");
    };
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].lifecycle_revision, 3);
    assert_eq!(
        profiles[0].cleanup_stage,
        Some(koru_agent_wire::ProfileCleanupStage::HostedCommitted)
    );
    assert_eq!(transport.resolved.load(Ordering::SeqCst), 1);
    assert!(
        service
            .lifecycle_repository
            .lock()
            .unwrap()
            .get(other_account_profile)
            .unwrap()
            .is_none()
    );
}

impl DeleteReplayTransport {
    fn new(fail_first: bool, cancel_after_commit: Option<Arc<AtomicBool>>) -> Self {
        Self {
            attempts: AtomicUsize::new(0),
            operations: Mutex::new(Vec::new()),
            fail_first,
            cancel_after_commit,
        }
    }
}

impl Transport for DeleteReplayTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        assert!(request.path_and_query.starts_with("/v1/account/profiles/"));
        let profile = request
            .path_and_query
            .trim_start_matches("/v1/account/profiles/");
        let profile = ProfileId::parse(profile).ok_or(TransportError::InvalidRequest)?;
        let body: DeleteProfileRequestV1 =
            serde_json::from_slice(&request.body).map_err(|_| TransportError::InvalidRequest)?;
        self.operations.lock().unwrap().push(body.operation_id);
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.fail_first && attempt == 0 {
            return Err(TransportError::Unavailable);
        }
        if let Some(cancel) = &self.cancel_after_commit {
            cancel.store(true, Ordering::Release);
        }
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&koru_account_wire::ProfileLifecycleResponseV1 {
                version: Version1,
                status: if attempt == 0 {
                    koru_account_wire::ProfileLifecycleMutationStatusV1::Applied
                } else {
                    koru_account_wire::ProfileLifecycleMutationStatusV1::ExactRetry
                },
                lifecycle: koru_account_core::ProfileLifecycle {
                    profile_id: profile,
                    koru_identity_id: KoruIdentityId::mint(),
                    state: koru_account_core::ProfileLifecycleState::Deleted,
                    revision: koru_account_core::ProfileLifecycleRevision::new(1),
                    operation_id: Some(body.operation_id),
                    lifecycle_at_millis: Some(1),
                },
            })
            .unwrap(),
        })
    }
}

fn deletion_service(transport: Arc<dyn Transport>) -> ProductionAccountService {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(transport),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("access-memory-only".into()));
    service.access_expires_at_millis = Some(2_000_000_000_000);
    service.durable_owner = Some(crate::identity::AccountOwner {
        account: "10000000-0000-4000-8000-000000000001".parse().unwrap(),
        auth_identity: "20000000-0000-4000-8000-000000000001".parse().unwrap(),
    });
    service
}

#[test]
fn delete_replays_the_durable_operation_after_an_uncertain_response() {
    let transport = Arc::new(DeleteReplayTransport::new(true, None));
    let mut service = deletion_service(transport.clone());
    let profile = ProfileId::new();
    service.profiles.push(ProfileSummaryV1 {
        version: Version1,
        koru_identity_id: service.durable_owner.unwrap().account,
        profile_id: profile,
        name_snapshot: None,
        locale_snapshot: None,
        auth_identities: vec![],
        lifecycle_revision: koru_account_core::ProfileLifecycleRevision::INITIAL,
    });
    let command = AccountCommand::DeleteProfile {
        profile_id: profile.to_string(),
        expected_lifecycle_revision: 0,
    };
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let first = match service.execute(1, command.clone(), None, &cancel, &mut |_| {}) {
        Err(error) => error,
        Ok(_) => panic!("uncertain transport must fail"),
    };
    assert_eq!(first.code, "account_unavailable");
    assert_eq!(
        service
            .lifecycle_repository
            .lock()
            .unwrap()
            .get(profile)
            .unwrap()
            .unwrap()
            .stage,
        koru_agent_wire::ProfileCleanupStage::HostedPending
    );

    let retry = service
        .execute(2, command, None, &cancel, &mut |_| {})
        .unwrap();
    assert!(matches!(
        retry.result,
        AccountOperationResult::ProfileDeleted {
            exact_retry: true,
            ..
        }
    ));
    let operations = transport.operations.lock().unwrap();
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0], operations[1]);
}

#[test]
fn cancellation_before_delete_changes_nothing_and_after_commit_keeps_the_result() {
    let before_transport = Arc::new(DeleteReplayTransport::new(false, None));
    let mut before = deletion_service(before_transport.clone());
    let profile = ProfileId::new();
    let cancelled = Cancel(Arc::new(AtomicBool::new(true)));
    let cancelled_error = match before.execute(
        1,
        AccountCommand::DeleteProfile {
            profile_id: profile.to_string(),
            expected_lifecycle_revision: 0,
        },
        None,
        &cancelled,
        &mut |_| {},
    ) {
        Err(error) => error,
        Ok(_) => panic!("pre-cancelled delete must not run"),
    };
    assert_eq!(cancelled_error.code, "cancelled");
    assert_eq!(before_transport.attempts.load(Ordering::SeqCst), 0);
    assert!(
        before
            .lifecycle_repository
            .lock()
            .unwrap()
            .get(profile)
            .unwrap()
            .is_none()
    );

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let after_transport = Arc::new(DeleteReplayTransport::new(
        false,
        Some(Arc::clone(&cancel_flag)),
    ));
    let mut after = deletion_service(after_transport);
    after.profiles.push(ProfileSummaryV1 {
        version: Version1,
        koru_identity_id: after.durable_owner.unwrap().account,
        profile_id: profile,
        name_snapshot: None,
        locale_snapshot: None,
        auth_identities: vec![],
        lifecycle_revision: koru_account_core::ProfileLifecycleRevision::INITIAL,
    });
    let completed = after
        .execute(
            2,
            AccountCommand::DeleteProfile {
                profile_id: profile.to_string(),
                expected_lifecycle_revision: 0,
            },
            None,
            &Cancel(cancel_flag),
            &mut |_| {},
        )
        .unwrap();
    assert!(matches!(
        completed.result,
        AccountOperationResult::ProfileDeleted {
            exact_retry: false,
            ..
        }
    ));
    assert_eq!(
        after
            .lifecycle_repository
            .lock()
            .unwrap()
            .get(profile)
            .unwrap()
            .unwrap()
            .stage,
        koru_agent_wire::ProfileCleanupStage::HostedCommitted
    );
}

#[test]
fn profile_authorization_preconditions_cross_agent_with_exact_codes_only() {
    for expected in [
        koru_account_wire::ErrorCode::PROFILE_OFFLINE_ONLY_CONFIRMATION_REQUIRED,
        koru_account_wire::ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED,
        koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED,
    ] {
        let client = AccountClient::with_transport(Arc::new(PreconditionErrorTransport(expected)));
        let access = AccessToken("access-sentinel".into());
        let client_error = client
            .profiles(&AccountAuthorization::Bearer(&access))
            .unwrap_err();
        assert!(!format!("{client_error:?}").contains("backend-pii-sentinel"));
        let mapped = map_client_error(client_error);
        assert_eq!(mapped.code, expected);
        assert!(!mapped.retryable);
        assert!(!mapped.clear_credential);
        assert!(!format!("{mapped:?}").contains("backend-pii-sentinel"));
    }
}

#[test]
fn fresh_google_sign_in_replaces_existing_session_before_removal_retry() {
    let mut service = ProductionAccountService::with_client(
        AccountClient::with_transport(Arc::new(NativeFlowTransport)),
        signed_descriptor(),
        "test".into(),
    );
    service.access = Some(AccessToken("old-access".into()));
    service.access_expires_at_millis = Some(2_000_000_000_000);
    let restored = SessionCredential::new(
        "90000000-0000-4000-8000-000000000009".into(),
        "old-refresh".into(),
    )
    .unwrap();
    let outcome = service
        .execute(
            91,
            AccountCommand::StartGoogle {
                locale: "en-US".into(),
            },
            Some(&restored),
            &Cancel(Arc::new(AtomicBool::new(false))),
            &mut |_| {},
        )
        .unwrap();
    let CredentialChange::Replace(replacement) = outcome.credential else {
        panic!("fresh Google sign-in must replace the stored session");
    };
    assert_eq!(
        replacement.session_id,
        "20000000-0000-4000-8000-000000000002"
    );
    assert_eq!(replacement.refresh_token, "refresh-vault-only");
    assert!(matches!(
        service.access.as_ref(),
        Some(AccessToken(value)) if value == "access-memory-only"
    ));
    assert!(matches!(outcome.result, AccountOperationResult::SignedIn));
}

use crate::delivery::SignInSession;
use crate::ports::{SystemClock, SystemEntropy};
use koru_oauth_loopback::{CallbackListener, TwoStageCallbackListener, WaitOptions};
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) fn current_platform() -> DevicePlatform {
    if cfg!(windows) {
        DevicePlatform::Windows
    } else if cfg!(target_os = "macos") {
        DevicePlatform::Macos
    } else {
        DevicePlatform::Unknown
    }
}
fn parse_callback(callback: &Callback, allow: bool) -> Result<ParsedCallback, ServiceError> {
    super::parse_callback(callback, allow, &SystemClock)
}
fn convert_callback(callback: koru_oauth_loopback::Callback) -> Callback {
    Callback::new(
        callback
            .fields()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
    )
}
struct TestDelivery;
struct TestSession {
    listener: TwoStageCallbackListener,
    port: u16,
}
impl SignInDelivery for TestDelivery {
    fn bind(&self) -> Result<Box<dyn SignInSession>, ServiceError> {
        let listener = CallbackListener::bind(koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH)
            .map_err(|_| ServiceError::terminal("callback_unavailable"))?;
        let port = listener.local_addr().port();
        Ok(Box::new(TestSession {
            listener: listener.into_two_stage(),
            port,
        }))
    }
}
impl SignInSession for TestSession {
    fn callback_port(&self) -> u16 {
        self.port
    }
    fn wait(&mut self, state: &str, cancel: &Cancel) -> Result<Callback, ServiceError> {
        self.listener
            .wait_stage(&wait_options(state), &|| cancel.cancelled())
            .map(convert_callback)
            .map_err(map_callback_error)
    }
}
impl ProductionAccountService {
    fn with_client(
        client: AccountClient,
        descriptor: SignedDeviceDescriptorV1,
        app_version: String,
    ) -> Self {
        Self::new(
            client,
            descriptor,
            koru_device::DeviceSigningKey::from_secret_bytes([42; 32]),
            current_platform(),
            app_version,
            AccountPorts {
                delivery: Arc::new(TestDelivery),
                local_profiles: Arc::new(lifecycle::EmptyProfileInventory),
                lifecycle_repository: lifecycle::LifecycleRepository::in_memory(),
                clock: Arc::new(SystemClock),
                entropy: Arc::new(SystemEntropy),
                cloud_sync: None,
            },
        )
    }
}
const CALLBACK_SUCCESS_HTML: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Return to Koru</title></head><body><p>Sign-in was returned to Koru. You can close this tab.</p><script>window.close();</script></body></html>"#;
const CALLBACK_ERROR_HTML: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Return to Koru</title></head><body><p>Koru could not accept this sign-in. Return to Koru and try again.</p></body></html>"#;
fn wait_options(state: &str) -> WaitOptions<'_> {
    WaitOptions {
        expected_state: state,
        timeout: CALLBACK_TIMEOUT,
        poll: koru_oauth_loopback::DEFAULT_POLL,
        max_request_bytes: koru_oauth_loopback::DEFAULT_MAX_REQUEST_BYTES,
        success_html: CALLBACK_SUCCESS_HTML,
        error_html: CALLBACK_ERROR_HTML,
    }
}
fn map_callback_error(error: koru_oauth_loopback::Error) -> ServiceError {
    match error.kind() {
        koru_oauth_loopback::ErrorKind::Cancelled => ServiceError::terminal("cancelled"),
        koru_oauth_loopback::ErrorKind::TimedOut => ServiceError::terminal("callback_timeout"),
        _ => ServiceError::terminal("callback_invalid"),
    }
}
