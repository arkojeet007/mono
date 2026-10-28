// SPDX-License-Identifier: MPL-2.0

//! The portable mobile host: device identity use, the local Store, iroh
//! streams, durable pairing evidence, and replication — everything a phone
//! runs, with **no platform binding in it**.
//!
//! Android's JNI shim (`crates/koru-mobile-native`) and iOS's C shim
//! (`crates/koru-mobile-ios`) are pure argument-marshalling over generated
//! Protobuf requests and responses. With no hand-written wire shape in either
//! shim, the two platforms cannot disagree about a payload by construction.
//! Historical JSON-envelope goldens remain test-only while the migration is
//! audited; they are not a callable platform boundary.
//!
//! The presentation layer above the shim — Kotlin, Swift — owns rendering and
//! the platform's secret vault; this crate expects the 64 secret bytes
//! (32 Ed25519 device seed + 32 iroh endpoint seed) already unwrapped.

use std::collections::HashMap;
#[cfg(target_os = "android")]
use std::ffi::CString;
#[cfg(target_os = "android")]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use koru_contract::mobile_proto as wire;
use koru_delegate_document::{RUN_KIND, RunDocument, RunDocumentError, SCHEDULE_KIND};
use koru_device::{DeviceId, TrustRecord};
use koru_replication::protocol::{
    AuthenticatedPeer, BlobChunk, DocumentReplicaState, DurableAck, ProfileReplicaState,
    ReplicaDescriptor, ReplicationStore, StoreFailure, WorkspaceCatalogState, WorkspaceInventory,
};
use koru_store_core::{DocumentId, ProfileId, Timestamp, WorkspaceId};
use serde::{Deserialize, Serialize};

pub mod account;
mod blob;
mod document_edit;
pub mod document_observer;
pub mod protobuf;
mod run_view;

use run_view::{DeviceNames, RUN_PILL_ROLE, RunView, TimelineWindow};

/// Activate account services using trusted application configuration, after
/// this root's trust migration succeeds. Platform adapters call this before
/// serving the account SDK/vault prompts. Product commands cannot configure
/// an origin, issuer or Google client ID. Repeated identical configuration is
/// harmless; changing it requires a new host lifetime.
pub fn configure_account(
    root: PathBuf,
    secrets: [u8; 64],
    device_name: &str,
    relay_enabled: bool,
    configuration: account::TrustedAccountConfig,
) -> Result<(), String> {
    let host = get_host(root, secrets, device_name, relay_enabled)?;
    account::activate(&host, configuration).map_err(str::to_owned)
}
mod trust;
use trust::TrustState;

static HOSTS: LazyLock<Mutex<HashMap<PathBuf, Arc<MobileHost>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(target_os = "android")]
#[derive(Default)]
struct AndroidLogWriter(Vec<u8>);

#[cfg(target_os = "android")]
impl AndroidLogWriter {
    fn emit(&mut self) {
        while self
            .0
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            self.0.pop();
        }
        if self.0.is_empty() {
            return;
        }
        let line = String::from_utf8_lossy(&self.0).replace('\0', "�");
        let Ok(line) = CString::new(line.as_bytes()) else {
            self.0.clear();
            return;
        };
        const ANDROID_LOG_INFO: std::ffi::c_int = 4;
        const TAG: &[u8] = b"KoruMobile\0";
        unsafe {
            __android_log_write(ANDROID_LOG_INFO, TAG.as_ptr().cast(), line.as_ptr());
        }
        self.0.clear();
    }
}

#[cfg(target_os = "android")]
impl Write for AndroidLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.emit();
        Ok(())
    }
}

#[cfg(target_os = "android")]
impl Drop for AndroidLogWriter {
    fn drop(&mut self) {
        self.emit();
    }
}

#[cfg(target_os = "android")]
#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(
        priority: std::ffi::c_int,
        tag: *const std::ffi::c_char,
        text: *const std::ffi::c_char,
    ) -> std::ffi::c_int;
}

/// Route the replication runtime's tracing into the platform console. Android
/// writes it under the existing `KoruMobile` logcat tag and Apple captures
/// native stderr in the Xcode/device console. Without this, a phone that cannot
/// pull a document fails invisibly.
/// The phone's log filter. **One line, no whitespace**: this was once
/// written across three source lines with `\n` and indentation inside the
/// literal, and the two directives that followed a newline
/// (`koru_replication`, `koru_network_iroh`) never took effect. Every
/// `info` line from peer hydration and Cloud transfer was missing from
/// every phone log, and the silence was read as the code not running.
/// The test below refuses whitespace so it cannot come back.
const LOG_FILTER: &str = "warn,noq_proto::connection=error,iroh::address_lookup::pkarr=error,\
    koru_replication=info,koru_store_core=info,koru_mobile=info,\
    koru_network_iroh=info,koru_blob_sync=info";

#[cfg(test)]
mod log_filter_tests {
    #[test]
    fn the_log_filter_names_every_koru_target_without_whitespace() {
        assert!(
            !super::LOG_FILTER.contains(char::is_whitespace),
            "a directive after whitespace is silently dead"
        );
        assert!(tracing_subscriber::EnvFilter::try_new(super::LOG_FILTER).is_ok());
        for target in [
            "koru_replication=info",
            "koru_network_iroh=info",
            "koru_blob_sync=info",
            "koru_store_core=info",
            "koru_mobile=info",
        ] {
            assert!(
                super::LOG_FILTER.split(',').any(|d| d == target),
                "{target}"
            );
        }
    }
}

fn init_logging() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter =
            // Store core at info carries the storage-lifecycle facts a phone
            // cannot otherwise show — "checkpoint taken" above all.
            // noq-proto's idle-path timer reports MultipathNotNegotiated and
            // LastOpenPath at warn even though both mean the single usable
            // path was retained. iroh's pkarr publisher warns on every retry
            // ("Failed to publish to pkarr", every few seconds while the
            // phone has no route) — that is activity, not news, so only that
            // module is demoted; every other iroh warning still lands.
            // Preserve dependency errors and Koru warnings.
            //
            // `koru_blob_sync` is here because it was not: a transfer's
            // three ordinary outcomes — the development origin installed,
            // absent, or a pass finding nothing to do — are all `info`,
            // and a target missing from this list is a target at `warn`.
            // So a phone refusing its own stack's byte URL printed the
            // refusal and nothing that would explain it, and the silence
            // was read three times as proof the code had not run. A log
            // line nobody can see is worse than none: it is believed.
            LOG_FILTER;
        #[cfg(target_os = "android")]
        let _ = tracing_subscriber::fmt()
            .with_writer(AndroidLogWriter::default)
            .with_env_filter(filter)
            .try_init();
        #[cfg(not(target_os = "android"))]
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .try_init();
    });
}

/// A whole trust store is dozens of small records, so replicating the device
/// list needs no delta machinery: a revision change sends the full set, and
/// this beat heals whatever a disconnect dropped.
const TRUST_GOSSIP_ANTIENTROPY: Duration = Duration::from_secs(5 * 60);

#[derive(Serialize)]
struct LinkResult<'a> {
    state: &'a str,
    peer_name: Option<String>,
    error_code: Option<String>,
}

#[derive(Serialize)]
struct PairingOfferResult {
    state: &'static str,
    qr_payload: Option<String>,
    expires_in_seconds: u16,
    error_code: Option<String>,
}

#[derive(Serialize, Default)]
struct DevicesResult {
    devices: Vec<LinkedDeviceResult>,
    /// This device itself, so the list can lead with it and offer rename.
    #[serde(skip_serializing_if = "Option::is_none")]
    this_device: Option<LinkedDeviceResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_event: Option<MobileSyncEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    /// The relay connection as the network runtime reads it now — the mobile
    /// mirror of the desktop tray's status row, carried on the same query the
    /// Devices screen already polls. Absent only when the host could not be
    /// reached at all (the error path).
    #[serde(skip_serializing_if = "Option::is_none")]
    relay: Option<RelayStatusResult>,
    /// No effective peer exists, or signed local revocation history still has
    /// a peer without a fresh direct relationship. Derived from trust on every
    /// read, never UI state.
    relink_available: bool,
}

/// The relay connection split into the wire words. `state` is one of
/// `"off"` | `"connecting"` | `"connected"` | `"disconnected"`; `host` is
/// empty for `"off"` (the switch already says which switch) and when no
/// relay has been named yet.
#[derive(Serialize, Debug, PartialEq, Eq)]
struct RelayStatusResult {
    state: &'static str,
    host: String,
}

fn relay_status(value: koru_network_iroh::RelayConnectionStatus) -> RelayStatusResult {
    use koru_network_iroh::RelayConnectionStatus as Status;
    let (state, host) = match value {
        Status::Off => ("off", String::new()),
        Status::Connecting { host } => ("connecting", host),
        Status::Connected { host } => ("connected", host),
        Status::Disconnected { host } => ("disconnected", host),
    };
    RelayStatusResult { state, host }
}

/// A fresh signed-in device with no effective peer can enroll without QR. For
/// an established mesh, relink is owed only for a peer this device explicitly
/// revoked and has not since trusted through a fresh direct relationship. A
/// transitive route can keep content moving through another device, but it
/// does not repair the relationship the user asked to restore.
fn has_unrestored_local_relationship(
    view: &koru_device::TrustView,
    authorized: &[koru_device::AuthorizedDevice],
) -> bool {
    if authorized.is_empty() {
        return true;
    }
    let local = view.local_device.device_id;
    view.revocations.iter().any(|revocation| {
        revocation.issued_by == local
            && !authorized
                .iter()
                .any(|peer| peer.device_id == revocation.revoked_device_id && peer.via.is_none())
    })
}

#[derive(Serialize)]
struct MobileSyncEvent {
    sequence: u64,
    at_millis: i64,
    direction: &'static str,
    /// The device this stage is about. Sync passes from several peers
    /// interleave in one log, and without it a workspace that "never
    /// completes" cannot be attributed to the connection that died.
    peer: String,
    stage: &'static str,
    workspace_id: Option<String>,
    items: Option<usize>,
    error_code: Option<String>,
}

#[derive(Serialize)]
struct LinkedDeviceResult {
    device_id: String,
    display_name: String,
    linked_at_millis: i64,
    connected: bool,
    last_seen_millis: Option<i64>,
    /// The directly paired device this one arrived through, when there is no
    /// local pairing with it. `None`/absent for a directly linked device.
    #[serde(skip_serializing_if = "Option::is_none")]
    introduced_by: Option<String>,
    /// `version · channel · branch · OS`, as the device's latest self-signed
    /// build record puts it; absent for a device that has never said.
    #[serde(skip_serializing_if = "Option::is_none")]
    build_label: Option<String>,
    /// Semver drift from this device: "same" | "patch" | "minor" | "major" |
    /// "incompatible" | "unknown"; absent when either side has not said.
    #[serde(skip_serializing_if = "Option::is_none")]
    build_drift: Option<String>,
    /// "warning" (a minor apart) or "danger" (a major apart, or incompatible).
    #[serde(skip_serializing_if = "Option::is_none")]
    build_severity: Option<String>,
    /// Only when incompatible: "peer" or "this_device" — who has to update.
    #[serde(skip_serializing_if = "Option::is_none")]
    update_needed_on: Option<String>,
    /// `"direct"` or `"relay"` while connected and the path is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_sync_millis: Option<i64>,
    history: Vec<MobileStateEvent>,
}

#[derive(Serialize)]
struct MobileStateEvent {
    event_id: Option<i64>,
    at_millis: i64,
    kind: String,
    profile_id: Option<String>,
    workspace_id: Option<String>,
    document_id: Option<String>,
    document_name: Option<String>,
    blob_hash: Option<String>,
    detail: Option<String>,
    items: Option<usize>,
}

#[derive(Serialize)]
struct MobileStateHistory {
    device_id: String,
    events: Vec<MobileStateEvent>,
    next_before: Option<i64>,
}

/// The word the device list uses for how a live peer is reached.
fn connection_path(path: koru_network_iroh::PathKind) -> Option<&'static str> {
    match path {
        koru_network_iroh::PathKind::Direct => Some("direct"),
        koru_network_iroh::PathKind::Relay => Some("relay"),
        koru_network_iroh::PathKind::Unknown => None,
    }
}

#[derive(Serialize, Default)]
struct InventoryResult {
    profiles: Vec<InventoryProfile>,
    workspaces: Vec<InventoryWorkspace>,
    documents: Vec<InventoryDocument>,
    /// Rows excluded by the lifecycle filters. A document that flickers out
    /// of the inventory moves one of these counters, and that is the
    /// difference between "content is missing" and "state is oscillating".
    #[serde(skip_serializing_if = "is_zero")]
    deleted_workspaces: usize,
    #[serde(skip_serializing_if = "is_zero")]
    trashed_documents: usize,
    /// Workspaces whose own read failed and were skipped rather than let one
    /// bad row freeze the whole inventory.
    #[serde(skip_serializing_if = "is_zero")]
    failed_workspaces: usize,
    /// Run and schedule documents the listing hides (proposal F6): a run is
    /// reached through its pill and never as a row. Counted so a document
    /// that "disappeared" from the list can be told from one that never
    /// arrived.
    #[serde(skip_serializing_if = "is_zero")]
    hidden_runs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Serialize)]
struct InventoryProfile {
    id: String,
    name: String,
    locale: String,
    /// First-class switcher icon. `leading_emoji` remains below as a
    /// compatibility projection for mobile builds from before profile
    /// appearance was explicit.
    icon: Option<String>,
    leading_emoji: Option<String>,
    display_name: String,
    color: Option<String>,
    account_binding: String,
}

#[derive(Serialize)]
struct InventoryWorkspace {
    id: String,
    profile_id: String,
    name: String,
    icon: Option<String>,
    color: Option<String>,
    leading_emoji: Option<String>,
    display_name: String,
    archived: bool,
    deleted_at_millis: Option<i64>,
    modified_at_millis: i64,
}

#[derive(Serialize)]
struct InventoryDocument {
    id: String,
    workspace_id: String,
    title: String,
    display_title: String,
    leading_emoji: Option<String>,
    kind: String,
    mime: Option<String>,
    /// The file's one note, when it has one. Notes themselves never appear as
    /// rows — they open through the file they belong to.
    note_id: Option<String>,
    /// When this document last changed, so a list can show the newest first.
    /// Zero when the repository has no record, which sorts oldest rather than
    /// pretending to a recency nobody measured.
    updated_at_millis: i64,
}

#[derive(Serialize)]
#[cfg(test)]
struct DocumentViewResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<DocumentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

#[derive(Clone, Serialize)]
struct DocumentView {
    id: String,
    workspace_id: String,
    title: String,
    revision: u64,
    blocks: Vec<ViewBlock>,
}

#[derive(Clone, Serialize)]
struct ViewBlock {
    id: String,
    parent_id: Option<String>,
    kind: String,
    properties: ViewBlockProperties,
    #[serde(skip_serializing_if = "Option::is_none")]
    rich_text: Option<ViewRichText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blob: Option<ViewBlob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<ViewReference>,
}

#[derive(Clone, Serialize)]
struct ViewBlockProperties {
    name: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    done: bool,
    alignment: &'static str,
    heading_level: Option<u8>,
    list_style: Option<&'static str>,
    image_width: Option<u32>,
    image_height: Option<u32>,
    language: Option<String>,
}

#[derive(Clone, Serialize)]
struct ViewRichText {
    text: String,
    marks: Vec<ViewMark>,
    inline_objects: Vec<ViewInlineObject>,
}

#[derive(Clone, Serialize)]
struct ViewMark {
    start_utf8: usize,
    end_utf8: usize,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

#[derive(Clone, Serialize)]
struct ViewInlineObject {
    id: String,
    anchor_utf8: usize,
    role: String,
    label: Option<String>,
    reference: ViewReference,
}

#[derive(Clone, Serialize)]
struct ViewBlob {
    id: String,
    mime_type: String,
    size: u64,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ViewReference {
    Document {
        document_id: String,
        /// One block inside that document, when the link names one.
        ///
        /// **Carried, not yet acted on.** Scrolling to a block is its own piece
        /// of work on this side; the wire stops being lossy now, while the
        /// field is being added, rather than after a release has shipped the
        /// shape without it.
        #[serde(skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
    },
    Block {
        block_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        document_id: Option<String>,
    },
    Url {
        url: String,
    },
    Label {
        value: String,
    },
    /// A delegated run, by its Run document (P1.18). The pill's stored
    /// record is a reference homed at the run under the `koru.run` role
    /// (proposal §3.2); the phone opens it on the Run screen, and names the
    /// executing device from the run's header rather than from the pill.
    Run {
        document_id: String,
    },
}

struct MobileHost {
    /// Platform-authenticated local admission. Mobile carries no bearer and
    /// opens no loopback listener, but every product call is catalogued and
    /// scope-checked here before touching the embedded domain state.
    gateway: koru_api_gateway_core::MobileGateway,
    _runtime: tokio::runtime::Runtime,
    network: koru_network_iroh::RuntimeHandle,
    signing_key: koru_device::DeviceSigningKey,
    /// `None` when the on-disk identity is inconsistent: the local Store must
    /// still open and serve content, so identity failure degrades sync rather
    /// than bricking the app.
    root: PathBuf,
    trust: Mutex<Option<TrustState>>,
    trust_revision: AtomicU64,
    /// Why `trust` is `None`, while it is. Cleared by [`Self::reset_identity`].
    identity_error: Mutex<Option<String>>,
    /// Kept for [`Self::reset_identity`]: a mismatched open never provisions
    /// the endpoint, and the reset has to.
    endpoint_secret: koru_device::ReplicationEndpointSecret,
    /// Kept for the host lifetime: dropping it requests shutdown and joins the
    /// only thread allowed to own the persistent Store.
    _store_worker: koru_store_server::WorkerHandle,
    store: koru_store_server::StoreWorkerClient,
    /// The blob transfer engine the worker composed, with this platform's
    /// network policy and announcement installed into it (Plan B5). `None`
    /// on a device whose read model would not open: reconciling against a
    /// guess about which blobs a Workspace has would move the wrong set.
    blobs: Option<blob::MobileBlobs>,
    document_views: Mutex<HashMap<DocumentId, (Vec<u8>, DocumentView)>>,
    document_focus: Mutex<Option<DocumentId>>,
    /// The runs this phone has open: one fork per open, kept for the
    /// document version it was forked at, so a window read costs no store
    /// bytes (proposal §9). Bounded: a phone shows one run and the pills of
    /// one document at a time.
    run_views: Mutex<Vec<OpenRun>>,
    /// Diagnostics, always live and free: forks taken for runs, and every
    /// round trip to the store worker. The tests hold the criterion with
    /// them — ten windows after one open move neither.
    run_forks: AtomicU64,
    store_calls: AtomicU64,
    // Reused across body edits; one fresh CRDT peer per keystroke would grow
    // every document's version vector without bound during a typing session.
    document_edit_peer: u64,
    linking: AtomicBool,
    pairing: Mutex<Option<koru_pairing::InvitationSession>>,
    account: Mutex<Option<Arc<account::MobileAccount>>>,
    /// When the Cloud coordinator was last woken, so the app's own poll can
    /// pace a heartbeat without holding a timer of its own.
    last_cloud_wake: Mutex<Option<std::time::Instant>>,
    /// The authorization snapshot last handed to the network runtime, kept to
    /// be read back: it is where a peer's display name lives, which is how a
    /// waiting document names the device it waits for. The desktop Store
    /// reads the same value out of its `ControlHost`.
    authorization: Mutex<Option<koru_sync_control::AuthorizationSnapshot>>,
}

/// How often a phone that is merely open asks the Cloud for other devices'
/// work. The desktop's own heartbeat thread uses the same interval.
const CLOUD_WAKE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether the poll should wake the Cloud coordinator now. A device that has
/// never woken one is always due: the first poll after a start is what makes
/// a phone that writes nothing still receive.
fn cloud_wake_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.is_none_or(|at| {
        now.checked_duration_since(at)
            .is_none_or(|elapsed| elapsed >= CLOUD_WAKE_INTERVAL)
    })
}

#[cfg(test)]
mod cloud_wake_tests {
    use super::{CLOUD_WAKE_INTERVAL, cloud_wake_due};
    use std::time::{Duration, Instant};

