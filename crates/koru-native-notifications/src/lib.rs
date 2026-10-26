// SPDX-License-Identifier: MPL-2.0

//! The small boundary between Koru's reminder scheduler and an operating
//! system's notification service.
//!
//! This crate owns vocabulary, not policy. It deliberately names no profile,
//! entity, reminder-domain, Store, clock, thread, window, or platform API. The
//! Agent worker supplies an already-resolved instant and display text; a native
//! implementation registers it. That keeps the fake used by scheduler tests
//! identical to the Windows and macOS implementations without pulling either
//! platform into the domain.
//!
//! Registration identity is stable and replacement is part of the contract:
//! scheduling the same [`RegistrationId`] again replaces the old pending or
//! delivered notification. Withdrawal removes both pending and delivered
//! copies. Implementations may need separate native calls to uphold that one
//! Koru operation.

mod mac_owner;
#[cfg(target_os = "macos")]
mod macos;
mod sounds;
#[cfg(windows)]
mod windows;

pub use mac_owner::{
    InvalidMacRootRoute, MAC_ACTION_HANDLED, MAC_ACTION_OPEN, MAC_ACTION_SNOOZE,
    MAC_CATEGORY_OPEN_ONLY, MAC_CATEGORY_REMINDER, MAC_OWNER_APP, MAC_OWNER_BUNDLE_ID,
    MAC_OWNER_EXECUTABLE, MAX_MAC_OWNER_MESSAGE, MAX_MAC_ROOT_LEN, MacActionTitles,
    MacActivationLaunch, MacOwnerRequest, MacOwnerResponse, MacOwnerValue, MacRootRoute,
    activation_from_mac_response, decode_mac_root_route, mac_agent_executable_from_owner,
    mac_agent_launch_args, mac_owner_executable_from, mac_owner_info_plist_from,
    namespace_from_native_id,
};
pub use sounds::{
    BUNDLED_TONES, BundledTone, SoundAsset, WavError, preview, read_sound_asset, sound_asset_roots,
    validate_notification_wav,
};

#[cfg(target_os = "macos")]
pub use macos::{run_notification_owner, run_notification_owner_app};

use serde::{Deserialize, Serialize};

/// The package application that owns Windows notification history and COM
/// activation. It is deliberately not the long-running Agent application:
/// Windows lifecycle-manages one application identity as one activation unit.
pub const WINDOWS_NOTIFICATION_APPLICATION_ID: &str = "KoruNotification";

/// Whether this process currently has the Windows package identity declared by
/// the release artifact. This is a runtime fact, not a compile-time promise.
pub fn windows_package_identity_available() -> bool {
    #[cfg(windows)]
    return ::windows::ApplicationModel::Package::Current().is_ok();
    #[cfg(not(windows))]
    false
}

const MAX_OPAQUE_ACTIVATION_LEN: usize = 256;

/// A content-free stable key chosen by the scheduler.
///
/// It normally encodes or hashes an occurrence identity, but this mechanism
/// crate neither knows nor parses that meaning. Native activation returns it
/// to the Agent, whose content-free ledger resolves it back to domain state.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
#[serde(try_from = "String", into = "String")]
pub struct RegistrationId(String);

impl RegistrationId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidRegistrationId> {
        let value = value.into();
        // Keep one bounded transport-safe alphabet for ledgers, activation,
        // and native adapters. An adapter may derive a stricter platform token
        // without changing this stable Koru identity.
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(InvalidRegistrationId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InvalidRegistrationId;

impl TryFrom<String> for RegistrationId {
    type Error = InvalidRegistrationId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RegistrationId> for String {
    fn from(value: RegistrationId) -> Self {
        value.0
    }
}

impl std::fmt::Display for InvalidRegistrationId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("registration id is not a portable native identifier")
    }
}

impl std::error::Error for InvalidRegistrationId {}

/// The three synchronized actions phase one promises.
///
/// The native surface only reports which action was chosen. Snooze duration
/// selection belongs to the host flow and never becomes platform vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Open,
    Snooze,
    Handled,
}