    #[test]
    fn a_phone_that_never_woke_is_due_and_one_that_just_did_waits() {
        let now = Instant::now();
        assert!(cloud_wake_due(None, now), "the first poll after a start");
        let just_now = now.checked_sub(Duration::from_secs(1)).unwrap();
        assert!(!cloud_wake_due(Some(just_now), now));
        let long_ago = now.checked_sub(CLOUD_WAKE_INTERVAL).unwrap();
        assert!(cloud_wake_due(Some(long_ago), now), "exactly the interval");
        let older = now.checked_sub(CLOUD_WAKE_INTERVAL * 4).unwrap();
        assert!(cloud_wake_due(Some(older), now));
    }
}

/// A run the phone holds open: the fork the store served, at the version it
/// was served, and the sequence the cache evicts by.
struct OpenRun {
    document: DocumentId,
    revision: Vec<u8>,
    run: Arc<RunDocument>,
    opened: u64,
}

/// How many runs stay open at once: the run on screen plus the pills of the
/// document beneath it. Past this the oldest is dropped and re-forked when
/// asked for again.
const OPEN_RUN_SLOTS: usize = 6;

impl MobileHost {
    /// `device_name` is minted into `device.json` on a *first* open — it is
    /// this device's display name on every other device's list, so the shim
    /// names its platform ("Android"; the iOS shim passes what Swift hands
    /// it). An existing descriptor wins; the name is never rewritten.
    ///
    /// `relay_enabled` is this device's relay on/off value (decision №6's
    /// mobile semantic). The platform's local storage owns it —
    /// SharedPreferences on Android, UserDefaults on iOS — and hands it in
    /// through `HostContext.relay_enabled`; absent means the shipped
    /// default, relays on. A later change arrives through
    /// [`Self::set_relay_enabled`], not a reopen.
    fn open(
        root: PathBuf,
        secrets: [u8; 64],
        device_name: &str,
        relay_enabled: bool,
    ) -> Result<Arc<Self>, String> {
        init_logging();
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let signing_key = koru_device::DeviceSigningKey::from_secret_bytes(
            secrets[..32].try_into().expect("slice length"),
        );
        let endpoint_secret = koru_device::ReplicationEndpointSecret::from_secret_bytes(
            secrets[32..].try_into().expect("slice length"),
        );
        let identity_path = root.join("device.json");
        let descriptor = (|| -> Result<koru_device::DeviceDescriptorV1, String> {
            match std::fs::read(&identity_path) {
                Ok(bytes) => {
                    serde_json::from_slice(&bytes).map_err(|_| "device_identity_mismatch".into())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Missing identity beside old trust is partial data, never
                    // permission for migration to create a different device.
                    for name in [
                        "trust.json",
                        "trust.db",
                        "trust.db-wal",
                        "trust.db-shm",
                        "trust.migrated-to-sqlite.json",
                    ] {
                        match std::fs::metadata(root.join(name)) {
                            Ok(_) => return Err("device_identity_mismatch".into()),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(_) => return Err("device_identity_unreadable".into()),
                        }
                    }
                    let descriptor = koru_device::DeviceDescriptorV1::new(
                        DeviceId::new(),
                        device_name.to_owned(),
                        &signing_key,
                        endpoint_secret.endpoint_id(),
                        now_millis(),
                    );
                    std::fs::write(
                        &identity_path,
                        serde_json::to_vec_pretty(&descriptor)
                            .map_err(|_| "device_identity_unreadable")?,
                    )
                    .map_err(|_| "device_identity_unreadable")?;
                    Ok(descriptor)
                }
                Err(_) => Err("device_identity_unreadable".into()),
            }
        })();
        // Identity failure degrades sync; local content still opens below.
        let trust = descriptor.and_then(|descriptor| {
            if descriptor.signing_public_key != signing_key.public_key()
                || descriptor.endpoint_id != endpoint_secret.endpoint_id()
                || descriptor.verify().is_err()
            {
                return Err("device_identity_mismatch".into());
            }
            TrustState::open(&root, descriptor)
        });
        let (trust, identity_error) = match trust {
            Ok(trust) => (Some(trust), None),
            Err(error) => (None, Some(error)),
        };
        let store_worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::mobile(root.clone(), "mobile"),
        )
        .spawn()
        .map_err(|error| error.to_string())?;
        let store = store_worker.client();
        // Installed before anything can ask for a picture, and before the
        // account is activated: the engine exists as soon as the worker is
        // ready, and a platform fact that arrives after the first pass is a
        // platform fact the first pass did not have.
        let blobs = blob::MobileBlobs::install(&store_worker);
        if blobs.is_none() {
            // Unconditional: a phone that will never fetch a picture is not
            // something to discover from an empty frame.
            tracing::warn!("blob transfer is unavailable: the read model did not open");
        }
        let adapter: Arc<dyn ReplicationStore> = Arc::new(MobileReplicationStore {
            store: store.clone(),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let network = {
            let _entered = runtime.enter();
            koru_network_iroh::RuntimeHandle::spawn_with_store(
                network_config(relay_enabled),
                adapter,
            )
        };
        if trust.is_some() {
            network.provision_endpoint(endpoint_secret.secret_bytes());
        }
        let host = Arc::new(Self {
            gateway: koru_api_gateway_core::MobileGateway::platform_local(device_name),
            _runtime: runtime,
            network,
            signing_key,
            root: root.clone(),
            trust: Mutex::new(trust),
            trust_revision: AtomicU64::new(0),
            identity_error: Mutex::new(identity_error),
            endpoint_secret,
            _store_worker: store_worker,
            store,
            blobs,
            document_views: Mutex::new(HashMap::new()),
            document_focus: Mutex::new(None),
            run_views: Mutex::new(Vec::new()),
            run_forks: AtomicU64::new(0),
            store_calls: AtomicU64::new(0),
            document_edit_peer: loro::LoroDoc::new().peer_id(),
            linking: AtomicBool::new(false),
            pairing: Mutex::new(None),
            account: Mutex::new(None),
            last_cloud_wake: Mutex::new(None),
            authorization: Mutex::new(None),
        });
        host.refresh_authorization();
        host.start_maintenance();
        Ok(host)
    }

    fn admit(&self, operation: &str) -> Result<(), String> {
        self.gateway
            .admit(operation)
            .map(|_| ())
            .map_err(str::to_owned)
    }

    fn with_store<R, F>(&self, task: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: FnOnce(&mut koru_store_server::WorkerStore) -> Result<R, String> + Send + 'static,
    {
        self.store_calls.fetch_add(1, Ordering::Relaxed);
        self.store
            .with_store(task)
            .map_err(|error| store_worker_code(&error).to_owned())?
    }

    // ----- what the in-process Cloud host reaches for (account::cloud) -----

    pub(crate) fn store(&self) -> &koru_store_server::StoreWorkerClient {
        &self.store
    }

    /// The blob transfer engine, or `None` on a device whose read model did
    /// not open.
    pub(crate) fn blobs(&self) -> Option<&blob::MobileBlobs> {
        self.blobs.as_ref()
    }

    pub(crate) fn network(&self) -> &koru_network_iroh::RuntimeHandle {
        &self.network
    }

    /// The host's own Tokio runtime. The Cloud coordinator blocks on it from
    /// its worker thread, which is never one of the runtime's own.
    pub(crate) fn tokio(&self) -> tokio::runtime::Handle {
        self._runtime.handle().clone()
    }

    pub(crate) fn device_key(&self) -> koru_device::DeviceSigningKey {
        koru_device::DeviceSigningKey::from_secret_bytes(self.signing_key.secret_bytes())
    }

    pub(crate) fn local_endpoint(&self) -> koru_device::ReplicationEndpointId {
        self.endpoint_secret.endpoint_id()
    }

    /// A durable local commit: the peer data plane is woken where the
    /// commit happens, and the Cloud coordinator here, so a phone's edit
    /// reaches the worker on its next pass rather than on the next remote
    /// hint.
    fn wake_cloud(&self) {
        self.note_cloud_wake();
        if let Ok(guard) = self.account.lock()
            && let Some(account) = guard.as_ref()
        {
            account.wake_cloud();
        }
    }

    fn note_cloud_wake(&self) {
        if let Ok(mut last) = self.last_cloud_wake.lock() {
            *last = Some(std::time::Instant::now());
        }
    }

    /// The phone's own heartbeat, hung on the poll the app already makes
    /// while a person is looking at it.
    ///
    /// Without this a phone runs a Cloud pass only when it *writes* something:
    /// a device that is merely open never asks the worker for anything, so a
    /// document another device changed arrives on this one's next edit and
    /// not before. The desktop has a 30 s thread for exactly this; a phone
    /// must not hold a timer of its own while backgrounded, so its own poll
    /// is the tick, paced to the same interval.
    pub(crate) fn wake_cloud_periodically(&self) {
        let due = match self.last_cloud_wake.lock() {
            Ok(last) => cloud_wake_due(*last, std::time::Instant::now()),
            Err(_) => false,
        };
        if due {
            tracing::debug!("cloud host: periodic wake from the account poll");
            self.wake_cloud();
        }
    }

    fn identity_error_code(&self) -> String {
        self.identity_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .unwrap_or_else(|| "device_identity_mismatch".into())
    }

    /// Start over as a new device, keeping every workspace. Only for a host
    /// whose identity could not be opened — the seeds in the platform's
    /// keystore no longer produce the keys `device.json` names, which is what
    /// happens when an app is deleted and reinstalled (iOS drops the
    /// Keychain item with it) or its data is restored onto another phone.
    /// Nothing is destroyed: `device.json`, legacy trust JSON, and the closed
    /// trust database with its WAL/shared-memory sidecars move into
    /// `identity-set-aside-<millis>/`, a fresh descriptor is minted from the
    /// keys this device *does* hold, and linking works again. The old device
    /// stays listed as offline on its peers until someone unlinks it there;
    /// there is no key to sign a revocation with.
    fn reset_identity(&self, device_name: &str) -> Result<(), String> {
        // Activation takes these locks in this order. A healthy identity is
        // never eligible; a failed open dropped its SQLite connection before
        // publishing trust=None and could never activate enrollment.
        let account_guard = self.account.lock().map_err(|_| "account_lock")?;
        let mut guard = self.trust.lock().unwrap_or_else(|error| error.into_inner());
        if guard.is_some() {
            return Err("identity_intact".into());
        }
        if account_guard.is_some() {
            return Err("identity_reset_busy".into());
        }
        let root = self.root.clone();
        let aside = root.join(format!("identity-set-aside-{}", now_millis()));
        trust::archive_identity(&mut trust::DiskIdentityArchive {
            root: &root,
            aside: &aside,
        })?;
        let descriptor = koru_device::DeviceDescriptorV1::new(
            DeviceId::new(),
            device_name.to_owned(),
            &self.signing_key,
            self.endpoint_secret.endpoint_id(),
            now_millis(),
        );
        std::fs::write(
            root.join("device.json"),
            serde_json::to_vec_pretty(&descriptor).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let trust = TrustState::open(&root, descriptor)?;
        *guard = Some(trust);
        *self
            .identity_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        drop(guard);
        drop(account_guard);
        tracing::warn!(set_aside = %aside.display(), "device identity reset; the previous identity is set aside");
        self.network
            .provision_endpoint(self.endpoint_secret.secret_bytes());
        self.refresh_authorization();
        Ok(())
    }

    /// Switch relay use on the live network runtime, without an app restart
    /// (decision №6: the change applies immediately). The endpoint rebinds
    /// under the re-resolved provider with the node secret unchanged; relay
    /// routes drop or return while direct/LAN paths heal on their own. The
    /// *persisted* value stays owned by the platform's local storage — the
    /// shim writes it there and hands it back through `HostContext` on the
    /// next open; this host records nothing.
    fn set_relay_enabled(&self, enabled: bool) {
        self.network
            .replace_relay_provider(network_config(enabled).relay_provider);
    }

    fn status(&self) -> LinkResult<'static> {
        let guard = self.trust.lock().unwrap();
        let Some(trust) = guard.as_ref() else {
            return LinkResult {
                state: "unavailable",
                peer_name: None,
                error_code: Some(self.identity_error_code()),
            };
        };
        let view = match trust.view() {
            Ok(view) => view,
            Err(error) => {
                return LinkResult {
                    state: "unavailable",
                    peer_name: None,
                    error_code: Some(error),
                };
            }
        };
        match koru_device::Authorization::devices(&view).first() {
            Some(peer) => LinkResult {
                state: "linked",
                peer_name: view
                    .devices
                    .iter()
                    .find(|device| device.device_id == peer.device_id)
                    .map(|device| device.display_name.clone()),
                error_code: None,
            },
            None => LinkResult {
                state: "unlinked",
                peer_name: None,
                error_code: None,
            },
        }
    }

    fn inventory(&self) -> Result<InventoryResult, String> {
        self.with_store(|store| {
            let profiles = store
                .profiles()
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .map(|profile| {
                    let (name_emoji, display) = koru_title::split_leading_emoji(&profile.name);
                    let avatar_emoji = profile.avatar.as_deref().and_then(|avatar| {
                        let (emoji, rest) = koru_title::split_leading_emoji(avatar);
                        rest.is_empty().then_some(emoji).flatten()
                    });
                    let icon = profile
                        .icon
                        .clone()
                        .or_else(|| avatar_emoji.map(str::to_owned));
                    let color = profile.color.clone().or_else(|| {
                        profile_avatar_color(profile.avatar.as_deref()).map(str::to_owned)
                    });
                    InventoryProfile {
                        id: profile.id.to_storage(),
                        name: profile.name.clone(),
                        locale: profile.locale.to_string(),
                        icon: icon.clone(),
                        leading_emoji: icon.or_else(|| name_emoji.map(str::to_owned)),
                        display_name: display.to_owned(),
                        color,
                        account_binding: profile.account.clone().unwrap_or_default(),
                    }
                })
                .collect();
            let workspace_records = store
                .workspaces_including_deleted()
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .collect::<Vec<_>>();
            let mut workspaces = Vec::with_capacity(workspace_records.len());
            for workspace in &workspace_records {
                // A workspace with no documents yields no modified time; that is
                // its creation, not a reason to fail the whole inventory.
                let modified_at_millis = match workspace.deleted_at {
                    Some(deleted_at) => deleted_at.millis().max(workspace.created_at.millis()),
                    None => store
                        .workspace_modified_at(workspace.id)
                        .map(|at| at.millis())
                        .unwrap_or_else(|_| workspace.created_at.millis()),
                };
                let (leading, display) = koru_title::split_leading_emoji(&workspace.name);
                workspaces.push(InventoryWorkspace {
                    id: workspace.id.to_storage(),
                    profile_id: workspace.owner.to_storage(),
                    name: workspace.name.clone(),
                    icon: workspace.icon.clone(),
                    color: workspace.color.clone(),
                    leading_emoji: leading.map(str::to_owned),
                    display_name: display.to_owned(),
                    archived: workspace.archived,
                    deleted_at_millis: workspace.deleted_at.map(|at| at.millis()),
                    modified_at_millis,
                });
            }
            let mut documents = Vec::new();
            let mut deleted_workspaces = 0usize;
            let mut trashed_documents = 0usize;
            let mut failed_workspaces = 0usize;
            let mut hidden_runs = 0usize;
            for workspace in workspace_records.into_iter() {
                if workspace.deleted_at.is_some() {
                    // Counted, not just skipped: an oscillating lifecycle flag is
                    // invisible in a list that silently loses rows.
                    deleted_workspaces += 1;
                    continue;
                }
                // One read per Workspace, not one per document: recency is a
                // repository record and joining it here keeps the listing a
                // single pass.
                let updates = store
                    .document_updates(workspace.id)
                    .map(|rows| rows.into_iter().collect::<HashMap<_, _>>())
                    .unwrap_or_default();
                let workspace_documents = match store.documents(workspace.id) {
                    Ok(documents) => documents,
                    Err(_) => {
                        // One broken workspace must not take every other
                        // workspace's inventory down with it.
                        failed_workspaces += 1;
                        continue;
                    }
                };
                for document in workspace_documents {
                    if document.trashed_at.is_some() {
                        trashed_documents += 1;
                        continue;
                    }
                    if document.kind == koru_core::explore::KIND_NOTE {
                        // A note is reached through its file's `note_id`, not
                        // as a row of its own.
                        continue;
                    }
                    if document.kind == RUN_KIND || document.kind == SCHEDULE_KIND {
                        // A run is reached through its pill and the Run
                        // screen, a schedule through its own surface (M2);
                        // neither is a row a person browses (proposal F6).
                        hidden_runs += 1;
                        continue;
                    }
                    let (leading, display) = koru_title::split_leading_emoji(&document.title);
                    let leading_emoji = leading.map(str::to_owned);
                    let display_title = display.to_owned();
                    documents.push(InventoryDocument {
                        id: document.id.to_storage(),
                        workspace_id: workspace.id.to_storage(),
                        title: document.title,
                        display_title,
                        leading_emoji,
                        kind: document.kind,
                        mime: document.mime,
                        note_id: document.note.map(|note| note.to_storage()),
                        updated_at_millis: updates
                            .get(&document.id)
                            .map(|at| at.millis())
                            .unwrap_or_default(),
                    });
                }
            }
            Ok(InventoryResult {
                profiles,
                workspaces,
                documents,
                deleted_workspaces,
                trashed_documents,
                failed_workspaces,
                hidden_runs,
                error_code: None,
            })
        })
    }

    /// A complete, immutable view of one note. Unchanged Store revisions reuse
    /// the last projection. The Store lock is otherwise held only while taking
    /// the structural Loro fork and reading blob metadata; the node walk happens
    /// afterwards so a long document cannot stall sync.
    fn view_document(&self, document_id: &str) -> Result<DocumentView, String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        *self
            .document_focus
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(document);
        let cached = self
            .document_views
            .lock()
            .map_err(|_| "document_cache_lock".to_owned())?
            .get(&document)
            .cloned();
        let cached_revision = cached.as_ref().map(|(revision, _)| revision.clone());
        let loaded = self.with_store(move |store| {
            // Distinct codes on purpose: an intermittent NotFound needs its
            // failing layer named, and "somewhere in this function" is not a
            // layer.
            let workspace = store
                .locate(document)
                .map_err(|error| error.code().as_str().to_owned())?
                .ok_or_else(|| "document_unlocated".to_owned())?;
            let entry = store
                .documents(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|entry| entry.id == document && entry.trashed_at.is_none())
                .ok_or_else(|| "document_not_listed".to_owned())?;
            if entry.kind == "file" {
                return Err("document_is_file".to_owned());
            }
            // A run has no node tree to walk; it opens on the Run screen
            // through `view_run`. A schedule has no phone surface yet (M2).
            if entry.kind == RUN_KIND {
                return Err("document_is_run".to_owned());
            }
            if entry.kind == SCHEDULE_KIND {
                return Err("document_is_schedule".to_owned());
            }
            let revision = store
                .version(workspace, document)
                .map_err(|error| error.code().as_str().to_owned())?
                .encode();
            if cached_revision.as_ref() == Some(&revision) {
                return Ok(None);
            }
            let fork = store
                .fork_document(workspace, document)
                .map_err(|error| error.code().as_str().to_owned())?;
            let blob_sizes = store
                .blobs(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .map(|blob| (blob.hash.as_str().to_owned(), blob.size))
                .collect::<HashMap<_, _>>();
            Ok(Some((workspace, entry.title, revision, fork, blob_sizes)))
        })?;
        let Some((workspace, listed_title, revision, fork, blob_sizes)) = loaded else {
            return Ok(cached.expect("matching cached revision has a view").1);
        };

        let model = koru_core::WorkspaceDoc::adopt(fork)
            .map_err(|_| "invalid_document_snapshot".to_owned())?;
        let view = document_view(
            &model,
            workspace,
            document,
            listed_title,
            revision.clone(),
            &blob_sizes,
        )?;
        self.document_views
            .lock()
            .map_err(|_| "document_cache_lock".to_owned())?
            .insert(document, (revision, view.clone()));
        // Which pictures this phone is showing, for the announcement seam's
        // `watching`. It decides **order** and never whether — a referenced
        // blob is fetched whether or not anybody is looking, or coverage would
        // depend on which document was open — so a picture somebody is staring
        // at a placeholder for simply goes first.
        self.note_showing();
        Ok(view)
    }

    /// Replace the set of blob hashes the phone's open document views name.
    fn note_showing(&self) {
        let Some(blobs) = self.blobs.as_ref() else {
            return;
        };
        let Ok(views) = self.document_views.lock() else {
            return;
        };
        blobs.arrivals().showing(
            views
                .values()
                .flat_map(|(_, view)| view.blocks.iter())
                .filter_map(|block| block.blob.as_ref())
                .map(|blob| blob.id.clone()),
        );
    }

    /// A delegated run's header as the phone shows it (P1.18). Opened the
    /// way a note is — `locate`, `documents`, `fork_document` — and read
    /// with the run model. **One fork per open**: the fork is kept for the
    /// document version it was served at, an unchanged version reuses it,
    /// and every window the phone scrolls afterwards is read from it without
    /// a store round trip (proposal §9). The Store lock is held for the
    /// catalog reads and the fork only; the header is read afterwards.
    fn view_run(&self, document_id: &str) -> Result<RunView, String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        let (workspace, revision, run) = self.open_run(document)?;
        let header = match run.header() {
            Ok(header) => header,
            // A newer build may have reshaped the header; what this build
            // cannot read is reported as such, never as a crash.
            Err(RunDocumentError::Malformed { .. }) if !run.compatibility().writable() => {
                return Err("run_needs_newer_koru".to_owned());
            }
            Err(_) => return Err("invalid_run_document".to_owned()),
        };
        let guard = self.trust.lock().map_err(|_| "trust_lock".to_owned())?;
        // dev made `view()` fallible; a trust view Koru cannot read names
        // nothing, which is the same answer as having no identity at all.
        let view = guard.as_ref().and_then(|trust| trust.view().ok());
        let name_of =
            |device: DeviceId| view.as_ref().and_then(|view| view.display_name_of(device));
        let names = DeviceNames {
            local: view.as_ref().map(|view| view.local_device.device_id),
            name_of: &name_of,
        };
        Ok(run_view::run_view(
            document,
            workspace,
            revision_number(&revision),
            &run,
            &header,
            &names,
        ))
    }

    /// One window of a run's activity, newest first, from the fork
    /// [`view_run`](Self::view_run) holds. **No store read**: the rows come
    /// from the run already in memory, whatever the offset (criteria 2 and
    /// 3 of proposal §9). A run the phone has not opened is opened first —
    /// one fork — and the window read from that. The window is the version
    /// the phone was last told about; a newer version reaches it through
    /// the observer's next `view_run`, never through a window read.
    fn run_timeline_window(
        &self,
        document_id: &str,
        newest_first_from: usize,
        len: usize,
    ) -> Result<TimelineWindow, String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        let (revision, run) = match self.held_run(document) {
            Some(held) => held,
            None => {
                let (_, revision, run) = self.open_run(document)?;
                (revision, run)
            }
        };
        // Zero asks for the default window; anything larger than the bound
        // is the bound, so one answer's cost stays bounded.
        let len = match len {
            0 => run_view::TIMELINE_WINDOW,
            len => len.min(run_view::MAX_TIMELINE_WINDOW),
        };
        Ok(TimelineWindow {
            document_id: document.to_storage(),
            newest_first_from: newest_first_from.min(u32::MAX as usize) as u32,
            rows: run_view::timeline_rows(&run, newest_first_from, len),
            timeline_len: run.timeline_len().min(u32::MAX as usize) as u32,
            revision: revision_number(&revision),
        })
    }

    fn held_run(&self, document: DocumentId) -> Option<(Vec<u8>, Arc<RunDocument>)> {
        self.run_views
            .lock()
            .ok()?
            .iter()
            .find(|open| open.document == document)
            .map(|open| (open.revision.clone(), Arc::clone(&open.run)))
    }

    /// The run at the store's current version: the held fork when the
    /// version has not moved, otherwise a fresh fork that replaces it.
    fn open_run(
        &self,
        document: DocumentId,
    ) -> Result<(WorkspaceId, Vec<u8>, Arc<RunDocument>), String> {
        let held = self.held_run(document);
        let held_revision = held.as_ref().map(|(revision, _)| revision.clone());
        let (workspace, revision, fork) = self.with_store(move |store| {
            let workspace = store
                .locate(document)
                .map_err(|error| error.code().as_str().to_owned())?
                .ok_or_else(|| "document_unlocated".to_owned())?;
            let entry = store
                .documents(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|entry| entry.id == document && entry.trashed_at.is_none())
                .ok_or_else(|| "document_not_listed".to_owned())?;
            if entry.kind != RUN_KIND {
                return Err("document_is_not_a_run".to_owned());
            }
            let revision = store
                .version(workspace, document)
                .map_err(|error| error.code().as_str().to_owned())?
                .encode();
            if held_revision.as_ref() == Some(&revision) {
                return Ok((workspace, revision, None));
            }
            let fork = store
                .fork_document(workspace, document)
                .map_err(|error| error.code().as_str().to_owned())?;
            Ok((workspace, revision, Some(fork)))
        })?;
        let Some(fork) = fork else {
            let (_, run) = held.expect("a matching held revision has a run");
            return Ok((workspace, revision, run));
        };
        self.run_forks.fetch_add(1, Ordering::Relaxed);
        let run = match RunDocument::adopt(fork) {
            Ok(run) => Arc::new(run),
            Err(RunDocumentError::NotARun) => return Err("document_is_not_a_run".to_owned()),
            Err(_) => return Err("invalid_run_document".to_owned()),
        };
        let mut open = self
            .run_views
            .lock()
            .map_err(|_| "run_cache_lock".to_owned())?;
        open.retain(|held| held.document != document);
        let opened = open.iter().map(|held| held.opened).max().unwrap_or(0) + 1;
        open.push(OpenRun {
            document,
            revision: revision.clone(),
            run: Arc::clone(&run),
            opened,
        });
        if open.len() > OPEN_RUN_SLOTS {
            open.sort_by_key(|held| held.opened);
            open.remove(0);
        }
        Ok((workspace, revision, run))
    }

    fn load_blob(&self, workspace_id: &str, hash: &str) -> Result<Vec<u8>, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let hash =
            koru_store_core::BlobHash::parse(hash).map_err(|_| "invalid_blob_id".to_owned())?;
        let local_hash = hash.clone();
        let local = self.with_store(move |store| {
            store
                .get_blob(workspace, &local_hash)
                .map_err(|error| error.code().as_str().to_owned())
        });
        match local {
            Ok(bytes) => return Ok(bytes),
            Err(code) if code == "blob_not_found" => {}
            Err(code) => return Err(code),
        }

        // The Store lock is deliberately gone before the network wait. The
        // shared coordinator publishes success only after a verified Store
        // commit. **The wait is bounded**: when no reachable peer can serve
        // the bytes it answers `blob_waiting_for_peer` or
        // `no_linked_devices` at once and keeps waiting in the background,
        // which is what the editor's placeholder is built to show. It used
        // to wait however long it took, and every picture on a phone whose
        // holder was offline pinned a thread until that device came back.
        // Not a Cloud request: a picture drawing itself is not a person
        // asking for it, and D50's holds are waived only by a person.
        self.tokio()
            .block_on(
                self.network
                    .hydrate_blob(workspace, hash.clone())
                    .wait_unless_parked(),
            )
            .map_err(|error| error.code)?;
        self.with_store(move |store| {
            store
                .get_blob(workspace, &hash)
                .map_err(|error| error.code().as_str().to_owned())
        })
    }