/// The complete, content-free payload accepted from an OS activation.
///
/// This is safe to cross a native process-activation boundary: it contains only
/// a schema number, a registration key, and a bounded action. A profile,
/// entity target, title, or reminder phrase must never be added here. Once the
/// Agent receives it, the ledger resolves the registration and navigation
/// reuses its existing private `Router::open(Target)` path. The entity target
/// itself never travels on the command line.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub schema_version: u8,
    pub registration: RegistrationId,
    pub action: Action,
}

impl Activation {
    pub const SCHEMA_VERSION: u8 = 1;

    pub fn new(registration: RegistrationId, action: Action) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            registration,
            action,
        }
    }

    pub fn validate(&self) -> Result<(), UnsupportedActivationVersion> {
        (self.schema_version == Self::SCHEMA_VERSION)
            .then_some(())
            .ok_or(UnsupportedActivationVersion(self.schema_version))
    }

    /// Encode the only payload an operating-system activation may hand to an
    /// Agent process. URL-safe base64 keeps the value one argv token without
    /// putting a profile, target, title, or authored text on the command line.
    pub fn encode_opaque(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(self).expect("Activation is an infallible fixed-shape value"),
        )
    }

    pub fn decode_opaque(value: &str) -> Result<Self, InvalidOpaqueActivation> {
        use base64::Engine as _;
        if value.is_empty() || value.len() > MAX_OPAQUE_ACTIVATION_LEN {
            return Err(InvalidOpaqueActivation);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| InvalidOpaqueActivation)?;
        let activation: Self =
            serde_json::from_slice(&bytes).map_err(|_| InvalidOpaqueActivation)?;
        activation.validate().map_err(|_| InvalidOpaqueActivation)?;
        Ok(activation)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InvalidOpaqueActivation;

impl std::fmt::Display for InvalidOpaqueActivation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("notification activation is not a supported content-free envelope")
    }
}

impl std::error::Error for InvalidOpaqueActivation {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UnsupportedActivationVersion(pub u8);

/// What sound the operating system should play at delivery.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    Silent,
    System,
    Bundled(BundledTone),
}

/// Which native actions a delivered notification may expose.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ActionSet {
    Reminder,
    OpenOnly,
}

/// User-visible notification content, held only for the native registration
/// call. Implementations must not persist it in Koru's device ledger or put it
/// in diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Content {
    pub title: String,
    pub body: Option<String>,
}

/// User-visible labels for the three native reminder actions.
///
/// Policy supplies these in the person's language. Platform adapters must not
/// embed an English fallback of their own: that would make native UI disagree
/// with Koru while still looking superficially complete.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ActionLabels {
    pub open: String,
    pub snooze: String,
    pub handled: String,
}

impl Default for ActionLabels {
    fn default() -> Self {
        Self {
            open: "Open".into(),
            snooze: "Snooze".into(),
            handled: "Handled".into(),
        }
    }
}

/// A resolved request from the scheduler to the native adapter.
///
/// `deliver_at_unix_ms` is an instant. Floating/calendar semantics are resolved
/// by the Agent and repaired after a device-zone change before they reach this
/// boundary; an adapter must not reinterpret the instant as local calendar
/// components.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub registration: RegistrationId,
    pub deliver_at_unix_ms: i64,
    pub content: Content,
    pub action_labels: ActionLabels,
    pub sound: Sound,
    pub actions: ActionSet,
}

/// An opaque platform handle suitable for a content-free local ledger.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NativeHandle(pub String);

/// A content-free native namespace for one selected Agent root.
///
/// The source key is hashed immediately: filesystem paths and channel names do
/// not become notification identifiers. FNV-1a is used for the same reason as
/// Koru's rendezvous names: it is a stable name, not a security boundary, and
/// must not move when Rust changes its process-randomized default hasher.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
#[serde(try_from = "String", into = "String")]
pub struct NotificationNamespace(String);

impl NotificationNamespace {
    pub fn from_stable_key(key: &str) -> Self {
        Self(stable_native_token(key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(any(windows, target_os = "macos", test))]
    pub(crate) fn native_id(&self, registration: &RegistrationId) -> String {
        format!("koru.{}.{}", self.0, registration.as_str())
    }
}

/// A stable content-free token for native identifier fields with a strict
/// 16-character limit. This is a name, not a security boundary.
pub(crate) fn stable_native_token(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InvalidNotificationNamespace;

impl TryFrom<String> for NotificationNamespace {
    type Error = InvalidNotificationNamespace;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            Ok(Self(value))
        } else {
            Err(InvalidNotificationNamespace)
        }
    }
}

impl From<NotificationNamespace> for String {
    fn from(value: NotificationNamespace) -> Self {
        value.0
    }
}

impl std::fmt::Display for InvalidNotificationNamespace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("notification namespace is not a bounded native identifier")
    }
}

impl std::error::Error for InvalidNotificationNamespace {}

/// Windows adds the content-free selected-root namespace needed to route two
/// identical registration ids without exposing either root. The broker strips
/// this wrapper before forwarding the ordinary platform-neutral activation to
/// the selected Agent.
#[cfg(windows)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct WindowsActivationEnvelope {
    schema_version: u8,
    pub namespace: NotificationNamespace,
    pub registration: RegistrationId,
    pub action: Action,
}

#[cfg(windows)]
impl WindowsActivationEnvelope {
    pub const SCHEMA_VERSION: u8 = 1;

    pub fn new(namespace: NotificationNamespace, activation: Activation) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            namespace,
            registration: activation.registration,
            action: activation.action,
        }
    }

    pub fn into_activation(self) -> Activation {
        Activation::new(self.registration, self.action)
    }

    pub fn encode_opaque(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(self)
                .expect("WindowsActivationEnvelope is an infallible fixed-shape value"),
        )
    }

    pub fn decode_opaque(value: &str) -> Result<Self, InvalidOpaqueActivation> {
        use base64::Engine as _;
        if value.is_empty() || value.len() > MAX_OPAQUE_ACTIVATION_LEN {
            return Err(InvalidOpaqueActivation);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| InvalidOpaqueActivation)?;
        let envelope: Self = serde_json::from_slice(&bytes).map_err(|_| InvalidOpaqueActivation)?;
        if envelope.schema_version != Self::SCHEMA_VERSION {
            return Err(InvalidOpaqueActivation);
        }
        Ok(envelope)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    NotDetermined,
    Allowed,
    Denied,
    Unavailable,
}

/// Static and current facts kept separate so a permission change does not look
/// like the platform lost a capability.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capability {
    pub authorization: Authorization,
    pub actions: bool,
    pub stable_replacement: bool,
    pub withdrawal: bool,
    pub custom_sound: bool,
    pub activation_while_stopped: bool,
}

/// Platform/package facts that can be inspected without calling the native
/// notification service or asking about authorization.
///
/// Scheduler recovery uses this before touching a missing/corrupt ledger. An
/// honestly unsupported package must settle as `NativeUnsupported`, rather
/// than turning an unregistered notifier lookup into a perpetual platform
/// retry. Settings still uses [`NativeNotifications::capability`] for the
/// current authorization result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeliverySupport {
    pub actions: bool,
    pub stable_replacement: bool,
    pub withdrawal: bool,
    pub custom_sound: bool,
    pub activation_while_stopped: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    Unsupported,
    PermissionDenied,
    AssetMissing,
    InvalidRequest,
    Platform,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Failure {
    pub code: FailureCode,
    /// Developer detail. It is never suitable for display and must not contain
    /// notification content.
    pub detail: String,
}

/// Implemented by fakes in scheduler tests and by one adapter per desktop OS.
///
/// Every method may block on OS or disk work and therefore belongs on the Agent
/// worker, never the winit thread.
pub trait NativeNotifications: Send {
    fn delivery_support(&self) -> DeliverySupport;

    fn capability(&mut self) -> Result<Capability, Failure>;

    /// Add or replace one notification by its stable registration id.
    fn schedule(&mut self, request: Schedule) -> Result<NativeHandle, Failure>;

    /// Remove pending and already-delivered native state for this id.
    fn withdraw(&mut self, registration: &RegistrationId) -> Result<(), Failure>;

    /// Remove every pending and delivered notification owned by this adapter's
    /// root namespace. This is used only when the rebuildable ledger cannot
    /// prove which obsolete registrations still exist in the OS.
    fn reset_namespace(&mut self) -> Result<(), Failure>;

    fn open_settings(&mut self) -> Result<(), Failure>;