    /// What blob a `kind == "file"` document *is*.
    ///
    /// **Its own operation because opening one is refused.** A file is not a
    /// node tree to walk, so `document` and the edit path both answer
    /// `document_is_file` — deliberately. A caller that wants to open the file
    /// therefore has no way to reach its hash through the document API at all,
    /// which is what this closes.
    ///
    /// The shape is `import_file`'s, read back: the one node under the root
    /// whose kind is `File`, whose content is `Content::Blob`. Size comes from
    /// the Workspace's blob listing rather than the node, which is where the
    /// document projection reads it too.
    fn file_blob(&self, document_id: &str) -> Result<(String, String, u64), String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        self.with_store(move |store| {
            let workspace = store
                .locate(document)
                .map_err(|error| error.code().as_str().to_owned())?
                .ok_or_else(|| "document_unlocated".to_owned())?;
            let entry = store
                .documents(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|entry| entry.id == document && entry.trashed_at.is_none())
                .ok_or_else(|| "document_not_listed".to_owned())?;
            if entry.kind != "file" {
                return Err("document_is_not_a_file".to_owned());
            }
            let fork = store
                .fork_document(workspace, document)
                .map_err(|error| error.code().as_str().to_owned())?;
            let model = koru_core::WorkspaceDoc::adopt(fork)
                .map_err(|_| "invalid_document_snapshot".to_owned())?;
            let root = model
                .children(koru_core::Parent::Root)
                .map_err(|_| "invalid_document_snapshot".to_owned())?
                .into_iter()
                .find(|id| model.kind_of(*id) == Some(koru_core::Kind::File))
                .ok_or_else(|| "file_has_no_blob".to_owned())?;
            let node = model
                .read(root)
                .map_err(|_| "invalid_document_snapshot".to_owned())?;
            let koru_core::Content::Blob { hash, mime, .. } = node.content else {
                return Err("file_has_no_blob".to_owned());
            };
            // Two `BlobHash` types meet here: the node's is `koru_core`'s and
            // the listing's is the Store's. They are the same digest spelled
            // by two crates, so the comparison goes through the text.
            let hash = hash.0;
            let size = store
                .blobs(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|blob| blob.hash.as_str() == hash)
                .map(|blob| blob.size)
                .unwrap_or(0);
            Ok((hash, mime, size))
        })
    }

    /// Copy one blob to a path the platform named, for handing to another app.
    ///
    /// **The mirror of [`Self::import_file`], and a path for the same reason.**
    /// A file can be a video; `load_blob` answers with the whole payload in a
    /// `Vec<u8>` that then crosses JNI into the JVM heap, which is exactly what
    /// the inbound side was written to avoid. This streams Store to disk and
    /// holds one buffer.
    ///
    /// **Verified once, at the start.** `open_blob` deliberately does not
    /// re-hash — a chunked read that verified per chunk would be O(size²) — so
    /// the contract is that a streaming caller calls `verify_blob` first. A
    /// player is handed bytes this device has vouched for, not bytes it merely
    /// found.
    ///
    /// **Atomic.** The copy goes to a sibling temporary and is renamed, so a
    /// reader opening the destination never sees a half-written video, and a
    /// failure leaves no file at all rather than a plausible broken one.
    fn export_blob(
        &self,
        workspace_id: &str,
        hash: &str,
        destination: &str,
    ) -> Result<u64, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let hash =
            koru_store_core::BlobHash::parse(hash).map_err(|_| "invalid_blob_id".to_owned())?;
        let destination = std::path::PathBuf::from(destination);
        // The platform owns its storage layout; this refuses anything that is
        // not an absolute path with a file name rather than inventing one.
        if !destination.is_absolute() || destination.file_name().is_none() {
            return Err("invalid_destination_path".to_owned());
        }

        // Local first, then peers, then Koru Cloud — see `fetch_for_open`.
        // This used to wait on peer hydration however long it took and never
        // asked the Cloud at all: a file held only on a linked device that
        // was offline, or only on Koru Cloud, left the person on a spinner
        // that nothing would ever end.
        if !self.blob_present(workspace, &hash)? {
            self.fetch_for_open(workspace, &hash)?;
        }

        let parent = destination
            .parent()
            .ok_or_else(|| "invalid_destination_path".to_owned())?
            .to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|_| "destination_unwritable".to_owned())?;
        let temporary = parent.join(format!(".{}.partial", hash.as_str()));

        let copy_hash = hash.clone();
        let copy_temporary = temporary.clone();
        let written = self.with_store(move |store| {
            store
                .verify_blob(workspace, &copy_hash)
                .map_err(|error| error.code().as_str().to_owned())?;
            let mut reader = store
                .open_blob(workspace, &copy_hash)
                .map_err(|error| error.code().as_str().to_owned())?;
            let mut file = std::fs::File::create(&copy_temporary)
                .map_err(|_| "destination_unwritable".to_owned())?;
            let written =
                std::io::copy(&mut reader, &mut file).map_err(|_| "export_failed".to_owned())?;
            // The bytes are on the volume before the name appears, or a reader
            // could open a file the kernel has not finished writing.
            file.sync_all().map_err(|_| "export_failed".to_owned())?;
            Ok(written)
        });
        let written = match written {
            Ok(written) => written,
            Err(code) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(code);
            }
        };
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            let _ = std::fs::remove_file(&temporary);
            let _ = error;
            return Err("destination_unwritable".to_owned());
        }
        Ok(written)
    }

    /// Whether this device already holds the bytes, without reading them.
    fn blob_present(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::BlobHash,
    ) -> Result<bool, String> {
        let hash = hash.clone();
        match self.with_store(move |store| {
            store
                .verify_blob(workspace, &hash)
                .map_err(|error| error.code().as_str().to_owned())
        }) {
            Ok(()) => Ok(true),
            Err(code) if code == "blob_not_found" => Ok(false),
            Err(code) => Err(code),
        }
    }

    /// **A file a person asked to open, from wherever it is.**
    ///
    /// Peers first (§8): a connected device that holds the bytes serves
    /// them now. When no reachable peer can — the holder is offline, nothing
    /// is linked, or every connected peer lacks them — the peer wait ends
    /// there rather than holding the caller, and the blob engine is asked
    /// explicitly. That is §10's request and D50's override: it reopens the
    /// park and carries through the holds a person may waive, because a
    /// tap on *Open file* is exactly the person asking for that one. The
    /// peer hydration keeps waiting in the background and completes when
    /// the holder connects.
    ///
    /// The code answered is the one a person can act on: an offline holder
    /// worth waiting for beats a Cloud reason, and a Cloud reason beats
    /// "nobody reachable has it".
    fn fetch_for_open(
        &self,
        workspace: WorkspaceId,
        hash: &koru_store_core::BlobHash,
    ) -> Result<(), String> {
        let peer = self.tokio().block_on(
            self.network
                .hydrate_blob(workspace, hash.clone())
                .wait_unless_parked(),
        );
        let peer_code = match peer {
            Ok(()) => return Ok(()),
            Err(error) => error.code,
        };
        let Some(blobs) = self.blobs() else {
            return Err(peer_code);
        };
        self.tokio()
            .block_on(blobs.request(workspace, hash.clone()));
        if self.blob_present(workspace, hash)? {
            return Ok(());
        }
        let cloud_reason = blobs.status(workspace).pending_reason;
        Err(match (peer_code.as_str(), cloud_reason) {
            ("blob_waiting_for_peer", _) => peer_code,
            (_, Some(reason)) => reason.to_owned(),
            _ => peer_code,
        })
    }

    // ------------------------------------------------- blob transfer (B5)

    fn blobs_or_refuse(&self) -> Result<&blob::MobileBlobs, String> {
        self.blobs
            .as_ref()
            .ok_or_else(|| "blob_transfer_unavailable".to_owned())
    }

    /// §10's `blob_status`: counts, bytes and the stable pending reason.
    fn blob_status(&self, workspace_id: &str) -> Result<wire::BlobTransferStatus, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let blobs = self.blobs_or_refuse()?;
        Ok(blob::status_message(workspace, &blobs.status(workspace)))
    }

    // ------------------------------------------------ Cloud sync progress

    /// **One Workspace's Cloud sync state**: the phone's answer to the
    /// question the desktop's Manage Profiles asks its Store over
    /// `GET /v1/workspaces/{id}/documents/{manifest}/cloud-sync`.
    ///
    /// Asked about the Workspace's **manifest**, as the desktop asks, because
    /// every field a progress row reads is about the Workspace rather than a
    /// document — so the manifest is resolved here and named in the answer.
    ///
    /// The two halves come from the same places the desktop Store reads, in
    /// process: `sync` from the live permit the Cloud coordinator holds for
    /// this Workspace (the desktop Store's `ControlHost` stage, whose phone
    /// counterpart is the coordinator's own snapshot), and `blobs` from the
    /// engine's `BlobTransferStatusService::status` — the very function the
    /// desktop route calls. A device with no engine answers `blobs: None`,
    /// which the desktop route answers too and which is not zero.
    ///
    /// The per-document halves the desktop's context pane reads (`local`,
    /// `pass`, `document_activity`, `file`) are not asked for here: a
    /// Workspace row reads none of them, and the phone keeps no pass record.
    fn cloud_sync_status(
        &self,
        workspace_id: &str,
    ) -> Result<koru_store_wire::internal::CloudSyncStatus, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let manifest = self.with_store(move |store| {
            store
                .manifest_id(workspace)
                .map_err(|error| error.code().as_str().to_owned())
        })?;
        let account = self
            .account
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let sync = account
            .and_then(|account| account.cloud_workspace_stage(workspace.as_uuid()))
            .map(
                |(stage, error_code)| koru_store_wire::internal::CloudSyncState {
                    enabled: true,
                    stage: Some(stage.to_owned()),
                    error_code: error_code.map(str::to_owned),
                },
            );
        Ok(koru_store_wire::internal::CloudSyncStatus {
            workspace: Some(workspace),
            document: Some(manifest),
            sync,
            blobs: self.blobs.as_ref().map(|blobs| blobs.report(workspace)),
            ..koru_store_wire::internal::CloudSyncStatus::default()
        })
    }

    /// **Which documents are waiting on a file Koru Cloud does not hold
    /// yet**, and which device each waits for — the desktop's
    /// `cloud-sync/waiting`, answered by the same two functions: the engine's
    /// read-only `waiting` (no park is cleared or shortened) and
    /// [`koru_store_api::cloud_waiting::documents`], which turns it into
    /// titles and holder names — each blob's origin merged with the devices
    /// the phone's own network runtime heard advertise it.
    ///
    /// A device with no blob engine answers `blob_transfer_unavailable`,
    /// which a surface says as *unavailable* rather than as an empty list.
    fn cloud_waiting(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<koru_store_wire::CloudWaitingDocument>, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let blobs = self.blobs_or_refuse()?;
        let waiting = self
            .tokio()
            .block_on(blobs.waiting(workspace))
            .map_err(str::to_owned)?;
        let advertised = self.advertised_holders(workspace, &waiting);
        self.name_waiting(workspace, waiting, advertised)
    }

    /// The devices this host's own `network` runtime heard advertise the
    /// waiting blobs this session, as storage ids per hash — the phone's half
    /// of what the desktop reads through `BlobHolderDirectory`. Read, never
    /// copied: the directory stays in the runtime.
    fn advertised_holders(
        &self,
        workspace: WorkspaceId,
        waiting: &[koru_store_api::state::WaitingFiles],
    ) -> HashMap<koru_store_core::model::BlobHash, Vec<String>> {
        if waiting.is_empty() {
            return HashMap::new();
        }
        self.network
            .advertised_blob_holders(workspace, &koru_store_api::cloud_waiting::hashes(waiting))
            .into_iter()
            .map(|(hash, devices)| {
                let devices = devices.iter().map(|device| device.to_storage()).collect();
                (hash, devices)
            })
            .collect()
    }

    /// Titles and holder names for what the engine said is waiting, through
    /// the one mapping the desktop's route also calls.
    fn name_waiting(
        &self,
        workspace: WorkspaceId,
        waiting: Vec<koru_store_api::state::WaitingFiles>,
        advertised: HashMap<koru_store_core::model::BlobHash, Vec<String>>,
    ) -> Result<Vec<koru_store_wire::CloudWaitingDocument>, String> {
        let authorization = match waiting.is_empty() {
            true => None,
            false => self
                .authorization
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        };
        self.with_store(move |store| {
            koru_store_api::cloud_waiting::documents(
                store,
                workspace,
                waiting,
                &advertised,
                authorization.as_ref(),
            )
            .map_err(|error| error.code().as_str().to_owned())
        })
    }

    /// §10's `set_blob_policy`. A policy that **became** permissive is §10's
    /// fifth trigger and runs a pass at once, for every Workspace this device
    /// holds; one that became restrictive is not, and the next pass is what
    /// notices it.
    fn set_blob_policy(&self, policy: &wire::BlobPolicy) -> Result<(), String> {
        let blobs = self.blobs_or_refuse()?;
        let was_held = blobs.policy().hold_now().is_some();
        blobs.policy().replace(policy);
        let workspaces = self
            .with_store(|store| {
                store
                    .workspaces()
                    .map_err(|error| error.code().as_str().to_owned())
            })?
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        blobs.policy_replaced(&self.tokio(), workspaces, was_held);
        Ok(())
    }

    /// §10's `request_blob`: somebody is looking at a placeholder, and D50
    /// lets that through a metered pause.
    fn request_blob(&self, workspace_id: &str, blob_hash: &str) -> Result<(), String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let hash = koru_store_core::BlobHash::parse(blob_hash)
            .map_err(|_| "invalid_blob_id".to_owned())?;
        let blobs = self.blobs_or_refuse()?;
        self.tokio().block_on(blobs.request(workspace, hash));
        Ok(())
    }

    /// **Clear local files**: drop this device's copies of the files Koru
    /// Cloud holds, keep every reference, and say what happened in numbers.
    fn release_blobs(&self, workspace_id: &str) -> Result<wire::ReleasedBlobs, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let blobs = self.blobs_or_refuse()?;
        let report = self
            .tokio()
            .block_on(blobs.release(workspace))
            .map_err(str::to_owned)?;
        Ok(wire::ReleasedBlobs {
            referenced: report.referenced,
            released: report.released,
            bytes_freed: report.bytes_freed,
            kept: report.kept,
            not_held: report.not_held,
            error_code: report.failure_code,
        })
    }

    /// Resume what a killed process left behind: the staging sweep and one
    /// pass. WorkManager's entry point, and nothing that becomes a timer.
    fn recover_blobs(&self, workspace_id: &str) -> Result<(), String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let blobs = self.blobs_or_refuse()?;
        self.tokio().block_on(blobs.recover(workspace));
        Ok(())
    }

    /// The §11 announcement, read rather than pushed: Android has no room
    /// registry, so a surface asks what has arrived since it last looked.
    fn blob_arrivals(
        &self,
        workspace_id: &str,
        after_sequence: u64,
    ) -> Result<wire::BlobArrivals, String> {
        let workspace = match workspace_id.trim() {
            "" => None,
            value => {
                Some(WorkspaceId::parse(value).ok_or_else(|| "invalid_workspace_id".to_owned())?)
            }
        };
        let blobs = self.blobs_or_refuse()?;
        let (rows, sequence) = blobs.arrivals().since(workspace, after_sequence);
        Ok(wire::BlobArrivals {
            arrivals: rows
                .into_iter()
                .map(|arrival| wire::BlobArrival {
                    workspace_id: arrival.workspace.to_string(),
                    blob_hash: arrival.hash,
                    sequence: arrival.sequence,
                    at_millis: arrival.at_millis,
                })
                .collect(),
            sequence,
            error_code: None,
        })
    }

    fn mutate_workspace(
        &self,
        workspace_id: &str,
        action: &str,
        title: &str,
    ) -> Result<(), String> {
        let id =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let action = action.to_owned();
        let title = title.to_owned();
        let result = self.with_store(move |store| {
            match action.as_str() {
                "rename" => store
                    .rename_workspace(id, &title, Timestamp::from_millis(now_millis()))
                    .map(|_| ()),
                "archive" | "unarchive" => (|| {
                    let workspace = store.workspace(id)?;
                    store
                        .set_workspace_appearance(
                            id,
                            workspace.icon.as_deref(),
                            workspace.color.as_deref(),
                            workspace.pinned,
                            action == "archive",
                        )
                        .map(|_| ())
                })(),
                "delete" => store
                    .delete_workspace(id, Timestamp::from_millis(now_millis()))
                    .map(|_| ()),
                "restore" => store.restore_workspace(id).map(|_| ()),
                _ => return Err("invalid_workspace_action".to_owned()),
            }
            .map_err(|error| error.code().as_str().to_owned())
        });
        if result.is_ok() {
            // Workspace metadata changes are durable Store commits just like
            // document edits. Wake the data plane immediately so a mobile
            // rename does not wait for an unrelated document change before it
            // is announced to peers.
            self.network.notify_local_change();
            self.wake_cloud();
        }
        result
    }

    fn mutate_profile(&self, profile_id: &str, action: &str, value: &str) -> Result<(), String> {
        let id = koru_store_core::ProfileId::parse(profile_id)
            .ok_or_else(|| "invalid_profile_id".to_owned())?;
        let action = action.to_owned();
        let value = value.to_owned();
        let result = self.with_store(move |store| {
            match action.as_str() {
                "rename" => store.rename_profile(id, &value).map(|_| ()),
                _ => return Err("invalid_profile_action".to_owned()),
            }
            .map_err(|error| error.code().as_str().to_owned())
        });
        if result.is_ok() {
            // A profile rename is a durable CRDT commit like any other
            // change; wake the data plane so peers pull it now rather than
            // on the next unrelated edit.
            self.network.notify_local_change();
            self.wake_cloud();
        }
        result
    }

    /// Register an external file as a `kind == "file"` document.
    ///
    /// The bytes arrive as a path rather than a byte array: Android has
    /// already streamed the sender's content URI to app-private storage while
    /// its temporary grant was alive, and a share can be a video. Copying that
    /// through a JNI array would hold the whole payload in the JVM heap on top
    /// of this one.
    ///
    /// The shape is the desktop's, from `koru-import-files`: put the blob,
    /// build a one-node `WorkspaceDoc` whose root is `Content::Blob`, then
    /// register it in the manifest with its MIME. Anything else produces a row
    /// the Files tab lists and no reader can open.
    ///
    /// A non-empty `note` attaches the file's one note in the same call, with
    /// the text as its first paragraph — see [`Self::ensure_file_note`].
    fn import_file(
        &self,
        workspace_id: &str,
        name: &str,
        mime: &str,
        path: &str,
        note: Option<&str>,
    ) -> Result<String, String> {
        let workspace =
            WorkspaceId::parse(workspace_id).ok_or_else(|| "invalid_workspace_id".to_owned())?;
        let name = name.trim();
        if name.is_empty() {
            return Err("invalid_file_name".to_owned());
        }
        let bytes = std::fs::read(path).map_err(|_| "file_unreadable".to_owned())?;
        if bytes.is_empty() {
            return Err("empty_file".to_owned());
        }
        let at = Timestamp::from_millis(now_millis());
        // The storing device names itself as the blob's origin, when trust is
        // initialized. Before first pairing there is no device identity to
        // record, and none is better than a wrong one.
        let origin_device = self
            .trust
            .lock()
            .map_err(|_| "trust_lock".to_owned())?
            .as_ref()
            .and_then(|trust| {
                trust
                    .local_descriptor()
                    .ok()
                    .map(|descriptor| descriptor.device_id.to_storage())
            });
        let name = name.to_owned();
        let mime = mime.to_owned();
        let note = note.map(str::to_owned);
        let id = self.with_store(move |store| {
            let stored = store
                .put_blob(
                    workspace,
                    &bytes,
                    &mime,
                    Some(&name),
                    at,
                    origin_device.as_deref(),
                )
                .map_err(|error| error.code().as_str().to_owned())?;
            let id = DocumentId::new();
            let mut root = koru_core::Node::file(
                &name,
                koru_core::node::BlobHash(stored.hash.as_str().to_owned()),
                stored.mime.clone(),
            )
            .map_err(|_| "invalid_file_node".to_owned())?;
            root.id = koru_core::NodeId::from_uuid(id.as_uuid());
            let mut doc = koru_core::WorkspaceDoc::create(
                koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
                &name,
            )
            .map_err(|_| "document_create_failed".to_owned())?;
            doc.insert(koru_core::schema::Parent::Root, root)
                .map_err(|_| "document_create_failed".to_owned())?;
            let snapshot = doc
                .export_snapshot()
                .map_err(|_| "document_export_failed".to_owned())?;

            store
                .import_document_at_with_mime(
                    workspace,
                    id,
                    &name,
                    koru_core::explore::KIND_FILE,
                    Some(&stored.mime),
                    None,
                    None,
                    snapshot,
                    at,
                )
                .map_err(|error| error.code().as_str().to_owned())?;
            if let Some(text) = note
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                let note = store
                    .ensure_file_note(workspace, id, at)
                    .map_err(|error| error.code().as_str().to_owned())?
                    .id;
                seed_note_document(store, workspace, note, &name, Some(text), at)?;
            }
            Ok(id)
        })?;
        // The blob and its document are one durable commit as far as a peer is
        // concerned; announce them together rather than on the next unrelated
        // edit.
        self.network.notify_local_change();
        self.wake_cloud();
        Ok(id.to_storage())
    }

    fn mutate_document(&self, document_id: &str, action: &str, title: &str) -> Result<(), String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        let at = Timestamp::from_millis(now_millis());
        let action = action.to_owned();
        let title = title.to_owned();
        let result = self.with_store(move |store| {
            let workspace = store
                .locate(document)
                .map_err(|error| error.code().as_str().to_owned())?
                .ok_or_else(|| "document_not_found".to_owned())?;
            let entry = store
                .documents(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|entry| entry.id == document)
                .ok_or_else(|| "document_not_found".to_owned())?;

            match action.as_str() {
                "rename" => {
                    if title.trim().is_empty() {
                        return Err("invalid_document_title".to_owned());
                    }
                    // A Koru document's row-zero title is authoritative. Files do
                    // not contain the node model, so their manifest name stands
                    // alone. In both cases the manifest projection is committed
                    // through the same durable Store path afterwards.
                    if entry.kind != "file" {
                        let snapshot = store
                            .snapshot(workspace, document)
                            .map_err(|error| error.code().as_str().to_owned())?;
                        let mut model = koru_core::WorkspaceDoc::from_snapshot(&snapshot)
                            .map_err(|_| "invalid_document_snapshot".to_owned())?;
                        let before = model.version();
                        // The third open seam, and the same one-line
                        // guarantee. `WorkspaceDoc::create` is the only writer
                        // of the workspace id and the store never calls it — it
                        // hands out an empty `LoroDoc` — so a document nothing
                        // has stamped gains it here, from the id the store
                        // itself answered `locate` with. Nothing on this path
                        // runs a task command today, which is exactly why it is
                        // worth a line now: the next one that does will not have
                        // to find this out the way the desktop and the bridge
                        // did. After `before`, so the stamp travels with the
                        // rename's update rather than being written and lost.
                        model
                            .stamp_workspace_id(koru_core::WorkspaceId::from_uuid(
                                workspace.as_uuid(),
                            ))
                            .map_err(|_| "invalid_document_snapshot".to_owned())?;
                        // And which document it is, from the id this call was
                        // made with. Same seam, same reason, and it is the
                        // middle segment of every `task:` reference a future
                        // command here would mint.
                        model
                            .stamp_document_id(koru_core::DocumentId::from_uuid(document.as_uuid()))
                            .map_err(|_| "invalid_document_snapshot".to_owned())?;
                        let root = model
                            .children(koru_core::schema::Parent::Root)
                            .map_err(|_| "invalid_document_snapshot".to_owned())?
                            .into_iter()
                            .find(|id| model.kind_of(*id) == Some(koru_core::Kind::Document))
                            .ok_or_else(|| "document_title_not_found".to_owned())?;
                        model
                            .set_text(root, title.trim())
                            .map_err(|_| "invalid_document_title".to_owned())?;
                        let update = model
                            .export_updates_since(&before)
                            .map_err(|_| "document_export_failed".to_owned())?;
                        store
                            .apply_update(
                                workspace,
                                document,
                                update,
                                at,
                                koru_store_core::ports::UpdateOrigin::Store,
                            )
                            .map_err(|error| error.code().as_str().to_owned())?;
                        store
                            .flush(workspace, document, at)
                            .map_err(|error| error.code().as_str().to_owned())?;
                    }
                    store
                        .rename_document_entry(workspace, document, title.trim(), at)
                        .map(|_| ())
                        .map_err(|error| error.code().as_str().to_owned())
                }
                "trash" => store
                    .trash_document_entry(workspace, document, at, None)
                    .map(|_| ())
                    .map_err(|error| error.code().as_str().to_owned()),
                _ => Err("invalid_document_action".to_owned()),
            }
        });
        if result.is_ok() {
            self.network.notify_local_change();
            self.wake_cloud();
        }
        result
    }

    /// The one note a file carries, created on first ask. The store registers
    /// it empty; this host then seeds the `Kind::Document` root the way the
    /// desktop seeds every store-created page, because the store deliberately
    /// cannot build document content (ADR-80). Idempotent: an existing note
    /// comes back as-is, and an empty-but-registered one is seeded rather
    /// than duplicated.
    fn ensure_file_note(&self, document_id: &str) -> Result<String, String> {
        let document =
            DocumentId::parse(document_id).ok_or_else(|| "invalid_document_id".to_owned())?;
        let at = Timestamp::from_millis(now_millis());
        let note = self.with_store(move |store| {
            let workspace = store
                .locate(document)
                .map_err(|error| error.code().as_str().to_owned())?
                .ok_or_else(|| "document_not_found".to_owned())?;
            let entry = store
                .documents(workspace)
                .map_err(|error| error.code().as_str().to_owned())?
                .into_iter()
                .find(|entry| entry.id == document && entry.trashed_at.is_none())
                .ok_or_else(|| "document_not_found".to_owned())?;
            let note = store
                .ensure_file_note(workspace, document, at)
                .map_err(|error| error.code().as_str().to_owned())?
                .id;
            seed_note_document(store, workspace, note, &entry.title, None, at)?;
            Ok(note)
        })?;
        self.network.notify_local_change();
        self.wake_cloud();
        Ok(note.to_storage())
    }

    fn start_pairing(&self) -> PairingOfferResult {
        let descriptor = {
            let guard = self.trust.lock().unwrap_or_else(|error| error.into_inner());
            let Some(trust) = guard.as_ref() else {
                return PairingOfferResult {
                    state: "failed",
                    qr_payload: None,
                    expires_in_seconds: 0,
                    error_code: Some("device_identity_mismatch".into()),
                };
            };
            match trust.local_descriptor() {
                Ok(value) => value,
                Err(error) => {
                    return PairingOfferResult {
                        state: "failed",
                        qr_payload: None,
                        expires_in_seconds: 0,
                        error_code: Some(error),
                    };
                }
            }
        };
        let session = koru_pairing::InvitationSession::new(descriptor, None, Instant::now());
        let payload = session.invitation.encode_link_url();
        self.network.begin_pairing(session.invitation.clone());
        *self
            .pairing
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(session);
        match payload {
            Ok(qr_payload) => PairingOfferResult {
                state: "displaying",
                qr_payload: Some(qr_payload),
                expires_in_seconds: koru_pairing::INVITATION_LIFETIME.as_secs() as u16,
                error_code: None,
            },
            Err(_) => PairingOfferResult {
                state: "failed",
                qr_payload: None,
                expires_in_seconds: 0,
                error_code: Some("pairing_encoding".into()),
            },
        }
    }

    fn cancel_pairing(&self) {
        if let Some(session) = self.pairing.lock().unwrap().as_mut() {
            session.cancel();
        }
        self.network.cancel_pairing();
    }

    fn devices(&self) -> DevicesResult {
        let guard = self.trust.lock().unwrap();
        let Some(trust) = guard.as_ref() else {
            return DevicesResult {
                devices: Vec::new(),
                this_device: None,
                sync_event: None,
                error_code: Some(self.identity_error_code()),
                // The network runtime is alive even without an identity, so
                // the relay row can still say what it honestly reads.
                relay: Some(relay_status(self.network.status().relay)),
                relink_available: false,
            };
        };
        let view = match trust.view() {
            Ok(view) => view,
            Err(error) => {
                return DevicesResult {
                    devices: Vec::new(),
                    this_device: None,
                    sync_event: None,
                    error_code: Some(error),
                    relay: Some(relay_status(self.network.status().relay)),
                    relink_available: false,
                };
            }
        };
        let network = self.network.status();
        // The mesh, not just the directly paired ring: every device in this
        // device's connected component is listed, a transitive one marked
        // with the direct peer that introduced it. Names resolve through
        // gossiped renames, so a device appears under its current name, and
        // builds the same way, so the list says what each one runs.
        let local = view.local_device.device_id;
        let local_build = view.build_of(local);
        let authorized = koru_device::Authorization::devices(&view);
        let relink_available = has_unrestored_local_relationship(&view, &authorized);
        let devices = authorized
            .into_iter()
            .map(|peer| {
                let (connected, connection) = match network.peers.get(&peer.device_id) {
                    Some(koru_network_iroh::PeerConnectionState::Connected { path }) => {
                        (true, connection_path(*path))
                    }
                    _ => (false, None),
                };
                let build = view.build_of(peer.device_id);
                let drift = match (&local_build, &build) {
                    (Some(ours), Some(theirs)) => Some(ours.drift_from(theirs)),
                    _ => None,
                };
                let last_sync_millis = network
                    .last_state_change_millis
                    .get(&peer.device_id)
                    .copied();
                LinkedDeviceResult {
                    device_id: peer.device_id.to_storage(),
                    display_name: view
                        .display_name_of(peer.device_id)
                        .unwrap_or_else(|| "Unknown device".into()),
                    linked_at_millis: view
                        .relationship(peer.relationship_id)
                        .map_or(0, |relationship| relationship.created_at_millis),
                    connected,
                    last_seen_millis: network.last_seen_millis.get(&peer.device_id).copied(),
                    introduced_by: peer.via.and_then(|via| view.display_name_of(via)),
                    build_drift: drift.map(|drift| drift.as_str().to_owned()),
                    build_severity: drift
                        .and_then(|drift| drift.severity().as_str())
                        .map(str::to_owned),
                    update_needed_on: drift
                        .and_then(koru_device::BuildDrift::update_needed_on)
                        .map(str::to_owned),
                    build_label: build.map(|build| build.label()),
                    connection: connection.map(str::to_owned),
                    last_sync_millis,
                    history: Vec::new(),
                }
            })
            .collect();
        DevicesResult {
            devices,
            this_device: Some(LinkedDeviceResult {
                device_id: local.to_storage(),
                display_name: view
                    .display_name_of(local)
                    .unwrap_or_else(|| view.local_device.display_name.clone()),
                linked_at_millis: view.local_device.created_at_millis,
                connected: true,
                last_seen_millis: None,
                introduced_by: None,
                build_label: local_build.map(|build| build.label()),
                build_drift: None,
                build_severity: None,
                update_needed_on: None,
                connection: None,
                last_sync_millis: None,
                history: Vec::new(),
            }),
            sync_event: network.last_sync_event.map(|event| MobileSyncEvent {
                sequence: event.sequence,
                at_millis: event.at_millis,
                direction: koru_replication::sync_event_direction(event.stage),
                peer: event.peer.to_storage(),
                stage: event.stage,
                workspace_id: event.workspace.map(|id| id.to_storage()),
                items: event.items,
                error_code: event.error_code,
            }),
            error_code: None,
            relay: Some(relay_status(network.relay)),
            relink_available,
        }
    }

    fn state_history(
        &self,
        device_id: &str,
        before: Option<i64>,
    ) -> Result<MobileStateHistory, String> {
        let peer = DeviceId::parse(device_id).ok_or_else(|| "invalid_device_id".to_owned())?;
        let page = self
            .network
            .state_change_page(peer, before)
            .map_err(|error| error.code.to_owned())?;
        Ok(MobileStateHistory {
            device_id: device_id.to_owned(),
            events: page
                .events
                .into_iter()
                .map(|event| MobileStateEvent {
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
                })
                .collect(),
            next_before: page.next_before,
        })
    }

    /// Rename this device. The record gossips like any other; authorization
    /// is untouched, so no connection is rebuilt for a name.
    fn rename(&self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("empty_name".into());
        }
        if name.chars().count() > koru_device::MAX_DEVICE_NAME_CHARS {
            return Err("name_too_long".into());
        }
        self.trust
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "device_identity_mismatch".to_owned())?
            .rename_local(name.to_owned(), &self.signing_key)?;
        // Publishing follows trust_revision, not the database revision alone.
        // Make the persisted name visible to gossip without waiting for another
        // trust event, anti-entropy, or a restart.
        self.refresh_authorization();
        Ok(())
    }

    /// What this device tells its peers it runs. The app passes it in —
    /// Gradle's `versionName`, the build type, the OS — because the native
    /// library has no build info of its own. Writes a record only when the
    /// description changed; the revision bump then gossips it like a rename.
    #[cfg(test)]
    fn advertise_build(&self, json: &str) -> Result<(), String> {
        let payload: koru_device::BuildPayload =
            serde_json::from_str(json).map_err(|_| "invalid_build_payload".to_owned())?;
        self.advertise_build_payload(payload)
    }

    fn advertise_build_payload(
        &self,
        mut payload: koru_device::BuildPayload,
    ) -> Result<(), String> {
        // The protocol number is this library's, whatever the app said: an
        // APK cannot claim a number its native code does not speak.
        payload.peer_protocol = Some(koru_replication::PEER_PROTOCOL_VERSION);
        let written = self
            .trust
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "device_identity_mismatch".to_owned())?
            .advertise_build(&payload, &self.signing_key)?;
        if written {
            tracing::info!(build = %payload.label(), "build advertised");
            self.refresh_authorization();
        }
        Ok(())
    }

    fn unlink(&self, value: &str) -> Result<(), String> {
        let peer = DeviceId::parse(value).ok_or_else(|| "invalid_device_id".to_owned())?;
        let records = self
            .trust
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "device_identity_mismatch".to_owned())?
            .revoke(peer, &self.signing_key)?;
        for record in records {
            self.network.send_trust_record(peer, record);
        }
        self.refresh_authorization();
        Ok(())
    }

    fn link(&self, code: &str) -> LinkResult<'static> {
        if self
            .linking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return failed("pairing_in_progress");
        }
        let result = self.link_inner(code);
        self.linking.store(false, Ordering::Release);
        result
    }

    fn link_inner(&self, code: &str) -> LinkResult<'static> {
        let invitation = match koru_pairing::PairingInvitationV1::decode_link(code) {
            Ok(value) => value,
            Err(_) => return failed("invalid_link_code"),
        };
        let descriptor = {
            let guard = self.trust.lock().unwrap();
            let Some(trust) = guard.as_ref() else {
                return failed("device_identity_mismatch");
            };
            match trust.local_descriptor() {
                Ok(value) => value,
                Err(error) => {
                    return LinkResult {
                        state: "failed",
                        peer_name: None,
                        error_code: Some(error),
                    };
                }
            }
        };
        let join = match koru_pairing::PairingJoinV1::sign(
            &invitation,
            descriptor,
            &self.signing_key,
            now_millis(),
        ) {
            Ok(value) => value,
            Err(_) => return failed("pairing_signature"),
        };
        self.network.join_pairing(join);
        let deadline = Instant::now() + Duration::from_secs(125);
        while Instant::now() < deadline {
            for event in self.network.take_pairing_events() {
                match event {
                    koru_sync_control::PairingEvent::Completed { prepared } => {
                        let committed = self
                            .trust
                            .lock()
                            .unwrap()
                            .as_mut()
                            .map(|trust| trust.commit_pairing(&prepared));
                        if !matches!(committed, Some(Ok(()))) {
                            return failed("pairing_commit");
                        }
                        self.refresh_authorization();
                        return LinkResult {
                            state: "linked",
                            peer_name: Some(prepared.transcript.inviter.display_name.clone()),
                            error_code: None,
                        };
                    }
                    koru_sync_control::PairingEvent::Failed { code } => return failed_owned(code),
                    koru_sync_control::PairingEvent::TrustRecordReceived {
                        delivery_id,
                        from,
                        record,
                    } => {
                        let accepted =
                            self.trust.lock().unwrap().as_mut().is_some_and(|trust| {
                                trust.apply_direct_trust(from, *record).is_ok()
                            });
                        self.network.complete_trust_record(delivery_id, accepted);
                        self.refresh_authorization_if_changed();
                    }
                    koru_sync_control::PairingEvent::TrustRecordsReceived {
                        delivery_id,
                        records,
                        ..
                    } => {
                        let accepted = self
                            .trust
                            .lock()
                            .unwrap()
                            .as_mut()
                            .is_some_and(|trust| trust.apply_remote_records(records).is_ok());
                        self.network.complete_trust_record(delivery_id, accepted);
                        self.refresh_authorization_if_changed();
                    }
                    koru_sync_control::PairingEvent::JoinRequested { .. } => {}
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        failed("pairing_timeout")
    }

    fn refresh_authorization_if_changed(&self) {
        let changed = self.trust.lock().unwrap().as_ref().is_some_and(|trust| {
            trust
                .revision()
                .is_ok_and(|revision| revision != self.trust_revision.load(Ordering::Acquire))
        });
        if changed {
            self.refresh_authorization();
        }
    }

    /// Hand a snapshot to the network runtime, and keep a copy to read back.
    fn install_authorization(&self, snapshot: koru_sync_control::AuthorizationSnapshot) {
        *self.authorization.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot.clone());
        self.network.replace_authorization(snapshot);
    }

    fn refresh_authorization(&self) {
        let guard = self.trust.lock().unwrap();
        let Some(trust) = guard.as_ref() else {
            // Degraded identity: no trust, nothing to authorize — and no
            // reason to churn connections over it.
            return;
        };
        let view = match trust.view() {
            Ok(view) => view,
            Err(_) => {
                if let Ok(descriptor) = trust.local_descriptor() {
                    self.install_authorization(koru_sync_control::AuthorizationSnapshot {
                        session_id: koru_sync_control::ControlSessionId::new(),
                        sequence: 1,
                        local_device: descriptor.device_id,
                        peers: Vec::new(),
                        local_build: None,
                    });
                }
                return;
            }
        };
        let workspace_rows = self
            .with_store(|store| {
                store
                    .workspaces()
                    .map_err(|error| error.code().as_str().to_owned())
            })
            .unwrap_or_default();
        let deleted_profiles = view.deleted_profiles();
        let workspaces = workspace_rows
            .into_iter()
            .filter(|workspace| !deleted_profiles.contains(&workspace.owner))
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let local_profiles = self
            .with_store(|store| {
                store
                    .profiles()
                    .map_err(|error| error.code().as_str().to_owned())
            })
            .unwrap_or_default()
            .into_iter()
            .map(|profile| profile.id)
            .collect::<std::collections::BTreeSet<_>>();
        let peers = koru_device::Authorization::devices(&view)
            .into_iter()
            .map(|peer| {
                let mut authorized_profiles = peer.authorized_profiles.clone();
                let all_data = peer.all_data && deleted_profiles.is_empty();
                if peer.all_data && !deleted_profiles.is_empty() {
                    authorized_profiles.extend(local_profiles.iter().copied());
                    authorized_profiles.retain(|profile| !deleted_profiles.contains(profile));
                }
                koru_sync_control::AuthorizedPeer {
                    relationship_id: peer.relationship_id,
                    device_id: peer.device_id,
                    endpoint_id: peer.endpoint_id,
                    all_data,
                    authorized_profiles: authorized_profiles.into_iter().collect(),
                    workspaces: if peer.all_data {
                        workspaces.clone()
                    } else {
                        Vec::new()
                    },
                    via: peer.via,
                    display_name: view.display_name_of(peer.device_id),
                    build: view.build_of(peer.device_id),
                }
            })
            .collect::<Vec<_>>();
        self.install_authorization(koru_sync_control::AuthorizationSnapshot {
            session_id: koru_sync_control::ControlSessionId::new(),
            sequence: view.effective_revision.max(1),
            local_device: view.local_device.device_id,
            peers: peers.clone(),
            local_build: view.build_of(view.local_device.device_id),
        });
        self.trust_revision
            .store(view.effective_revision, Ordering::Release);
        for peer in peers {
            self.network.connect_peer(peer.device_id);
        }
    }

    fn start_maintenance(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("koru mobile trust".into())
            .spawn(move || {
                // Device-list gossip state: which peer was last sent which
                // trust revision, and when the next unconditional re-send is
                // due.
                let mut gossiped = std::collections::HashMap::<DeviceId, u64>::new();
                let mut gossip_antientropy = Instant::now() + TRUST_GOSSIP_ANTIENTROPY;
                loop {
                    let Some(host) = weak.upgrade() else { return };
                    if host.linking.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    for event in host.network.take_pairing_events() {
                        match event {
                            koru_sync_control::PairingEvent::JoinRequested { attempt_id, join } => {
                                let prepared = (|| {
                                    let mut pairing = host.pairing.lock().unwrap();
                                    let session =
                                        pairing.as_mut().ok_or("invitation_unavailable")?;
                                    session
                                        .claim(
                                            &join.secret,
                                            join.transcript.joiner.device_id,
                                            Instant::now(),
                                        )
                                        .map_err(|_| "invalid_join")?;
                                    session.preparing();
                                    join.complete(&session.invitation, &host.signing_key)
                                        .map_err(|_| "pairing_signature")
                                })();
                                if let Ok(prepared) = prepared {
                                    let committed = host
                                        .trust
                                        .lock()
                                        .unwrap()
                                        .as_mut()
                                        .map(|trust| trust.commit_pairing(&prepared));
                                    if matches!(committed, Some(Ok(()))) {
                                        host.network.complete_pairing(attempt_id, prepared);
                                        if let Some(session) = host.pairing.lock().unwrap().as_mut()
                                        {
                                            session.succeed();
                                        }
                                        host.refresh_authorization();
                                    }
                                }
                            }
                            koru_sync_control::PairingEvent::TrustRecordReceived {
                                delivery_id,
                                from,
                                record,
                            } => {
                                let outcome = host
                                    .trust
                                    .lock()
                                    .unwrap()
                                    .as_mut()
                                    .map(|trust| trust.apply_direct_trust(from, *record))
                                    .unwrap_or_else(|| Err("device_identity_mismatch".into()));
                                host.network
                                    .complete_trust_record(delivery_id, outcome.is_ok());
                                host.refresh_authorization_if_changed();
                            }
                            koru_sync_control::PairingEvent::TrustRecordsReceived {
                                delivery_id,
                                records,
                                ..
                            } => {
                                let outcome = host
                                    .trust
                                    .lock()
                                    .unwrap()
                                    .as_mut()
                                    .map(|trust| trust.apply_remote_records(records))
                                    .unwrap_or_else(|| Err("device_identity_mismatch".into()));
                                host.network
                                    .complete_trust_record(delivery_id, outcome.is_ok());
                                host.refresh_authorization_if_changed();
                            }
                            _ => {}
                        }
                    }
                    // Replicate the device list itself, the same way the data
                    // replicates: the whole trust-record set goes to every
                    // connected mesh peer whenever the local revision moved,
                    // and to everyone again every few minutes as
                    // anti-entropy. Applying a batch bumps the revision, so
                    // gossip re-fires and the mesh converges on its own. A
                    // degraded identity has no trust store to share.
                    let revision = {
                        let guard = host.trust.lock().unwrap();
                        guard
                            .as_ref()
                            .map(|_| host.trust_revision.load(Ordering::Acquire))
                    };
                    let Some(revision) = revision else {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    };
                    if Instant::now() >= gossip_antientropy {
                        gossip_antientropy = Instant::now() + TRUST_GOSSIP_ANTIENTROPY;
                        gossiped.clear();
                    }
                    let network = host.network.status();
                    for (device, state) in &network.peers {
                        if !matches!(
                            state,
                            koru_network_iroh::PeerConnectionState::Connected { .. }
                        ) || gossiped.get(device) == Some(&revision)
                        {
                            continue;
                        }
                        let mut records = {
                            let guard = host.trust.lock().unwrap();
                            guard
                                .as_ref()
                                .map(|trust| trust.records().unwrap_or_default())
                                .unwrap_or_default()
                        };
                        if records.is_empty() {
                            break;
                        }
                        koru_device::sort_for_gossip(&mut records);
                        // Mark the batch as sent only when it was really
                        // queued: a snapshot replacement can close this
                        // peer's connection in the same instant, and an
                        // unmarked batch is retried on the next pass.
                        let delivered = host.network.send_trust_records(*device, records);
                        if delivered.recv_timeout(Duration::from_millis(500)) == Ok(true) {
                            gossiped.insert(*device, revision);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok();
    }
}

/// **A note the store made is empty, and Koru's is not.**
///
/// `Store::ensure_file_note` registers the note with nothing inside it — the
/// store deliberately does not know what is inside a document (ADR-80). The
/// desktop's answer for the same shape is `koru_workspace`'s
/// `seed_new_document`: a `Kind::Document` root node — its text is the title,
/// and it is row 0 of the body (ADR-91) — with the note's first paragraph
/// under it. Seeded here rather than by the store, and only while the
/// document is still empty, so a note with content is never re-seeded and an
/// empty-but-registered one is healed rather than duplicated.
///
/// **First it folds.** The manifest entry syncs apart from the document, so
/// two devices can each find the note empty and each seed it; there is no
/// mergeable `LoroTree::create`, so the merge keeps both roots.
/// `WorkspaceDoc::unify_document_roots` folds them back to one before the
/// empty check, which would otherwise — rightly — skip a doubled note and
/// leave the split standing.
fn seed_note_document(
    store: &mut koru_store_server::WorkerStore,
    workspace: WorkspaceId,
    note: DocumentId,
    title: &str,
    text: Option<&str>,
    at: Timestamp,
) -> Result<(), String> {
    let snapshot = store
        .snapshot(workspace, note)
        .map_err(|error| error.code().as_str().to_owned())?;
    let mut model = koru_core::WorkspaceDoc::from_snapshot(&snapshot)
        .map_err(|_| "invalid_document_snapshot".to_owned())?;
    let before = model.version();
    let folded = model
        .unify_document_roots()
        .map_err(|_| "invalid_document_snapshot".to_owned())?;
    if !model
        .walk()
        .map_err(|_| "invalid_document_snapshot".to_owned())?
        .is_empty()
    {
        if !folded {
            return Ok(());
        }
    } else {
        let root = model
            .insert(
                koru_core::schema::Parent::Root,
                koru_core::Node::document(title)
                    .map_err(|_| "document_create_failed".to_owned())?,
            )
            .map_err(|_| "document_create_failed".to_owned())?;
        if let Some(text) = text {
            let line = koru_core::Node::paragraph(text)
                .map_err(|_| "document_create_failed".to_owned())?;
            model
                .insert(koru_core::schema::Parent::Node(root), line)
                .map_err(|_| "document_create_failed".to_owned())?;
        }
        model
            .stamp_schema_version()
            .map_err(|_| "document_export_failed".to_owned())?;
    }
    let update = model
        .export_updates_since(&before)
        .map_err(|_| "document_export_failed".to_owned())?;
    store
        .apply_update(
            workspace,
            note,
            update,
            at,
            koru_store_core::ports::UpdateOrigin::Store,
        )
        .map_err(|error| error.code().as_str().to_owned())?;
    store
        .flush(workspace, note, at)
        .map(|_| ())
        .map_err(|error| error.code().as_str().to_owned())
}

fn document_view(
    model: &koru_core::WorkspaceDoc,
    workspace: WorkspaceId,
    document: DocumentId,
    listed_title: String,
    revision: Vec<u8>,
    blob_sizes: &HashMap<String, u64>,
) -> Result<DocumentView, String> {
    use koru_core::schema::Parent;

    let order = model.walk_all().map_err(|_| "invalid_document_snapshot")?;
    let root = order
        .iter()
        .map(|(id, _)| *id)
        .find(|id| model.kind_of(*id) == Some(koru_core::Kind::Document))
        .ok_or_else(|| "document_title_not_found".to_owned())?;
    let title = model
        .read(root)
        .ok()
        .and_then(|node| node.content.as_text().map(str::to_owned))
        .filter(|title| !title.is_empty())
        .unwrap_or(listed_title);

    let mut inside = false;
    let mut blocks = Vec::new();
    for (id, depth) in order {
        if id == root {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if depth == 0 {
            break;
        }
        let node = model
            .read(id)
            .map_err(|_| "invalid_document_block".to_owned())?;
        if node.kind == koru_core::Kind::Trash {
            continue;
        }
        let parent_id = match model.parent(id).map_err(|_| "invalid_document_parent")? {
            Parent::Node(parent) if parent != root => Some(parent.to_storage()),
            Parent::Node(_) | Parent::Root => None,
        };
        blocks.push(view_block(
            model, &node, parent_id, root, document, blob_sizes,
        )?);
    }

    Ok(DocumentView {
        id: document.to_storage(),
        workspace_id: workspace.to_storage(),
        title,
        revision: revision_number(&revision),
        blocks,
    })
}

fn view_block(
    model: &koru_core::WorkspaceDoc,
    node: &koru_core::Node,
    parent_id: Option<String>,
    root: koru_core::NodeId,
    document: DocumentId,
    blob_sizes: &HashMap<String, u64>,
) -> Result<ViewBlock, String> {
    use koru_core::Content;

    let (rich_text, blob, reference, kind) = match &node.content {
        Content::Text { .. } => {
            let rich = matches!(
                node.kind,
                koru_core::Kind::Paragraph
                    | koru_core::Kind::Quote
                    | koru_core::Kind::Task
                    | koru_core::Kind::Code
                    | koru_core::Kind::Table
                    | koru_core::Kind::Contact
            )
            .then(|| view_rich_text(model, node, root, document))
            .transpose()?;
            (rich, None, None, node.kind.as_str().to_owned())
        }
        Content::Blob {
            hash,
            mime,
            width,
            height,
            ..
        } => (
            None,
            Some(ViewBlob {
                id: hash.0.clone(),
                mime_type: mime.clone(),
                size: blob_sizes.get(&hash.0).copied().unwrap_or(0),
                width: *width,
                height: *height,
            }),
            None,
            node.kind.as_str().to_owned(),
        ),
        Content::Embed { target } => (
            None,
            None,
            Some(view_reference(*target, "embed", root, document)),
            "embed".to_owned(),
        ),
    };

    let meta = &node.meta;
    let alignment = match meta.align {
        Some(koru_core::Align::Center) => "center",
        Some(koru_core::Align::End) => "end",
        Some(koru_core::Align::Start) | None => "start",
    };
    let (image_width, image_height) = meta.image_size.unzip();
    Ok(ViewBlock {
        id: node.id.to_storage(),
        parent_id,
        kind,
        properties: ViewBlockProperties {
            name: meta.name.clone(),
            icon: meta.icon.clone(),
            color: meta.colour.clone(),
            done: meta.done,
            alignment,
            heading_level: meta.heading.map(|heading| heading.level()),
            list_style: meta.list_style.map(koru_core::ListStyle::as_str),
            image_width,
            image_height,
            language: meta.language.clone(),
        },
        rich_text,
        blob,
        reference,
    })
}

fn view_rich_text(
    model: &koru_core::WorkspaceDoc,
    node: &koru_core::Node,
    root: koru_core::NodeId,
    document: DocumentId,
) -> Result<ViewRichText, String> {
    use koru_core::AttrValue;

    let runs = model
        .runs(node.id)
        .map_err(|_| "invalid_document_text".to_owned())?;
    let text = runs.iter().map(|run| run.text.as_str()).collect();
    let mut marks = Vec::new();
    let mut inline_objects = Vec::new();
    for run in &runs {
        let start = run.start.0;
        let end = run.end.0;
        let mut mark = |present: bool, kind: &'static str, value: Option<String>| {
            if present && start < end {
                marks.push(ViewMark {
                    start_utf8: start,
                    end_utf8: end,
                    kind,
                    value,
                });
            }
        };
        mark(run.bold, "bold", None);
        mark(run.italic, "italic", None);
        mark(run.underline, "underline", None);
        mark(run.strike, "strike", None);
        mark(run.code, "code", None);
        mark(
            run.style.script.is_some(),
            match run.style.script.map(|script| script.as_str()) {
                Some("sup") => "superscript",
                _ => "subscript",
            },
            None,
        );
        mark(
            run.style.size.is_some(),
            "size",
            run.style.size.map(|size| size.to_string()),
        );
        mark(
            run.style.highlight.is_some(),
            "highlight",
            run.style.highlight.clone(),
        );

        let Some(attribute_id) = run.pill else {
            continue;
        };
        let Some(attribute) = node
            .content
            .attrs()
            .iter()
            .find(|attribute| attribute.id == attribute_id)
        else {
            continue;
        };
        let reference = match &attribute.value {
            AttrValue::Ref(target) => {
                view_reference(*target, attribute.role.as_str(), root, document)
            }
            // A stable document reference (ADR-127) names its target
            // directly — and under the run role it names a run (P1.18).
            AttrValue::Document(reference) if attribute.role == RUN_PILL_ROLE => {
                ViewReference::Run {
                    document_id: reference.document.to_storage(),
                }
            }
            AttrValue::Document(reference) => ViewReference::Document {
                document_id: reference.document.to_storage(),
                node_id: reference.node.map(|node| node.to_storage()),
            },
            AttrValue::Url(url) => ViewReference::Url { url: url.clone() },
            AttrValue::Date(date) => ViewReference::Label {
                value: date.to_string(),
            },
            AttrValue::Text(value) => ViewReference::Label {
                value: value.clone(),
            },
            // The temporal shapes (ADR-183); no pill carries one yet, and a
            // label is the honest projection when one does. A zoned value
            // names its zone rather than showing a bare number, which a reader
            // could not place.
            AttrValue::FloatingDateTime(wall) => ViewReference::Label {
                value: wall.to_storage(),
            },
            AttrValue::ZonedDateTime(zoned) => ViewReference::Label {
                value: format!("{} {}", zoned.instant_ms(), zoned.zone()),
            },
            AttrValue::Number(value) => ViewReference::Label {
                value: value.to_string(),
            },
            AttrValue::Bool(value) => ViewReference::Label {
                value: value.to_string(),
            },
        };
        inline_objects.push(ViewInlineObject {
            id: attribute.id.to_storage(),
            anchor_utf8: start,
            role: attribute.role.clone(),
            label: attribute.label.clone(),
            reference,
        });
    }
    Ok(ViewRichText {
        text,
        marks,
        inline_objects,
    })
}

fn view_reference(
    target: koru_core::Ref,
    role: &str,
    root: koru_core::NodeId,
    current_document: DocumentId,
) -> ViewReference {
    use koru_core::DocRef;

    if target.node == root {
        return ViewReference::Document {
            document_id: current_document.to_storage(),
            // A legacy `Ref` at the document root names the document itself,
            // so there is no block to open it at.
            node_id: None,
        };
    }
    let home = match target.home {
        Some(DocRef::Document(document)) => Some(document),
        Some(DocRef::DocumentAt { document, .. }) => Some(document),
        Some(DocRef::Profile(_) | DocRef::Workspace(_)) | None => None,
    };
    if let Some(document) = home.filter(|_| role == "document") {
        return ViewReference::Document {
            document_id: document.to_storage(),
            node_id: None,
        };
    }
    // A run pill reads back as a reference homed at the run (proposal §3.2,
    // F2): the home is the Run document, and the role says it is a run.
    if let Some(document) = home.filter(|_| role == RUN_PILL_ROLE) {
        return ViewReference::Run {
            document_id: document.to_storage(),
        };
    }
    ViewReference::Block {
        block_id: target.node.to_storage(),
        document_id: home.map(|document| document.to_storage()),
    }
}

fn revision_number(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    }) & i64::MAX as u64
}

struct MobileReplicationStore {
    store: koru_store_server::StoreWorkerClient,
}

impl MobileReplicationStore {
    fn with_store<R, F>(&self, task: F) -> Result<R, StoreFailure>
    where
        R: Send + 'static,
        F: FnOnce(&mut koru_store_server::WorkerStore) -> Result<R, StoreFailure> + Send + 'static,
    {
        self.store
            .with_store(task)
            .map_err(|error| sf(store_worker_code(&error)))?
    }

    fn with_workspace<R, F>(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        task: F,
    ) -> Result<R, StoreFailure>
    where
        R: Send + 'static,
        F: FnOnce(&mut koru_store_server::WorkerStore) -> Result<R, StoreFailure> + Send + 'static,
    {
        let peer = peer.clone();
        self.with_store(move |store| {
            let profile = match store.workspace_including_deleted(workspace) {
                Ok(workspace) => workspace.owner,
                Err(error)
                    if error.code() == koru_store_core::errors::Code::WorkspaceNotFound
                        && !peer.all_data
                        && !peer.authorized_workspaces.contains(&workspace) =>
                {
                    // Normalize only absence. A corrupt or unavailable Store
                    // remains a retryable operational failure, not authorization.
                    return Err(sf("unauthorized_workspace"));
                }
                Err(error) => return Err(map_store(error)),
            };
            peer.with_authorized_workspace_profile(workspace, profile, || task(store))
        })
    }
}

impl ReplicationStore for MobileReplicationStore {
    fn record_state_change(
        &self,
        event: &koru_replication::StateChangeEvent,
        retain_after_millis: i64,
    ) -> Result<koru_replication::StateChangeEvent, StoreFailure> {
        let event = mobile_store_event(event);
        let stored = self.with_store(move |store| {
            store
                .record_sync_state_event(&event, Timestamp::from_millis(retain_after_millis))
                .map_err(map_store)
        })?;
        mobile_replication_event(stored)
    }

    fn state_changes(
        &self,
        peer: DeviceId,
        retain_after_millis: i64,
        before_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<koru_replication::StateChangeEvent>, StoreFailure> {
        self.with_store(move |store| {
            store
                .sync_state_events(
                    &peer.to_storage(),
                    Timestamp::from_millis(retain_after_millis),
                    before_id,
                    limit,
                )
                .map_err(map_store)?
                .into_iter()
                .map(mobile_replication_event)
                .collect()
        })
    }

    fn latest_state_change_times(
        &self,
        retain_after_millis: i64,
    ) -> Result<Vec<(DeviceId, i64)>, StoreFailure> {
        self.with_store(move |store| {
            store
                .latest_sync_state_event_times(Timestamp::from_millis(retain_after_millis))
                .map_err(map_store)?
                .into_iter()
                .map(|(peer, at)| {
                    DeviceId::parse(&peer)
                        .map(|peer| (peer, at.millis()))
                        .ok_or_else(|| sf("corrupt_state"))
                })
                .collect()
        })
    }

    fn prune_state_changes(&self, retain_after_millis: i64) -> Result<usize, StoreFailure> {
        self.with_store(move |store| {
            store
                .prune_sync_state_events(Timestamp::from_millis(retain_after_millis))
                .map_err(map_store)
        })
    }

    fn profiles(&self, peer: &AuthenticatedPeer) -> Result<Vec<ProfileReplicaState>, StoreFailure> {
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_catalog_scope(|| {
                let mut result = Vec::new();
                for profile in store.profiles().map_err(map_store)? {
                    if !peer.all_data && !peer.authorized_profiles.contains(&profile.id) {
                        continue;
                    }
                    for state in store.profile_unit_states(profile.id).map_err(map_store)? {
                        if peer.check_authorized_profile_unit(state.unit).is_err() {
                            continue;
                        }
                        if let Some(workspace) = state.unit.workspace() {
                            let actual_owner = store
                                .workspace_including_deleted(workspace)
                                .map_err(map_store)?
                                .owner;
                            if peer
                                .check_authorized_workspace_profile(workspace, actual_owner)
                                .is_err()
                            {
                                continue;
                            }
                        }
                        let identity = matches!(
                            state.unit,
                            koru_store_core::profile_units::ProfileUnitRef::Identity { .. }
                        )
                        .then(|| {
                            koru_replication::protocol::ProfileIdentityState {
                                name: profile.name.clone(),
                                purpose: profile.purpose.clone(),
                                avatar: profile.avatar.clone(),
                                icon: profile.icon.clone(),
                                color: profile.color.clone(),
                                account: profile.account.clone(),
                                locale: profile.locale.to_string(),
                                created_at_ms: profile.created_at.millis(),
                                kind: profile.kind.as_str().to_owned(),
                            }
                        });
                        result.push(ProfileReplicaState {
                            unit: state.unit,
                            version: store
                                .profile_unit_version(state.unit)
                                .map_err(map_store)?
                                .encode(),
                            identity,
                        });
                    }
                }
                Ok(result)
            })
        })
    }

    fn provision_profile(
        &self,
        peer: &AuthenticatedPeer,
        state: &ProfileReplicaState,
    ) -> Result<(), StoreFailure> {
        state.validate()?;
        peer.check_authorized_profile_unit(state.unit)?;
        let identity = state
            .identity
            .as_ref()
            .ok_or_else(|| sf("invalid_profile_identity"))?;
        let locale = koru_store_core::text::Locale::parse(&identity.locale)
            .ok_or_else(|| sf("invalid_locale"))?;
        let profile = koru_store_core::model::Profile {
            id: state.unit.profile(),
            name: identity.name.clone(),
            purpose: identity.purpose.clone(),
            avatar: identity.avatar.clone(),
            icon: identity.icon.clone(),
            color: identity.color.clone(),
            account: identity.account.clone(),
            locale,
            created_at: koru_store_core::model::Timestamp::from_millis(identity.created_at_ms),
            kind: koru_store_core::model::ProfileKind::parse(&identity.kind),
        };
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_authorized_profile(profile.id, || {
                store.provision_replica_profile(&profile).map_err(map_store)
            })
        })
    }

    fn profile_version(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
    ) -> Result<Vec<u8>, StoreFailure> {
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_authorized_profile_unit(unit, || {
                if let Some(workspace) = unit.workspace() {
                    let actual_owner = store
                        .workspace_including_deleted(workspace)
                        .map_err(map_store)?
                        .owner;
                    peer.check_authorized_workspace_profile(workspace, actual_owner)?;
                }
                store
                    .profile_unit_version(unit)
                    .map(|version| version.encode())
                    .map_err(map_store)
            })
        })
    }

    fn profile_updates_since(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        let version = version.clone();
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_authorized_profile_unit(unit, || {
                if let Some(workspace) = unit.workspace() {
                    let actual_owner = store
                        .workspace_including_deleted(workspace)
                        .map_err(map_store)?
                        .owner;
                    peer.check_authorized_workspace_profile(workspace, actual_owner)?;
                }
                store
                    .profile_unit_updates_since(unit, &version)
                    .map_err(map_store)
            })
        })
    }

    fn apply_remote_profile_updates(
        &self,
        peer: &AuthenticatedPeer,
        unit: koru_store_core::profile_units::ProfileUnitRef,
        updates: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_authorized_profile_unit(unit, || {
                if let Some(workspace) = unit.workspace() {
                    let actual_owner = store
                        .workspace_including_deleted(workspace)
                        .map_err(map_store)?
                        .owner;
                    peer.check_authorized_workspace_profile(workspace, actual_owner)?;
                }
                store
                    .apply_profile_unit_updates(unit, &updates)
                    .map_err(map_store)?;
                Ok(DurableAck {
                    durable_sequence: None,
                })
            })
        })
    }

    fn catalog(
        &self,
        peer: &AuthenticatedPeer,
    ) -> Result<Vec<WorkspaceCatalogState>, StoreFailure> {
        let peer = peer.clone();
        self.with_store(move |store| {
            peer.with_catalog_scope(|| {
                let workspaces = store.workspaces_including_deleted().map_err(map_store)?;
                workspaces
                    .into_iter()
                    .filter(|workspace| {
                        peer.all_data
                            || peer.authorized_profiles.contains(&workspace.owner)
                            || peer.authorized_workspaces.contains(&workspace.id)
                    })
                    .map(|workspace| {
                        let digest = store
                            .workspace_sync_digest(workspace.id)
                            .map_err(map_store)?;
                        Ok(WorkspaceCatalogState {
                            workspace_id: workspace.id,
                            profile_id: workspace.owner,
                            digest,
                        })
                    })
                    .collect()
            })
        })
    }

    fn workspace_profile(&self, workspace: WorkspaceId) -> Result<ProfileId, StoreFailure> {
        self.with_store(move |store| {
            store
                .workspace_including_deleted(workspace)
                .map(|workspace| workspace.owner)
                .map_err(map_store)
        })
    }

    fn provision_replica(
        &self,
        peer: &AuthenticatedPeer,
        replica: &ReplicaDescriptor,
    ) -> Result<(), StoreFailure> {
        let peer = peer.clone();
        let replica = replica.clone();
        self.with_store(move |store| {
            peer.with_authorized_workspace_profile(replica.workspace_id, replica.profile_id, || {
                let profile = if peer.all_data {
                    let locale = koru_store_core::text::Locale::parse(&replica.profile_locale)
                        .ok_or_else(|| sf("invalid_locale"))?;
                    koru_store_core::model::Profile {
                        id: replica.profile_id,
                        name: replica.profile_name.clone(),
                        purpose: None,
                        avatar: None,
                        icon: replica.profile_icon.clone(),
                        color: replica.profile_color.clone(),
                        account: None,
                        locale,
                        created_at: koru_store_core::model::Timestamp::from_millis(
                            replica.profile_created_at_ms,
                        ),
                        kind: koru_store_core::model::ProfileKind::parse(&replica.profile_kind),
                    }
                } else {
                    if replica.has_profile_metadata() {
                        return Err(sf("scoped_profile_metadata"));
                    }
                    store.profile(replica.profile_id).map_err(map_store)?
                };
                let workspace = koru_store_core::model::Workspace {
                    id: replica.workspace_id,
                    owner: replica.profile_id,
                    name: replica.workspace_name.clone(),
                    created_at: koru_store_core::model::Timestamp::from_millis(
                        replica.workspace_created_at_ms,
                    ),
                    last_opened_at: None,
                    icon: replica.workspace_icon.clone(),
                    color: replica.workspace_color.clone(),
                    pinned: replica.workspace_pinned,
                    archived: replica.workspace_archived,
                    deleted_at: replica
                        .workspace_deleted_at_ms
                        .map(koru_store_core::model::Timestamp::from_millis),
                    position: 0,
                };
                store
                    .provision_replica(
                        &profile,
                        &workspace,
                        replica.manifest_document,
                        koru_store_core::model::Timestamp::now(),
                    )
                    .map_err(map_store)
            })
        })
    }

    fn provision_document(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<(), StoreFailure> {
        self.with_workspace(peer, workspace, move |store| {
            store
                .provision_replica_document(
                    workspace,
                    document,
                    koru_store_core::model::Timestamp::now(),
                )
                .map_err(map_store)
        })
    }

    fn inventory(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<WorkspaceInventory, StoreFailure> {
        let all_data = peer.all_data;
        self.with_workspace(peer, workspace, move |store| {
            let record = store
                .workspace_including_deleted(workspace)
                .map_err(map_store)?;
            let profile = store.profile(record.owner).map_err(map_store)?;
            let manifest = if record.deleted_at.is_some() {
                store
                    .replica_manifest_id_including_deleted(workspace)
                    .map_err(map_store)?
            } else {
                store.manifest_id(workspace).map_err(map_store)?
            };
            let mut documents = vec![DocumentReplicaState {
                document_id: manifest,
                room_id: format!("workspace:{}:manifest", workspace.to_storage()),
                version: store
                    .version(workspace, manifest)
                    .map_err(|error| map_document_store(error, "version", workspace, manifest))?
                    .encode(),
                is_manifest: true,
            }];
            if record.deleted_at.is_none() {
                for entry in store.documents(workspace).map_err(map_store)? {
                    let version = match store.version(workspace, entry.id) {
                        Ok(version) => version,
                        // A manifest entry with no physical document cannot be
                        // served — and it must not take the whole workspace's
                        // inventory down with it. The peer that holds the content
                        // advertises it instead, and this store pulls it there.
                        Err(error)
                            if error.code() == koru_store_core::errors::Code::DocumentNotFound =>
                        {
                            continue;
                        }
                        Err(error) => {
                            return Err(map_document_store(error, "version", workspace, entry.id));
                        }
                    };
                    documents.push(DocumentReplicaState {
                        document_id: entry.id,
                        room_id: format!("document:{}", entry.id.to_storage()),
                        version: version.encode(),
                        is_manifest: false,
                    });
                }
            }
            let mut inventory = WorkspaceInventory {
                workspace_id: workspace,
                replica: ReplicaDescriptor {
                    profile_id: profile.id,
                    profile_name: profile.name,
                    profile_locale: profile.locale.to_string(),
                    profile_created_at_ms: profile.created_at.millis(),
                    profile_icon: profile.icon,
                    profile_color: profile.color,
                    profile_kind: profile.kind.as_str().to_owned(),
                    workspace_id: record.id,
                    workspace_name: record.name,
                    workspace_created_at_ms: record.created_at.millis(),
                    workspace_icon: record.icon,
                    workspace_color: record.color,
                    workspace_pinned: record.pinned,
                    workspace_archived: record.archived,
                    workspace_deleted_at_ms: record.deleted_at.map(|at| at.millis()),
                    manifest_document: manifest,
                },
                documents,
                blobs: if record.deleted_at.is_some() {
                    Vec::new()
                } else {
                    store.blobs(workspace).map_err(map_store)?
                },
            };
            if !all_data {
                inventory.replica.redact_profile_metadata();
            }
            Ok(inventory)
        })
    }

    fn locate(
        &self,
        peer: &AuthenticatedPeer,
        document: DocumentId,
    ) -> Result<Option<WorkspaceId>, StoreFailure> {
        let peer = peer.clone();
        self.with_store(move |store| {
            let Some(workspace) = store.locate(document).map_err(map_store)? else {
                return Ok(None);
            };
            let profile = store
                .workspace_including_deleted(workspace)
                .map_err(map_store)?
                .owner;
            peer.with_authorized_workspace_profile(workspace, profile, || Ok(Some(workspace)))
        })
    }
    fn document_name(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Option<String> {
        self.with_workspace(peer, workspace, move |store| {
            Ok(store
                .documents(workspace)
                .ok()
                .and_then(|entries| entries.into_iter().find(|entry| entry.id == document))
                .map(|entry| entry.title))
        })
        .ok()
        .flatten()
    }
    fn manifest_id(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
    ) -> Result<DocumentId, StoreFailure> {
        self.with_workspace(peer, workspace, move |store| {
            store
                .replica_manifest_id_including_deleted(workspace)
                .map_err(map_store)
        })
    }
    fn version(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.with_workspace(peer, workspace, move |store| {
            store
                .version(workspace, document)
                .map(|v| v.encode())
                .map_err(map_store)
        })
    }
    fn updates_since(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
        version: &loro::VersionVector,
    ) -> Result<Vec<u8>, StoreFailure> {
        let version = version.clone();
        self.with_workspace(peer, workspace, move |store| {
            store
                .updates_since(workspace, document, &version)
                .map_err(map_store)
        })
    }
    fn snapshot(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Vec<u8>, StoreFailure> {
        self.with_workspace(peer, workspace, move |store| {
            store.snapshot(workspace, document).map_err(map_store)
        })
    }
    fn blob_origin(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Option<String> {
        let hash = hash.clone();
        self.with_workspace(peer, workspace, move |store| {
            Ok(store.blob_origin(workspace, &hash).ok().flatten())
        })
        .ok()
        .flatten()
    }
    fn record_blob_origins(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        blobs: &[koru_store_core::model::Blob],
    ) -> Result<usize, StoreFailure> {
        let blobs = blobs.to_vec();
        self.with_workspace(peer, workspace, move |store| {
            store
                .record_blob_origins(workspace, &blobs, Timestamp::now())
                .map_err(map_store)
        })
    }
    fn read_blob_chunk(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        hash: &koru_store_core::model::BlobHash,
        offset: u64,
        max_bytes: u32,
    ) -> Result<BlobChunk, StoreFailure> {
        let hash = hash.clone();
        self.with_workspace(peer, workspace, move |store| {
            let blob = store.blob(workspace, &hash).map_err(map_store)?;
            if offset > blob.size {
                return Err(sf("invalid_offset"));
            }
            // Verify once per transfer, then read ranges — see the desktop adapter
            // for why the whole-blob read that used to be here was quadratic. A
            // phone serving a file to a desktop is the case that made it matter.
            if offset == 0 {
                store.verify_blob(workspace, &hash).map_err(map_store)?;
            }
            let requested = max_bytes.clamp(1, 1024 * 1024) as usize;
            let bytes = store
                .read_blob_range(workspace, &hash, offset, requested)
                .map_err(map_store)?;
            let end = offset.saturating_add(bytes.len() as u64);
            Ok(BlobChunk {
                workspace_id: workspace,
                blob: blob.clone(),
                offset,
                bytes,
                complete: end >= blob.size,
            })
        })
    }
    fn store_remote_blob(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        blob: &koru_store_core::model::Blob,
        bytes: Vec<u8>,
    ) -> Result<(), StoreFailure> {
        // The record is stored as announced: its attributes name the
        // *origin* device, and this one is a holder, not the origin.
        let blob = blob.clone();
        self.with_workspace(peer, workspace, move |store| {
            store
                .put_remote_blob(workspace, &blob, &bytes)
                .map_err(map_store)?;
            Ok(())
        })
    }
    fn apply_remote_updates(
        &self,
        peer: &AuthenticatedPeer,
        workspace: WorkspaceId,
        document: DocumentId,
        updates: Vec<Vec<u8>>,
    ) -> Result<DurableAck, StoreFailure> {
        let now = koru_store_core::model::Timestamp::now();
        let origin = peer.update_origin();
        self.with_workspace(peer, workspace, move |store| {
            for update in updates {
                store
                    .apply_update(workspace, document, update, now, origin.clone())
                    .map_err(map_store)?;
            }
            let durable_sequence = store.flush(workspace, document, now).map_err(map_store)?;
            Ok(DurableAck { durable_sequence })
        })
    }
}

fn mobile_store_event(
    event: &koru_replication::StateChangeEvent,
) -> koru_store_core::ports::SyncStateEvent {
    koru_store_core::ports::SyncStateEvent {
        id: event.event_id,
        received_at: Timestamp::from_millis(event.at_millis),
        peer_device_id: event.peer.to_storage(),
        kind: event.kind.clone(),
        profile: event.profile,
        workspace: event.workspace,
        document: event.document,
        document_name: event.document_name.clone(),
        blob_hash: event.blob_hash.clone(),
        detail: event.detail.clone(),
        items: event.items,
    }
}

fn mobile_replication_event(
    event: koru_store_core::ports::SyncStateEvent,
) -> Result<koru_replication::StateChangeEvent, StoreFailure> {
    let peer = DeviceId::parse(&event.peer_device_id).ok_or_else(|| sf("corrupt_state"))?;
    Ok(koru_replication::StateChangeEvent {
        event_id: event.id,
        sequence: event.id.and_then(|id| u64::try_from(id).ok()).unwrap_or(0),
        at_millis: event.received_at.millis(),
        peer,
        kind: event.kind,
        profile: event.profile,
        workspace: event.workspace,
        document: event.document,
        document_name: event.document_name,
        blob_hash: event.blob_hash,
        detail: event.detail,
        items: event.items,
    })
}

/// This host's network configuration through the one shared helper
/// (Plan 5.2): isolated mode, the `KORU_IROH_RELAYS` override, the compiled
/// Koru relay list and the Relay-Off precedence all live in
/// `koru-network-iroh`, the same as the desktop hosts.
///
/// Mobile is always [`RelayChannelKind::Production`]: mobile builds carry no
/// release-channel stamping (`koru-release` stamps desktop binaries only, so
/// `koru_channel::build_info()` would call every phone "internal" and send
/// real users to the dev relay). Production is the honest compiled default
/// until mobile grows a channel of its own.
fn network_config(relay_enabled: bool) -> koru_network_iroh::IrohNetworkConfig {
    koru_network_iroh::host_network_config(
        relay_enabled,
        koru_network_iroh::RelayChannelKind::Production,
    )
}

/// `relay_enabled` is the device's platform-local relay preference, applied
/// only when this call is the one that opens the host; an already-open host
/// keeps its runtime state (a live change goes through
/// [`MobileHost::set_relay_enabled`]).
fn get_host(
    root: PathBuf,
    secrets: [u8; 64],
    device_name: &str,
    relay_enabled: bool,
) -> Result<Arc<MobileHost>, String> {
    let mut hosts = HOSTS.lock().map_err(|_| "host_lock".to_owned())?;
    if let Some(host) = hosts.get(&root) {
        return Ok(Arc::clone(host));
    }
    let host = MobileHost::open(root.clone(), secrets, device_name, relay_enabled)?;
    hosts.insert(root, Arc::clone(&host));
    Ok(host)
}
fn failed(code: &'static str) -> LinkResult<'static> {
    failed_owned(code.to_owned())
}
fn failed_owned(code: String) -> LinkResult<'static> {
    LinkResult {
        state: "failed",
        peer_name: None,
        error_code: Some(code),
    }
}
fn sf(code: &'static str) -> StoreFailure {
    StoreFailure::new(code)
}

/// Name which Store-worker failure this was.
///
/// **These are three different faults and they used to share one word.** Both
/// `with_store` wrappers collapsed every [`koru_store_server::WorkerError`]
/// into `store_worker_unavailable`, so a phone could not tell a full queue
/// (transient, retry) from a reply that never came (the dispatcher is wedged;
/// only a restart clears it) from a worker that had stopped (nothing will ever
/// answer). That one word cost several rounds of diagnosis on a wedged tablet,
/// because the log said the same thing for all of them. The worker has always
/// distinguished them; only this boundary threw the distinction away.
///
/// A timeout is logged as well as returned: it is the one that means something
/// is holding the Store, and it is worth a line in the device log the first
/// time it happens rather than only in a caller's error string.
fn store_worker_code(error: &koru_store_server::WorkerError) -> &'static str {
    match error {
        // The queue is full: the worker is answering, just not fast enough.
        koru_store_server::WorkerError::Saturated => "store_worker_busy",
        // Nothing is listening. Every later call will fail the same way.
        koru_store_server::WorkerError::Stopped => "store_worker_stopped",
        // Admitted, never answered — the dispatcher is blocked on something.
        koru_store_server::WorkerError::TimedOut => {
            tracing::error!(
                "store worker: a job was admitted and did not answer within the reply timeout; \
                 the Store dispatcher is blocked and every later call will time out too"
            );
            "store_worker_timeout"
        }
        koru_store_server::WorkerError::Poisoned => "store_worker_poisoned",
        koru_store_server::WorkerError::Startup(_) => "store_worker_unavailable",
    }
}
fn map_store(error: koru_store_core::Error) -> StoreFailure {
    StoreFailure::new(error.code().as_str()).with_detail(error.message())
}

fn map_document_store(
    error: koru_store_core::Error,
    operation: &'static str,
    workspace: WorkspaceId,
    document: DocumentId,
) -> StoreFailure {
    let detail = format!(
        "operation={operation} workspace={} document={}: {}",
        workspace.to_storage(),
        document.to_storage(),
        error.message()
    );
    StoreFailure::new(error.code().as_str()).with_detail(detail)
}
fn profile_avatar_color(avatar: Option<&str>) -> Option<&str> {
    avatar?
        .strip_prefix("color:")
        .map(str::trim)
        .filter(|color| !color.is_empty())
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Historical JSON-envelope goldens retained test-only during the Protobuf
/// cutover. No production shim compiles or exposes this module.
#[cfg(test)]
mod legacy_json_contract_tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde::Serialize;

    use super::{
        DevicesResult, DocumentViewResult, InventoryResult, LinkResult, MobileHost,
        PairingOfferResult, failed_owned, get_host,
    };

    fn host(
        root: &Path,
        secrets: &[u8],
        device_name: &str,
        operation: &str,
    ) -> Result<Arc<MobileHost>, String> {
        let secrets: [u8; 64] = secrets
            .try_into()
            .map_err(|_| "invalid_device_seeds".to_owned())?;
        let host = get_host(root.to_path_buf(), secrets, device_name, true)?;
        host.admit(operation)?;
        Ok(host)
    }

    fn to_json<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "{\"error_code\":\"json\"}".into())
    }

    fn link_json_of(result: LinkResult<'_>) -> String {
        serde_json::to_string(&result)
            .unwrap_or_else(|_| "{\"state\":\"failed\",\"error_code\":\"json\"}".into())
    }

    fn inventory_json_of(result: InventoryResult) -> String {
        serde_json::to_string(&result).unwrap_or_else(|_| {
            "{\"profiles\":[],\"workspaces\":[],\"documents\":[],\"error_code\":\"json\"}".into()
        })
    }

    // ------------------------------------------------------------- the calls

    pub fn status_json(root: &Path, secrets: &[u8], device_name: &str) -> String {
        link_json_of(
            host(root, secrets, device_name, "status")
                .map(|host| host.status())
                .unwrap_or_else(failed_owned),
        )
    }

    pub fn inventory_json(root: &Path, secrets: &[u8], device_name: &str) -> String {
        inventory_json_of(
            host(root, secrets, device_name, "store.inventory")
                .and_then(|host| host.inventory())
                .unwrap_or_else(|code| InventoryResult {
                    error_code: Some(code),
                    ..InventoryResult::default()
                }),
        )
    }

    /// The one call that answers bytes rather than JSON; a shim turns the
    /// error into its platform's failure shape (Android throws, C answers an
    /// empty buffer beside the error string).
    pub fn blob_bytes(
        root: &Path,
        secrets: &[u8],
        device_name: &str,
        workspace_id: &str,
        hash: &str,
    ) -> Result<Vec<u8>, String> {
        host(root, secrets, device_name, "store.blob")?.load_blob(workspace_id, hash)
    }

    pub fn advertise_build_json(
        root: &Path,
        secrets: &[u8],
        device_name: &str,
        build: &str,
    ) -> String {
        match host(root, secrets, device_name, "advertise_build")
            .and_then(|host| host.advertise_build(build))
        {
            Ok(()) => to_json(&serde_json::json!({"state":"advertised"})),
            Err(code) => error_json(&code),
        }
    }

    pub fn devices_json(root: &Path, secrets: &[u8], device_name: &str) -> String {
        to_json(
            &host(root, secrets, device_name, "devices")
                .map(|host| host.devices())
                .unwrap_or_else(|code| DevicesResult {
                    error_code: Some(code),
                    ..DevicesResult::default()
                }),
        )
    }

    pub fn device_history_json(
        root: &Path,
        secrets: &[u8],
        device_name: &str,
        device_id: &str,
        before: Option<i64>,
    ) -> String {
        match host(root, secrets, device_name, "device_history")
            .and_then(|host| host.state_history(device_id, before))
        {
            Ok(history) => to_json(&history),
            Err(code) => error_json(&code),
        }
    }

    /// Start over as a new device after `device_identity_mismatch`, keeping
    /// content. `{"state":"reset"}`, or `identity_intact` when nothing was
    /// wrong — a shim must not offer this on a healthy device.
    pub fn reset_identity_json(root: &Path, secrets: &[u8], device_name: &str) -> String {
        match host(root, secrets, device_name, "reset_identity")
            .and_then(|host| host.reset_identity(device_name))
        {
            Ok(()) => to_json(&serde_json::json!({"state":"reset"})),
            Err(code) => error_json(&code),
        }
    }

    // --------------------------------------------- a shim's own failures

    /// `{"error_code":…}` — the envelope of every mutation-shaped call.
    pub fn error_json(code: &str) -> String {
        to_json(&serde_json::json!({ "error_code": code }))
    }

    /// The failed shape of `status_json` / `link_json`.
    pub fn link_error_json(code: &str) -> String {
        link_json_of(failed_owned(code.to_owned()))
    }

    /// The failed shape of `inventory_json`.
    pub fn inventory_error_json(code: &str) -> String {
        inventory_json_of(InventoryResult {
            error_code: Some(code.to_owned()),
            ..InventoryResult::default()
        })
    }

    /// The failed shape of `document_json`.
    pub fn document_error_json(code: &str) -> String {
        to_json(&DocumentViewResult {
            document: None,
            error_code: Some(code.to_owned()),
        })
    }

    /// The failed shape of `start_pairing_json`.
    pub fn pairing_error_json(code: &str) -> String {
        to_json(&PairingOfferResult {
            state: "failed",
            qr_payload: None,
            expires_in_seconds: 0,
            error_code: Some(code.to_owned()),
        })
    }

    /// The failed shape of `devices_json`.
    pub fn devices_error_json(code: &str) -> String {
        to_json(&DevicesResult {
            error_code: Some(code.to_owned()),
            ..DevicesResult::default()
        })
    }

    /// **The goldens.** These byte-for-byte strings are the contract both
    /// shims answer; a Swift fixture that decodes one of these carries a
    /// comment naming the test here it mirrors. Change a shape and this test
    /// is where the change is decided — never in a shim.
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_error_envelope_is_the_endpoints_own() {
            assert_eq!(error_json("x"), r#"{"error_code":"x"}"#);
            assert_eq!(
                link_error_json("x"),
                r#"{"state":"failed","peer_name":null,"error_code":"x"}"#
            );
            assert_eq!(
                inventory_error_json("x"),
                r#"{"profiles":[],"workspaces":[],"documents":[],"error_code":"x"}"#
            );
            assert_eq!(document_error_json("x"), r#"{"error_code":"x"}"#);
            assert_eq!(
                pairing_error_json("x"),
                r#"{"state":"failed","qr_payload":null,"expires_in_seconds":0,"error_code":"x"}"#
            );
            assert_eq!(
                devices_error_json("x"),
                r#"{"devices":[],"error_code":"x","relink_available":false}"#
            );
        }

        /// The whole path a shim actually takes, against a real host on a
        /// fresh root — the byte-exact happy-path golden.
        #[test]
        fn a_fresh_root_answers_unlinked_through_the_api() {
            let directory = tempfile::tempdir().unwrap();
            assert_eq!(
                status_json(directory.path(), &[47u8; 64], "Test"),
                r#"{"state":"unlinked","peer_name":null,"error_code":null}"#
            );
        }

        /// What a shim sees when the app describes its build on a fresh
        /// root: `advertised`, then the device list carries the label on
        /// this device — with the protocol number the host put there, not
        /// the one the app claimed. The iOS canary mirrors this test.
        #[test]
        fn a_fresh_root_advertises_its_build_and_the_list_says_so() {
            let directory = tempfile::tempdir().unwrap();
            let build = r#"{"app_version":"0.1.0 (1)","channel":"debug","branch":"mac-catch","os":"iOS 26.0","peer_protocol":999}"#;
            assert_eq!(
                advertise_build_json(directory.path(), &[47u8; 64], "Test", build),
                r#"{"state":"advertised"}"#
            );
            let devices: serde_json::Value =
                serde_json::from_str(&devices_json(directory.path(), &[47u8; 64], "Test")).unwrap();
            assert_eq!(
                devices["this_device"]["build_label"],
                "0.1.0 (1) · debug · mac-catch · iOS 26.0"
            );
            // Advertising the same description again writes nothing — and
            // still answers advertised, so a caller cannot tell and need not.
            assert_eq!(
                advertise_build_json(directory.path(), &[47u8; 64], "Test", build),
                r#"{"state":"advertised"}"#
            );
        }

        /// The recovery a reinstalled phone needs: seeds that no longer match
        /// `device.json` open as a mismatch, `reset` sets the old identity
        /// aside and mints a new one from the seeds in hand, and the device is
        /// plainly unlinked afterwards — never blank, never destroyed.
        #[test]
        fn a_mismatched_identity_can_start_over_and_keeps_the_old_one_aside() {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            assert_eq!(
                status_json(root, &[47u8; 64], "Test"),
                r#"{"state":"unlinked","peer_name":null,"error_code":null}"#
            );
            let old_identity = std::fs::read_to_string(root.join("device.json")).unwrap();
            let original = get_host(root.to_owned(), [47; 64], "Test", true).unwrap();
            let retained_workspace = original
                .with_store(|store| {
                    let profile = store
                        .create_profile(
                            "Retained",
                            None,
                            None,
                            koru_store_core::Locale::parse("en").unwrap(),
                            koru_store_core::Timestamp::from_millis(1),
                        )
                        .map_err(|error| error.to_string())?;
                    store
                        .create_workspace(
                            profile.id,
                            "Retained workspace",
                            koru_store_core::Timestamp::from_millis(2),
                        )
                        .map(|workspace| workspace.id)
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            // A second process with other seeds: the reinstalled app.
            let stale_dir = tempfile::tempdir().unwrap();
            let stale = stale_dir.path().join("reinstalled");
            copy_dir(root, &stale);
            let old_database = std::fs::read(stale.join("trust.db")).unwrap();
            let old_wal = std::fs::read(stale.join("trust.db-wal")).ok();
            assert_eq!(
                status_json(&stale, &[48u8; 64], "Test"),
                r#"{"state":"unavailable","peer_name":null,"error_code":"device_identity_mismatch"}"#
            );
            assert_eq!(
                reset_identity_json(&stale, &[48u8; 64], "Test"),
                r#"{"state":"reset"}"#
            );
            assert_eq!(
                status_json(&stale, &[48u8; 64], "Test"),
                r#"{"state":"unlinked","peer_name":null,"error_code":null}"#
            );
            let aside = std::fs::read_dir(&stale)
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("identity-set-aside-")
                })
                .expect("the old identity is set aside");
            assert_eq!(
                std::fs::read_to_string(aside.path().join("device.json")).unwrap(),
                old_identity
            );
            assert_ne!(
                std::fs::read_to_string(stale.join("device.json")).unwrap(),
                old_identity
            );
            assert_eq!(
                std::fs::read(aside.path().join("trust.db")).unwrap(),
                old_database
            );
            if let Some(old_wal) = old_wal {
                assert_eq!(
                    std::fs::read(aside.path().join("trust.db-wal")).unwrap(),
                    old_wal
                );
            }
            let reset = get_host(stale.clone(), [48; 64], "Test", true).unwrap();
            assert!(
                reset
                    .with_store(move |store| Ok(store
                        .workspaces()
                        .map_err(|error| error.to_string())?
                        .iter()
                        .any(|workspace| workspace.id == retained_workspace)))
                    .unwrap()
            );
            // A healthy device refuses the reset.
            assert_eq!(
                reset_identity_json(root, &[47u8; 64], "Test"),
                r#"{"error_code":"identity_intact"}"#
            );
        }

        fn copy_dir(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap().filter_map(Result::ok) {
                let target = to.join(entry.file_name());
                if entry.path().is_dir() {
                    copy_dir(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }

        /// A device that has changed nothing here has an empty first page
        /// and no next one; an id that is not a device is named as such.
        /// The iOS canary mirrors these bytes.
        #[test]
        fn a_device_with_no_history_answers_an_empty_page() {
            let directory = tempfile::tempdir().unwrap();
            let device = "6cca06d3-5792-42d2-aedb-b887a6682475";
            assert_eq!(
                device_history_json(directory.path(), &[47u8; 64], "Test", device, None),
                format!(r#"{{"device_id":"{device}","events":[],"next_before":null}}"#)
            );
            assert_eq!(
                device_history_json(directory.path(), &[47u8; 64], "Test", "not-a-device", None),
                r#"{"error_code":"invalid_device_id"}"#
            );
        }

        /// Secret validation happens below every shim, so a short buffer
        /// answers the endpoint's envelope — the same bytes on both platforms.
        #[test]
        fn short_secrets_fail_closed_in_the_endpoints_shape() {
            let root = std::env::temp_dir().join("koru-mobile-host-never-created");
            assert_eq!(
                status_json(&root, &[1, 2, 3], "Test"),
                r#"{"state":"failed","peer_name":null,"error_code":"invalid_device_seeds"}"#
            );
            assert_eq!(
                inventory_json(&root, &[1, 2, 3], "Test"),
                r#"{"profiles":[],"workspaces":[],"documents":[],"error_code":"invalid_device_seeds"}"#
            );
            assert_eq!(
                blob_bytes(&root, &[1, 2, 3], "Test", "w", "h"),
                Err("invalid_device_seeds".to_owned())
            );
            // And nothing was written: validation refused before any open.
            assert!(!root.exists());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// This host's entry point is a thin wrapper over the shared helper with
    /// the production channel — whatever the machine's `KORU_IROH_*`
    /// environment says — and the Relay-Off row holds through this host's own
    /// wiring, which also makes the assertion immune to that environment.
    /// The remaining precedence rows are the helper's table-driven suite in
    /// `koru-network-iroh`.
    #[test]
    fn network_config_is_the_shared_helper_on_the_production_channel() {
        for relay_enabled in [false, true] {
            assert_eq!(
                network_config(relay_enabled),
                koru_network_iroh::host_network_config(
                    relay_enabled,
                    koru_network_iroh::RelayChannelKind::Production,
                )
            );
        }
        assert_eq!(
            network_config(false).relay_provider,
            koru_network_iroh::RelayProviderConfig::Disabled
        );
    }

    /// Opening with relays off hands the runtime the `Disabled` provider
    /// (pinned above through `network_config`, the exact value `open` passes
    /// to `spawn_with_store`) and is otherwise an ordinary open: the host
    /// answers, and the live switch runs in both directions without touching
    /// the on-disk identity.
    #[test]
    fn a_host_opened_with_relays_off_answers_and_switches_live() {
        let root = tempfile::tempdir().unwrap();
        let host = MobileHost::open(root.path().to_path_buf(), [63; 64], "Test", false).unwrap();
        assert_eq!(host.status().state, "unlinked");
        host.set_relay_enabled(true);
        host.set_relay_enabled(false);
        assert_eq!(host.status().state, "unlinked");
    }

    /// All four runtime relay readings onto the wire words the Devices
    /// screen shows: the state word, and the host carried on every
    /// connection state but dropped for Off (the row already names the
    /// switch).
    #[test]
    fn relay_status_maps_all_four_states_and_the_host() {
        use koru_network_iroh::RelayConnectionStatus as Status;
        let host = || "use1-1.relay.koru.do".to_owned();
        for (value, state, expected_host) in [
            (Status::Off, "off", ""),
            (
                Status::Connecting { host: host() },
                "connecting",
                "use1-1.relay.koru.do",
            ),
            (
                Status::Connected { host: host() },
                "connected",
                "use1-1.relay.koru.do",
            ),
            (
                Status::Disconnected { host: host() },
                "disconnected",
                "use1-1.relay.koru.do",
            ),
        ] {
            assert_eq!(
                relay_status(value),
                RelayStatusResult {
                    state,
                    host: expected_host.to_owned(),
                }
            );
        }
    }

    /// The Devices answer carries the relay row: a host opened with relays
    /// off reads exactly "off" with no host to name, and the live switch to
    /// on replaces it with the configured relay's host under a real
    /// connection state — the same word set the desktop tray renders.
    #[test]
    fn devices_carries_the_relay_status_row() {
        let root = tempfile::tempdir().unwrap();
        let host = MobileHost::open(root.path().to_path_buf(), [64; 64], "Test", false).unwrap();
        assert_eq!(
            host.devices().relay,
            Some(RelayStatusResult {
                state: "off",
                host: String::new(),
            })
        );
        host.set_relay_enabled(true);
        // The switch travels a command channel and applies on the runtime
        // task, so the reading is polled rather than asserted immediately.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let relay = loop {
            let relay = host.devices().relay.expect("a relay reading");
            if relay.state != "off" {
                break relay;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the live relay switch never reached the status"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        // The suite must not depend on reachability, so the exact state is
        // whatever the watcher has read by now — but it is a connection
        // state, and the configured relay is named.
        assert!(
            ["connecting", "connected", "disconnected"].contains(&relay.state),
            "unexpected state {:?}",
            relay.state
        );
        assert!(!relay.host.is_empty(), "the configured relay has a host");
    }

    #[test]
    fn relink_availability_is_derived_from_durable_direct_trust() {
        let local_id = DeviceId::new();
        let target_id = DeviceId::new();
        let other_id = DeviceId::new();
        let local_key = koru_device::DeviceSigningKey::from_secret_bytes([11; 32]);
        let target_key = koru_device::DeviceSigningKey::from_secret_bytes([12; 32]);
        let other_key = koru_device::DeviceSigningKey::from_secret_bytes([13; 32]);
        let local = descriptor_for(local_id, [11; 64], "Android");
        let target = descriptor_for(target_id, [12; 64], "Windows");
        let other = descriptor_for(other_id, [13; 64], "Tablet");
        let pair = |peer: &koru_device::DeviceDescriptorV1,
                    peer_key: &koru_device::DeviceSigningKey| {
            let relationship_id = koru_device::RelationshipId::new();
            (
                koru_device::TrustRelationshipV1::sign(
                    relationship_id,
                    local.clone(),
                    peer.clone(),
                    koru_device::transcript_hash(relationship_id.to_storage().as_bytes()),
                    2,
                    &local_key,
                    peer_key,
                ),
                vec![
                    koru_device::AllDataGrantV1::sign(
                        relationship_id,
                        peer.device_id,
                        local_id,
                        koru_device::RecordId::new(),
                        3,
                        &local_key,
                    ),
                    koru_device::AllDataGrantV1::sign(
                        relationship_id,
                        local_id,
                        peer.device_id,
                        koru_device::RecordId::new(),
                        3,
                        peer_key,
                    ),
                ],
            )
        };
        let view = |relationships: Vec<koru_device::TrustRelationshipV1>,
                    grants: Vec<koru_device::AllDataGrantV1>,
                    revocations: Vec<koru_device::RevocationV1>| {
            let devices = relationships
                .iter()
                .map(|relationship| relationship.joiner.clone())
                .collect();
            koru_device::TrustView {
                local_device: local.clone(),
                devices,
                relationships,
                profile_grants: Vec::new(),
                grants,
                revocations,
                names: Vec::new(),
                builds: Vec::new(),
                lifecycles: std::collections::BTreeMap::new(),
                effective_revision: 1,
            }
        };

        let fresh = view(Vec::new(), Vec::new(), Vec::new());
        assert!(has_unrestored_local_relationship(&fresh, &[]));

        let (old_target, old_target_grants) = pair(&target, &target_key);
        let linked = view(
            vec![old_target.clone()],
            old_target_grants.clone(),
            Vec::new(),
        );
        let authorized = koru_device::Authorization::devices(&linked);
        assert!(!has_unrestored_local_relationship(&linked, &authorized));

        let (other_relationship, other_grants) = pair(&other, &other_key);
        let revocation = koru_device::RevocationV1::sign(
            old_target.relationship_id,
            target_id,
            local_id,
            koru_device::RecordId::new(),
            4,
            &local_key,
        );
        let unlinked = view(
            vec![old_target.clone(), other_relationship.clone()],
            old_target_grants
                .iter()
                .cloned()
                .chain(other_grants.iter().cloned())
                .collect(),
            vec![revocation.clone()],
        );
        let authorized = koru_device::Authorization::devices(&unlinked);
        assert_eq!(authorized.len(), 1, "the other peer remains linked");
        assert!(has_unrestored_local_relationship(&unlinked, &authorized));

        // A new projection over the same signed records makes the same choice;
        // no process-local flag participates in visibility.
        let reprojected = unlinked.clone();
        let authorized = koru_device::Authorization::devices(&reprojected);
        assert!(has_unrestored_local_relationship(&reprojected, &authorized));

        let (fresh_target, fresh_target_grants) = pair(&target, &target_key);
        let repaired = view(
            vec![old_target, other_relationship, fresh_target],
            old_target_grants
                .into_iter()
                .chain(other_grants)
                .chain(fresh_target_grants)
                .collect(),
            vec![revocation],
        );
        let authorized = koru_device::Authorization::devices(&repaired);
        assert_eq!(authorized.len(), 2);
        assert!(!has_unrestored_local_relationship(&repaired, &authorized));
    }

    fn with_test_store<R, F>(host: &MobileHost, task: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&mut koru_store_server::WorkerStore) -> R + Send + 'static,
    {
        host.store.with_store(task).unwrap()
    }

    /// The whole pairing handshake, two real hosts, real iroh — the exact
    /// exchange a phone and a desktop perform, in one process on two roots.
    /// If this is green, "linking does not work" on a device is environment
    /// (permissions, sandbox, reachability), not the protocol.
    ///
    /// **Ignored by default**, because the join resolves the inviter through
    /// iroh discovery — a real network dial, which the suite's own rule
    /// forbids and a sandboxed or offline runner rightly blocks. Run it on
    /// purpose (`cargo test -p koru-mobile-host two_hosts_pair -- --ignored`)
    /// when the question is the protocol; the simulator's C-shim twin covers
    /// the iOS environment.
    #[ignore = "dials the network for iroh discovery; run explicitly"]
    #[test]
    fn two_hosts_pair_through_a_real_invitation() {
        let inviter_root = tempfile::tempdir().unwrap();
        let joiner_root = tempfile::tempdir().unwrap();
        let inviter =
            MobileHost::open(inviter_root.path().to_path_buf(), [61; 64], "Inviter", true).unwrap();
        let joiner =
            MobileHost::open(joiner_root.path().to_path_buf(), [62; 64], "Joiner", true).unwrap();

        let offer = inviter.start_pairing();
        let payload = offer.qr_payload.expect("an invitation payload");
        assert_eq!(offer.state, "displaying");

        let outcome = joiner.link(&payload);
        assert_eq!(
            outcome.state, "linked",
            "the join failed: {:?}",
            outcome.error_code
        );
        assert_eq!(outcome.peer_name.as_deref(), Some("Inviter"));

        // Both sides hold the relationship; the inviter learned the joiner's
        // name from the join, not from configuration.
        let linked = joiner.devices();
        assert_eq!(linked.devices.len(), 1);
        assert_eq!(linked.devices[0].display_name, "Inviter");
    }

    #[test]
    fn a_document_view_projects_every_readable_block_and_document_links() {
        use koru_core::schema::Parent;
        use koru_core::{Attr, AttrValue, Content, DocRef, Kind, Mark, Node, Ref};

        let mut model =
            koru_core::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Read model").unwrap();
        let root = model
            .insert(Parent::Root, Node::document("Full document").unwrap())
            .unwrap();
        let paragraph = model
            .insert(Parent::Node(root), Node::paragraph("Open ").unwrap())
            .unwrap();
        model.set_mark(paragraph, 0..4, Mark::Bold, true).unwrap();
        let linked_document = koru_core::DocumentId::new();
        model
            .insert_pill(
                paragraph,
                5,
                &Attr::labelled(
                    "document",
                    AttrValue::Ref(Ref::in_doc(
                        koru_core::NodeId::new(),
                        DocRef::Document(linked_document),
                    )),
                    "Linked page",
                ),
            )
            .unwrap();
        model
            .insert(Parent::Node(root), Node::quote("Quoted").unwrap())
            .unwrap();
        let callout = model
            .insert(
                Parent::Node(root),
                Node::callout(Some("⚠️"), Some("red")).unwrap(),
            )
            .unwrap();
        let callout_first = model
            .insert(
                Parent::Node(callout),
                Node::paragraph("Inside the box").unwrap(),
            )
            .unwrap();
        let callout_second = model
            .insert(
                Parent::Node(callout),
                Node::paragraph("Second line").unwrap(),
            )
            .unwrap();
        model
            .insert(Parent::Node(root), Node::task("Task").unwrap())
            .unwrap();
        let list = model
            .insert(Parent::Node(root), Node::list_block().unwrap())
            .unwrap();
        model
            .set_list_style(list, Some(koru_core::ListStyle::Roman))
            .unwrap();
        model
            .insert(Parent::Node(list), Node::paragraph("Item").unwrap())
            .unwrap();
        model
            .insert(Parent::Node(root), Node::code("fn main() {}\n").unwrap())
            .unwrap();
        model
            .insert(
                Parent::Node(root),
                Node::table("| A | B |\n|---|---|\n| 1 | 2 |").unwrap(),
            )
            .unwrap();
        let hash = koru_core::BlobHash("b3:aaaaaaaa".to_owned());
        model
            .insert(
                Parent::Node(root),
                Node::image(hash.clone(), "image/png", Some(10), Some(20)).unwrap(),
            )
            .unwrap();
        model
            .insert(
                Parent::Node(root),
                Node::new(
                    Kind::Canvas,
                    Content::Blob {
                        hash,
                        mime: "image/png".to_owned(),
                        width: Some(30),
                        height: Some(40),
                        attrs: Vec::new(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        model
            .insert(
                Parent::Node(root),
                Node::new(Kind::Contact, Content::text("Ada")).unwrap(),
            )
            .unwrap();
        model
            .insert(Parent::Node(root), Node::divider().unwrap())
            .unwrap();
        model
            .insert(
                Parent::Node(root),
                Node::new(
                    Kind::Paragraph,
                    Content::Embed {
                        target: Ref::in_doc(
                            koru_core::NodeId::new(),
                            DocRef::Document(linked_document),
                        ),
                    },
                )
                .unwrap(),
            )
            .unwrap();

        let document = DocumentId::new();
        let view = document_view(
            &model,
            WorkspaceId::new(),
            document,
            "stale title".to_owned(),
            model.version().encode(),
            &HashMap::from([("b3:aaaaaaaa".to_owned(), 123)]),
        )
        .unwrap();

        assert_eq!(view.title, "Full document");
        let kinds = view
            .blocks
            .iter()
            .map(|block| block.kind.as_str())
            .collect::<Vec<_>>();
        for kind in [
            "paragraph",
            "quote",
            "callout",
            "task",
            "listblock",
            "code",
            "table",
            "image",
            "canvas",
            "contact",
            "divider",
            "embed",
        ] {
            assert!(kinds.contains(&kind), "missing {kind}: {kinds:?}");
        }
        // A callout is a box like a list block: the container carries the
        // icon and tint and no text, and the lines inside it are its
        // children, in order.
        let (box_index, callout_box) = view
            .blocks
            .iter()
            .enumerate()
            .find(|(_, block)| block.kind == "callout")
            .unwrap();
        assert_eq!(callout_box.id, callout.to_storage());
        assert_eq!(callout_box.parent_id, None);
        assert_eq!(callout_box.properties.icon.as_deref(), Some("⚠️"));
        assert_eq!(callout_box.properties.color.as_deref(), Some("red"));
        assert!(callout_box.rich_text.is_none());
        let lines: Vec<&ViewBlock> = view
            .blocks
            .iter()
            .filter(|block| block.parent_id.as_deref() == Some(callout_box.id.as_str()))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].id, callout_first.to_storage());
        assert_eq!(lines[1].id, callout_second.to_storage());
        for (line, text) in lines.iter().zip(["Inside the box", "Second line"]) {
            assert_eq!(line.kind, "paragraph");
            assert_eq!(line.rich_text.as_ref().unwrap().text, text);
        }
        let first_index = view
            .blocks
            .iter()
            .position(|block| block.id == lines[0].id)
            .unwrap();
        assert!(first_index > box_index, "a box's lines follow the box");
        let link = view.blocks[0]
            .rich_text
            .as_ref()
            .unwrap()
            .inline_objects
            .first()
            .unwrap();
        assert!(matches!(
            &link.reference,
            ViewReference::Document { document_id, .. }
                if document_id == &linked_document.to_storage()
        ));
        assert_eq!(
            view.blocks
                .iter()
                .find_map(|block| block.blob.as_ref())
                .unwrap()
                .size,
            123
        );
    }

    #[test]
    fn profile_avatar_exposes_only_named_color_tokens() {
        assert_eq!(profile_avatar_color(Some("color:purple")), Some("purple"));
        assert_eq!(profile_avatar_color(Some("blob:sha256")), None);
        assert_eq!(profile_avatar_color(Some("color:")), None);
        assert_eq!(profile_avatar_color(None), None);
    }

    #[test]
    fn profile_appearance_survives_the_mobile_inventory_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [41; 64], "Test", true).unwrap();
        let profile = with_test_store(&host, |store| {
            store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap()
        });
        with_test_store(&host, move |store| {
            store
                .set_profile_appearance(profile.id, Some("🧪"), Some("purple"))
                .unwrap()
        });

        let inventory = host.inventory().unwrap();
        assert_eq!(inventory.profiles[0].icon.as_deref(), Some("🧪"));
        assert_eq!(
            inventory.profiles[0].leading_emoji.as_deref(),
            Some("🧪"),
            "older mobile clients retain their compatibility projection"
        );
        assert_eq!(inventory.profiles[0].color.as_deref(), Some("purple"));
    }

    #[test]
    fn scoped_mobile_adapter_hides_unknown_and_unrelated_workspaces_alike() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [43; 64], "Test", true).unwrap();
        let (allowed_profile, unrelated) = with_test_store(&host, |store| {
            let at = Timestamp::from_millis(1);
            let allowed = store
                .create_profile(
                    "Allowed",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    at,
                )
                .unwrap();
            let private = store
                .create_profile(
                    "Private",
                    None,
                    None,
                    koru_store_core::Locale::parse("fr").unwrap(),
                    at,
                )
                .unwrap();
            let workspace = store
                .create_workspace(private.id, "Private workspace", at)
                .unwrap();
            (allowed.id, workspace.id)
        });
        let adapter = MobileReplicationStore {
            store: host.store.clone(),
        };
        let endpoint = koru_device::ReplicationEndpointSecret::generate();
        let peer = AuthenticatedPeer {
            relationship_id: koru_device::RelationshipId::new(),
            device_id: DeviceId::new(),
            endpoint_id: endpoint.endpoint_id(),
            all_data: false,
            authorized_profiles: [allowed_profile].into_iter().collect(),
            authorized_workspaces: Default::default(),
            authorization_fence: koru_replication::protocol::AuthorizationFence::active(),
            display_name: None,
            incompatible: false,
            peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        };

        let unrelated_error = adapter.inventory(&peer, unrelated).unwrap_err();
        let unknown_error = adapter.inventory(&peer, WorkspaceId::new()).unwrap_err();
        assert_eq!(unrelated_error.code, "unauthorized_workspace");
        assert_eq!(unknown_error.code, unrelated_error.code);
    }

    #[test]
    fn an_unlinked_mobile_host_can_generate_a_decodable_pairing_offer() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [42; 64], "Test", true).unwrap();
        let offer = host.start_pairing();
        assert_eq!(offer.state, "displaying");
        assert_eq!(offer.error_code, None);
        let payload = offer.qr_payload.expect("QR payload");
        assert!(payload.starts_with(koru_pairing::PAIRING_LINK_PREFIX));
        koru_pairing::PairingInvitationV1::decode_link(&payload).unwrap();
    }

    fn descriptor_for(
        id: DeviceId,
        secrets: [u8; 64],
        name: &str,
    ) -> koru_device::DeviceDescriptorV1 {
        let signing = koru_device::DeviceSigningKey::from_secret_bytes(
            secrets[..32].try_into().expect("slice length"),
        );
        let endpoint = koru_device::ReplicationEndpointSecret::from_secret_bytes(
            secrets[32..].try_into().expect("slice length"),
        );
        koru_device::DeviceDescriptorV1::new(id, name.into(), &signing, endpoint.endpoint_id(), 1)
    }

    #[test]
    fn a_rename_updates_both_identity_files_and_reopens_cleanly() {
        let directory = tempfile::tempdir().unwrap();
        let id = DeviceId::new();
        let signing = koru_device::DeviceSigningKey::from_secret_bytes([7; 32]);
        let mut trust =
            TrustState::open(directory.path(), descriptor_for(id, [7; 64], "Android")).unwrap();
        trust.rename_local("Pixel8".into(), &signing).unwrap();

        // The rename is a gossipable record, and both stores carry the name —
        // the split that used to brick the device on its next start.
        let reopened = TrustState::open(directory.path(), descriptor_for(id, [7; 64], "Android"))
            .expect("a renamed device must still open");
        assert_eq!(reopened.local_descriptor().unwrap().display_name, "Pixel8");
        let on_disk: koru_device::DeviceDescriptorV1 =
            serde_json::from_slice(&std::fs::read(directory.path().join("device.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk.display_name, "Pixel8");
    }

    #[test]
    fn a_name_only_difference_is_not_an_identity_mismatch() {
        // Trust has a renamed descriptor while the startup copy is stale.
        // Identity is id and keys, never the name.
        let directory = tempfile::tempdir().unwrap();
        let id = DeviceId::new();
        let signing = koru_device::DeviceSigningKey::from_secret_bytes([7; 32]);
        let mut trust =
            TrustState::open(directory.path(), descriptor_for(id, [7; 64], "Android")).unwrap();
        trust.rename_local("Pixel8".into(), &signing).unwrap();
        // Reopen with the pre-rename descriptor, the way startup does.
        TrustState::open(directory.path(), descriptor_for(id, [7; 64], "Android"))
            .expect("a name change must not read as a different identity");
    }

    #[test]
    fn a_mismatched_identity_degrades_sync_but_keeps_the_local_store() {
        let directory = tempfile::tempdir().unwrap();
        {
            let host =
                MobileHost::open(directory.path().to_path_buf(), [8; 64], "Android", true).unwrap();
            with_test_store(&host, |store| {
                store
                    .create_profile(
                        "Mobile",
                        None,
                        None,
                        koru_store_core::Locale::parse("en").unwrap(),
                        Timestamp::from_millis(1),
                    )
                    .unwrap()
            });
        }
        // The vault and the descriptor now disagree — a genuinely different
        // identity, not a rename.
        let foreign = descriptor_for(DeviceId::new(), [9; 64], "Android");
        std::fs::write(
            directory.path().join("device.json"),
            serde_json::to_vec_pretty(&foreign).unwrap(),
        )
        .unwrap();

        let host = MobileHost::open(directory.path().to_path_buf(), [8; 64], "Android", true)
            .expect("an identity mismatch must not brick the app");
        assert!(host.trust.lock().unwrap().is_none());
        assert_eq!(host.status().state, "unavailable");
        assert_eq!(
            host.devices().error_code.as_deref(),
            Some("device_identity_mismatch")
        );
        // ...and the local Store answers as if nothing happened.
        assert_eq!(host.inventory().unwrap().profiles.len(), 1);
    }

    /// **Opening a file nobody reachable holds answers, and does not hang.**
    /// With nothing linked and no Cloud, the export used to wait on a peer
    /// hydration that had parked on *no linked devices*, forever; the
    /// person saw a spinner that nothing would end. Now the peer wait ends
    /// at the park, the engine is asked, and a stable code comes back. The
    /// same bound holds for a picture load.
    #[test]
    fn opening_a_file_nobody_reachable_holds_answers_a_code_rather_than_hanging() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [44; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Files", Timestamp::from_millis(2))
                .unwrap()
        });
        let id = workspace.id.to_storage();
        let hash = format!("b3:{}", "7".repeat(64));
        let destination = directory.path().join("exports").join("missing.png");
        let (send, receive) = std::sync::mpsc::channel();
        let opener = Arc::clone(&host);
        let opener_id = id.clone();
        let opener_hash = hash.clone();
        std::thread::spawn(move || {
            let exported =
                opener.export_blob(&opener_id, &opener_hash, destination.to_str().unwrap());
            let loaded = opener.load_blob(&opener_id, &opener_hash);
            let _ = send.send((exported, loaded));
        });
        let (exported, loaded) = receive
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("an answer within a minute, not a spinner");
        // The engine's own reason wins over "nobody reachable has it": this
        // Workspace is not connected to Koru Cloud, which is what the file
        // sheet says too.
        assert_eq!(exported, Err("cloud_access_unavailable".to_owned()));
        assert_eq!(loaded, Err("no_linked_devices".to_owned()));
    }

    #[test]
    fn workspace_mutations_reach_the_embedded_store() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [43; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Original", Timestamp::from_millis(2))
                .unwrap()
        });
        let id = workspace.id.to_storage();
        let workspace_id = workspace.id;
        let (manifest, before_rename, before_snapshot) = with_test_store(&host, move |store| {
            let manifest = store.manifest_id(workspace_id).unwrap();
            let version = store.version(workspace_id, manifest).unwrap();
            let snapshot = store.snapshot(workspace_id, manifest).unwrap();
            (manifest, version, snapshot)
        });

        host.mutate_workspace(&id, "rename", "Renamed").unwrap();
        let rename_update = with_test_store(&host, move |store| {
            store
                .updates_since(workspace_id, manifest, &before_rename)
                .unwrap()
        });
        let mut replica = koru_store_core::Manifest::from_snapshot(&before_snapshot).unwrap();
        replica.import(&rename_update).unwrap();
        assert_eq!(
            replica.name().as_deref(),
            Some("Renamed"),
            "the mobile mutation must create replicable manifest bytes"
        );
        host.mutate_workspace(&id, "archive", "").unwrap();
        let archived = with_test_store(&host, move |store| store.workspace(workspace_id).unwrap());
        assert_eq!(archived.name, "Renamed");
        assert!(archived.archived);

        host.mutate_workspace(&id, "delete", "").unwrap();
        assert!(with_test_store(&host, move |store| {
            store
                .workspace_including_deleted(workspace.id)
                .unwrap()
                .deleted_at
                .is_some()
        }));
        host.mutate_workspace(&id, "restore", "").unwrap();
        assert!(with_test_store(&host, move |store| store
            .workspace(workspace.id)
            .is_ok()));
    }

    #[test]
    fn an_imported_file_is_a_file_document_whose_root_carries_its_blob() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [45; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Workspace", Timestamp::from_millis(2))
                .unwrap()
                .id
        });

        let payload = directory.path().join("staged");
        std::fs::write(&payload, b"shared bytes").unwrap();
        let id = host
            .import_file(
                &workspace.to_storage(),
                "\u{8cc7}\u{6599}.txt",
                "text/plain",
                payload.to_str().unwrap(),
                None,
            )
            .unwrap();

        // The manifest row is what the Files tab reads.
        let entry = with_test_store(&host, move |store| {
            store
                .documents(workspace)
                .unwrap()
                .into_iter()
                .find(|entry| entry.id.to_storage() == id)
                .expect("the imported file is listed")
        });
        assert_eq!(entry.kind, koru_core::explore::KIND_FILE);
        assert_eq!(entry.mime.as_deref(), Some("text/plain"));
        assert_eq!(entry.title, "\u{8cc7}\u{6599}.txt");

        // ...and the document body is what an opener resolves the bytes through.
        let entry_id = entry.id;
        let snapshot = with_test_store(&host, move |store| {
            store.snapshot(workspace, entry_id).unwrap()
        });
        let model = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let root = model
            .children(koru_core::schema::Parent::Root)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let node = model.read(root).unwrap();
        let koru_core::node::Content::Blob { hash, mime, .. } = node.content else {
            panic!("an imported file must hold blob content");
        };
        assert_eq!(mime, "text/plain");
        let hash = koru_store_core::BlobHash::parse(&hash.0).unwrap();
        let bytes = with_test_store(&host, move |store| {
            store.get_blob(workspace, &hash).unwrap()
        });
        assert_eq!(bytes, b"shared bytes");
    }

    #[test]
    fn an_import_refuses_a_bad_workspace_an_unreadable_path_and_an_empty_payload() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [46; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Workspace", Timestamp::from_millis(2))
                .unwrap()
                .id
        });
        let payload = directory.path().join("staged");
        std::fs::write(&payload, b"bytes").unwrap();

        assert_eq!(
            host.import_file(
                "../etc",
                "a.txt",
                "text/plain",
                payload.to_str().unwrap(),
                None
            ),
            Err("invalid_workspace_id".to_owned()),
        );
        assert_eq!(
            host.import_file(
                &workspace.to_storage(),
                "   ",
                "text/plain",
                payload.to_str().unwrap(),
                None
            ),
            Err("invalid_file_name".to_owned()),
        );
        assert_eq!(
            host.import_file(
                &workspace.to_storage(),
                "a.txt",
                "text/plain",
                directory.path().join("absent").to_str().unwrap(),
                None
            ),
            Err("file_unreadable".to_owned()),
        );
        let empty = directory.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            host.import_file(
                &workspace.to_storage(),
                "a.txt",
                "text/plain",
                empty.to_str().unwrap(),
                None
            ),
            Err("empty_file".to_owned()),
        );
        // Nothing partial was left behind by the four refusals.
        assert!(with_test_store(&host, move |store| {
            store.documents(workspace).unwrap().is_empty()
        }));
    }

    #[test]
    fn document_rename_updates_authoritative_bytes_and_trash_updates_the_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [44; 64], "Test", true).unwrap();
        let (workspace, document) = {
            let mut model =
                koru_core::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Document").unwrap();
            model
                .insert(
                    koru_core::schema::Parent::Root,
                    koru_core::Node::document("Original").unwrap(),
                )
                .unwrap();
            let snapshot = model.export_snapshot().unwrap();
            with_test_store(&host, move |store| {
                let profile = store
                    .create_profile(
                        "Mobile",
                        None,
                        None,
                        koru_store_core::Locale::parse("en").unwrap(),
                        Timestamp::from_millis(1),
                    )
                    .unwrap();
                let workspace = store
                    .create_workspace(profile.id, "Workspace", Timestamp::from_millis(2))
                    .unwrap();
                let document = store
                    .import_document(
                        workspace.id,
                        "Original",
                        "document",
                        None,
                        snapshot,
                        Timestamp::from_millis(3),
                    )
                    .unwrap()
                    .0;
                (workspace.id, document)
            })
        };

        host.mutate_document(&document.to_storage(), "rename", "🚀 Renamed")
            .unwrap();
        let snapshot = with_test_store(&host, move |store| {
            store.snapshot(workspace, document).unwrap()
        });
        let model = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let root = model
            .children(koru_core::schema::Parent::Root)
            .unwrap()
            .into_iter()
            .find(|id| model.kind_of(*id) == Some(koru_core::Kind::Document))
            .unwrap();
        assert_eq!(
            model.read(root).unwrap().content.as_text(),
            Some("🚀 Renamed")
        );
        let title = with_test_store(&host, move |store| {
            store.documents(workspace).unwrap()[0].title.clone()
        });
        assert_eq!(title, "🚀 Renamed");

        host.mutate_document(&document.to_storage(), "trash", "")
            .unwrap();
        assert!(host.inventory().unwrap().documents.is_empty());
    }

    /// The share sheet's note field, end to end: the import attaches a
    /// kind-"note" child, seeded with the document root and the text as its
    /// first paragraph, the Files projection links it from the file without
    /// listing it, and the note opens and renames through the same document
    /// paths as any page.
    #[test]
    fn an_import_with_a_note_attaches_a_seeded_note_to_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [50; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Workspace", Timestamp::from_millis(2))
                .unwrap()
                .id
        });
        let payload = directory.path().join("staged");
        std::fs::write(&payload, b"shared bytes").unwrap();
        let file = host
            .import_file(
                &workspace.to_storage(),
                "a.txt",
                "text/plain",
                payload.to_str().unwrap(),
                Some("remember this"),
            )
            .unwrap();
        let file_id = DocumentId::parse(&file).unwrap();

        let (note, parent) = with_test_store(&host, move |store| {
            let entries = store.documents(workspace).unwrap();
            let file_entry = entries
                .iter()
                .find(|entry| entry.id == file_id)
                .expect("the imported file is listed");
            let note_entry = entries
                .iter()
                .find(|entry| entry.kind == koru_core::explore::KIND_NOTE)
                .expect("the note is registered");
            (file_entry.note, note_entry.parent)
        });
        let note = note.expect("the file names its note");
        assert_eq!(parent, Some(file_id));

        // The note body carries the text as the first paragraph under the
        // document root — the desktop's seed shape for a store-made document.
        let snapshot =
            with_test_store(&host, move |store| store.snapshot(workspace, note).unwrap());
        let model = koru_core::WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let root = model
            .children(koru_core::schema::Parent::Root)
            .unwrap()
            .into_iter()
            .find(|id| model.kind_of(*id) == Some(koru_core::Kind::Document))
            .expect("the seeded note has a document root");
        let paragraph = model
            .children(koru_core::schema::Parent::Node(root))
            .unwrap()
            .into_iter()
            .next()
            .expect("the note text is its first paragraph");
        assert_eq!(
            model.read(paragraph).unwrap().content.as_text(),
            Some("remember this")
        );

        // The Files projection hides the note and links it from the file.
        let inventory = host.inventory().unwrap();
        assert_eq!(inventory.documents.len(), 1);
        assert_eq!(inventory.documents[0].id, file);
        assert_eq!(
            inventory.documents[0].note_id.as_deref(),
            Some(note.to_storage().as_str())
        );

        // ...and the note opens and renames through the normal document paths.
        let view = host.view_document(&note.to_storage()).unwrap();
        assert_eq!(view.title, "a.txt");
        assert_eq!(view.blocks.len(), 1);
        assert_eq!(
            view.blocks[0]
                .rich_text
                .as_ref()
                .map(|rich| rich.text.as_str()),
            Some("remember this")
        );
        host.mutate_document(&note.to_storage(), "rename", "Renamed note")
            .unwrap();
        let view = host.view_document(&note.to_storage()).unwrap();
        assert_eq!(view.title, "Renamed note");
    }

    /// The row's *Add note*: ensure is idempotent, the first note arrives
    /// seeded-but-empty so the open path has a document root to project, and
    /// anything that is not a file is refused by the store.
    #[test]
    fn ensure_file_note_is_idempotent_seeded_and_files_only() {
        let directory = tempfile::tempdir().unwrap();
        let host =
            MobileHost::open(directory.path().to_path_buf(), [51; 64], "Test", true).unwrap();
        let workspace = with_test_store(&host, |store| {
            let profile = store
                .create_profile(
                    "Mobile",
                    None,
                    None,
                    koru_store_core::Locale::parse("en").unwrap(),
                    Timestamp::from_millis(1),
                )
                .unwrap();
            store
                .create_workspace(profile.id, "Workspace", Timestamp::from_millis(2))
                .unwrap()
                .id
        });
        let payload = directory.path().join("staged");
        std::fs::write(&payload, b"shared bytes").unwrap();
        let file = host
            .import_file(
                &workspace.to_storage(),
                "a.txt",
                "text/plain",
                payload.to_str().unwrap(),
                None,
            )
            .unwrap();

        let first = host.ensure_file_note(&file).unwrap();
        let again = host.ensure_file_note(&file).unwrap();
        assert_eq!(first, again, "ensure never makes a second note");

        // Seeded on creation: the empty note opens through the normal view
        // path with the file's title and no blocks yet.
        let view = host.view_document(&first).unwrap();
        assert_eq!(view.title, "a.txt");
        assert!(view.blocks.is_empty());

        // A page is not a file; the store's own refusal travels out.
        let page = with_test_store(&host, move |store| {
            let mut model =
                koru_core::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Document").unwrap();
            model
                .insert(
                    koru_core::schema::Parent::Root,
                    koru_core::Node::document("Page").unwrap(),
                )
                .unwrap();
            store
                .import_document(
                    workspace,
                    "Page",
                    "document",
                    None,
                    model.export_snapshot().unwrap(),
                    Timestamp::from_millis(3),
                )
                .unwrap()
                .0
        });
        assert_eq!(
            host.ensure_file_note(&page.to_storage()),
            Err("invalid_request".to_owned())
        );
        assert_eq!(
            host.ensure_file_note("not-a-document"),
            Err("invalid_document_id".to_owned())
        );
    }

    #[tokio::test]
    async fn an_embedded_store_rename_reaches_an_already_connected_replica() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a_worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::mobile(a_dir.path().to_path_buf(), "mobile-a"),
        )
        .spawn()
        .unwrap();
        let b_worker = koru_store_server::StoreWorker::new(
            koru_store_server::WorkerConfig::mobile(b_dir.path().to_path_buf(), "mobile-b"),
        )
        .spawn()
        .unwrap();
        let a_store = a_worker.client();
        let b_store = b_worker.client();
        let workspace = a_store
            .with_store(|store| {
                let profile = store
                    .create_profile(
                        "Profile",
                        None,
                        None,
                        koru_store_core::Locale::parse("en").unwrap(),
                        Timestamp::from_millis(1),
                    )
                    .unwrap();
                store
                    .create_workspace(profile.id, "Original", Timestamp::from_millis(2))
                    .unwrap()
            })
            .unwrap();

        let a_endpoint = koru_device::ReplicationEndpointSecret::generate();
        let b_endpoint = koru_device::ReplicationEndpointSecret::generate();
        let a_id = DeviceId::new();
        let b_id = DeviceId::new();
        let relationship = koru_device::RelationshipId::new();
        let a = koru_network_iroh::RuntimeHandle::spawn_with_store(
            koru_network_iroh::IrohNetworkConfig::isolated(),
            Arc::new(MobileReplicationStore {
                store: a_store.clone(),
            }),
        );
        let b = koru_network_iroh::RuntimeHandle::spawn_with_store(
            koru_network_iroh::IrohNetworkConfig::isolated(),
            Arc::new(MobileReplicationStore {
                store: b_store.clone(),
            }),
        );
        a.provision_endpoint(a_endpoint.secret_bytes());
        b.provision_endpoint(b_endpoint.secret_bytes());
        tokio::time::timeout(Duration::from_secs(5), async {
            while a.endpoint_addr().is_none() || b.endpoint_addr().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("isolated endpoints were not ready");
        a.replace_authorization(koru_sync_control::AuthorizationSnapshot {
            session_id: koru_sync_control::ControlSessionId::new(),
            sequence: 1,
            local_device: a_id,
            peers: vec![koru_sync_control::AuthorizedPeer {
                relationship_id: relationship,
                device_id: b_id,
                endpoint_id: b_endpoint.endpoint_id(),
                all_data: true,
                authorized_profiles: Vec::new(),
                workspaces: vec![workspace.id],
                via: None,
                display_name: None,
                build: Some(koru_device::BuildPayload {
                    peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                    ..Default::default()
                }),
            }],
            local_build: None,
        });
        b.replace_authorization(koru_sync_control::AuthorizationSnapshot {
            session_id: koru_sync_control::ControlSessionId::new(),
            sequence: 1,
            local_device: b_id,
            peers: vec![koru_sync_control::AuthorizedPeer {
                relationship_id: relationship,
                device_id: a_id,
                endpoint_id: a_endpoint.endpoint_id(),
                all_data: true,
                authorized_profiles: Vec::new(),
                workspaces: Vec::new(),
                via: None,
                display_name: None,
                build: Some(koru_device::BuildPayload {
                    peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
                    ..Default::default()
                }),
            }],
            local_build: None,
        });
        a.connect_peer_at(b_id, b.endpoint_addr().unwrap());
        let workspace_id = workspace.id;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if b_store
                    .with_store(move |store| {
                        store
                            .workspace(workspace_id)
                            .is_ok_and(|row| row.name == "Original")
                    })
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the empty replica did not bootstrap");

        a_store
            .with_store(move |store| {
                store.rename_workspace(workspace_id, "Renamed", Timestamp::from_millis(3))
            })
            .unwrap()
            .unwrap();
        a.notify_local_change();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if b_store
                    .with_store(move |store| {
                        store
                            .workspace(workspace_id)
                            .is_ok_and(|row| row.name == "Renamed")
                    })
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the connected replica did not project the manifest rename");
        a.clear();
        b.clear();
    }
}