    /// The only operation allowed to trigger an authorization prompt. Merely
    /// inspecting capability, scheduling, or testing must never prompt.
    fn request_authorization(&mut self) -> Result<Authorization, Failure> {
        Err(Failure {
            code: FailureCode::Unsupported,
            detail: "notification authorization cannot be requested on this platform".into(),
        })
    }

    /// Send immediately without creating reminder/profile state.
    fn send_test(&mut self, content: Content, sound: Sound) -> Result<(), Failure>;
}

impl<T: NativeNotifications + ?Sized> NativeNotifications for Box<T> {
    fn delivery_support(&self) -> DeliverySupport {
        (**self).delivery_support()
    }
    fn capability(&mut self) -> Result<Capability, Failure> {
        (**self).capability()
    }
    fn schedule(&mut self, request: Schedule) -> Result<NativeHandle, Failure> {
        (**self).schedule(request)
    }
    fn withdraw(&mut self, registration: &RegistrationId) -> Result<(), Failure> {
        (**self).withdraw(registration)
    }
    fn reset_namespace(&mut self) -> Result<(), Failure> {
        (**self).reset_namespace()
    }
    fn open_settings(&mut self) -> Result<(), Failure> {
        (**self).open_settings()
    }
    fn request_authorization(&mut self) -> Result<Authorization, Failure> {
        (**self).request_authorization()
    }
    fn send_test(&mut self, content: Content, sound: Sound) -> Result<(), Failure> {
        (**self).send_test(content, sound)
    }
}

/// Construct the shipping adapter without letting platform APIs leak into the
/// scheduler. Unsupported targets retain an explicit unavailable adapter.
pub fn platform_notifications(namespace: NotificationNamespace) -> Box<dyn NativeNotifications> {
    platform_notifications_for_root(namespace, None)
}

/// Construct the shipping adapter with the selected Agent root needed to
/// relaunch the same singleton after a cold native activation.
pub fn platform_notifications_for_root(
    namespace: NotificationNamespace,
    explicit_root: Option<std::path::PathBuf>,
) -> Box<dyn NativeNotifications> {
    #[cfg(not(target_os = "macos"))]
    let _ = &explicit_root;
    #[cfg(windows)]
    return Box::new(windows::WindowsNotifications::new(
        namespace,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    ));
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacNotifications::new(namespace, explicit_root));
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (namespace, explicit_root);
        Box::new(UnavailableNotifications)
    }
}

/// Construct a platform adapter whose actionable Windows capability follows
/// the Agent's COM activation host. Other platforms do not need this process-
/// local readiness seam.
pub fn platform_notifications_with_activation(
    namespace: NotificationNamespace,
    explicit_root: Option<std::path::PathBuf>,
    activation_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Box<dyn NativeNotifications> {
    #[cfg(windows)]
    {
        let _ = explicit_root;
        return Box::new(windows::WindowsNotifications::new(
            namespace,
            activation_ready,
        ));
    }
    #[cfg(not(windows))]
    {
        let _ = activation_ready;
        platform_notifications_for_root(namespace, explicit_root)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
struct UnavailableNotifications;

#[cfg(not(any(windows, target_os = "macos")))]
impl NativeNotifications for UnavailableNotifications {
    fn delivery_support(&self) -> DeliverySupport {
        DeliverySupport {
            actions: false,
            stable_replacement: false,
            withdrawal: false,
            custom_sound: false,
            activation_while_stopped: false,
        }
    }

    fn capability(&mut self) -> Result<Capability, Failure> {
        Err(unsupported())
    }
    fn schedule(&mut self, _: Schedule) -> Result<NativeHandle, Failure> {
        Err(unsupported())
    }
    fn withdraw(&mut self, _: &RegistrationId) -> Result<(), Failure> {
        Err(unsupported())
    }
    fn reset_namespace(&mut self) -> Result<(), Failure> {
        Err(unsupported())
    }
    fn open_settings(&mut self) -> Result<(), Failure> {
        Err(unsupported())
    }
    fn send_test(&mut self, _: Content, _: Sound) -> Result<(), Failure> {
        Err(unsupported())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unsupported() -> Failure {
    Failure {
        code: FailureCode::Unsupported,
        detail: "native notifications are unavailable on this target".into(),
    }
}

#[cfg(test)]
mod tests;
