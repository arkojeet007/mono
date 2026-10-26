// SPDX-License-Identifier: MPL-2.0

//! Durable device identity owned by the Agent.
//!
//! Public signed facts live in SQLite. Private keys live behind this
//! platform's protection — current-user DPAPI blobs on Windows; the Keychain
//! on a signed Mac build and an owner-only file on an internal one
//! ([`keychain`], M17) — and are never reminted after initialization, except by
//! an explicit start-over ([`IdentityManager::reset`]).

use std::path::{Path, PathBuf};

pub use koru_account_runtime::identity::{PairingPersistenceState, SqliteTrustRepository};
use koru_device::{
    DeviceDescriptorV1, DeviceError, DeviceId, DeviceSigningKey, ReplicationEndpointSecret,
    SecretBytes, SecretName, SecretStore, StoredLocalDevice, TrustRepository,
};

use crate::{Device, Paths};

// Compiled into the Windows tests too, so the Keychain cutover is exercised
// off a Mac (DPAPI stands in for the plaintext); only the system Keychain is macOS-only.
#[cfg(any(target_os = "macos", all(test, windows)))]
mod keychain;
pub mod profile_lifecycle;

/// The store this build keeps the device's secrets in. One choice, made once,
/// so `open` and `reset` can never disagree about where the keys are.
enum DeviceSecrets {
    File(ProtectedSecretStore),
    #[cfg(any(target_os = "macos", all(test, windows)))]
    Keychain(keychain::KeychainSecrets),
}

impl DeviceSecrets {
    fn for_paths(paths: &Paths) -> Self {
        #[cfg(target_os = "macos")]
        if !koru_channel::ReleaseChannel::this_build_is_internal() {
            return Self::Keychain(keychain::KeychainSecrets::system(paths.secrets_dir()));
        }
        Self::File(ProtectedSecretStore::new(paths.secrets_dir()))
    }

    /// Move the secrets to where this build keeps them, before anything reads
    /// them. Only the Keychain has anything to move.
    fn prepare(&self) -> Result<(), DeviceError> {
        match self {
            Self::File(_) => Ok(()),
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(store) => store.prepare(),
        }
    }

    fn holds(&self, name: SecretName) -> bool {
        match self {
            Self::File(store) => store.path(name).exists(),
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(store) => store.holds(name),
        }
    }

    /// Undo a failed first initialization, so a retry starts clean.
    fn forget(&self, name: SecretName) {
        match self {
            Self::File(store) => {
                let _ = std::fs::remove_file(store.path(name));
            }
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(_) => {}
        }
    }

    /// For a reset whose keys cannot be kept.
    fn retire_into(&self, aside: &Path, paths: &Paths) -> Result<(), DeviceError> {
        match self {
            Self::File(_) => set_aside(&paths.secrets_dir(), aside),
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(store) => store.retire_into(aside),
        }
    }
}

impl SecretStore for DeviceSecrets {
    fn load(&self, name: SecretName) -> Result<Option<SecretBytes>, DeviceError> {
        match self {
            Self::File(store) => store.load(name),
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(store) => store.load(name),
        }
    }
    fn store_new(&self, name: SecretName, value: &SecretBytes) -> Result<(), DeviceError> {
        match self {
            Self::File(store) => store.store_new(name, value),
            #[cfg(any(target_os = "macos", all(test, windows)))]
            Self::Keychain(store) => store.store_new(name, value),
        }
    }
}

pub struct IdentityManager;

impl IdentityManager {
    pub fn open(paths: &Paths, now_millis: i64) -> Result<StoredLocalDevice, DeviceError> {
        Self::open_with(paths, DeviceSecrets::for_paths(paths), now_millis)
    }

    fn open_with(
        paths: &Paths,
        secrets: DeviceSecrets,
        now_millis: i64,
    ) -> Result<StoredLocalDevice, DeviceError> {
        let db_path = paths.identity_db();
        secrets.prepare()?;
        let db_existed = db_path.exists();
        let signing_exists = secrets.holds(SecretName::DeviceSigning);
        let endpoint_exists = secrets.holds(SecretName::ReplicationEndpoint);

        if !db_existed && (signing_exists || endpoint_exists) {
            return Err(DeviceError::new(
                "incomplete_identity",
                "protected key material exists without an initialized identity database",
            ));
        }

        let mut repository = SqliteTrustRepository::open(&db_path)?;
        if db_existed {
            let descriptor = repository.local_device()?.ok_or_else(|| {
                DeviceError::new(
                    "incomplete_identity",
                    "identity database has no local device",
                )
            })?;
            let signing = load_signing(&secrets)?;
            let endpoint = load_endpoint(&secrets)?;
            validate_keys(&descriptor, &signing, &endpoint)?;
            return Ok(StoredLocalDevice {
                descriptor,
                signing_key: signing,
                endpoint_secret: endpoint,
            });
        }

        let legacy = Device::open(&paths.device_file(), now_millis)
            .map_err(|e| DeviceError::new("legacy_identity", e.to_string()))?;
        let device_id = DeviceId::parse(&legacy.id).ok_or_else(|| {
            DeviceError::new(
                "legacy_identity",
                "device.json contains an invalid device id",
            )
        })?;
        let signing = DeviceSigningKey::generate();
        let endpoint = ReplicationEndpointSecret::generate();
        let descriptor = DeviceDescriptorV1::new(
            device_id,
            legacy.name.clone(),
            &signing,
            endpoint.endpoint_id(),
            legacy.created_at_millis,
        );

        if let Err(error) = secrets.store_new(
            SecretName::DeviceSigning,
            &SecretBytes(signing.secret_bytes()),
        ) {
            return Err(error);
        }
        if let Err(error) = secrets.store_new(
            SecretName::ReplicationEndpoint,
            &SecretBytes(endpoint.secret_bytes()),
        ) {
            secrets.forget(SecretName::DeviceSigning);
            return Err(error);
        }
        if let Err(error) = repository.initialize(&descriptor) {
            secrets.forget(SecretName::DeviceSigning);
            secrets.forget(SecretName::ReplicationEndpoint);
            return Err(error);
        }

        Ok(StoredLocalDevice {
            descriptor,
            signing_key: signing,
            endpoint_secret: endpoint,
        })
    }
}

/// The marker a `ResetIdentity` request leaves for the next start. The reset
/// itself runs there, before anything has opened `agent.db`: a running Agent
/// may hold that file open, and Windows refuses to move an open file.
const RESET_MARKER: &str = "reset-identity.requested";

impl IdentityManager {
    /// Ask the next start to begin again as a new device. Written by the
    /// running Agent, which then stops; [`Self::reset_if_requested`] does the
    /// work. The marker holds nothing — its presence is the request.
    pub fn request_reset(paths: &Paths) -> Result<(), DeviceError> {
        std::fs::create_dir_all(paths.agent_dir()).map_err(io_error)?;
        std::fs::write(paths.agent_dir().join(RESET_MARKER), b"").map_err(io_error)
    }

    /// Carry out a reset the last run asked for, before the identity opens.
    /// The marker is removed whether or not the reset succeeds: a reset that
    /// failed once is reported through the identity that then fails to open,
    /// and retrying it on every start would be a loop nobody asked for.
    /// Returns where the old identity was set aside, when one was.
    pub fn reset_if_requested(paths: &Paths, now_millis: i64) -> Option<PathBuf> {
        let marker = paths.agent_dir().join(RESET_MARKER);
        if !marker.exists() {
            return None;
        }
        let outcome = Self::reset(paths, now_millis);
        let _ = std::fs::remove_file(&marker);
        match outcome {
            Ok(aside) => {
                tracing::warn!(
                    set_aside = %aside.display(),
                    "device identity reset; the previous identity is set aside"
                );
                Some(aside)
            }
            Err(error) => {
                tracing::error!(code = error.code, "device identity reset failed: {error}");
                None
            }
        }
    }

    /// Start over as a new device — the desktop twin of the phones'
    /// `reset_identity` (ADR-413).
    ///
    /// **Only a broken identity is reset.** One that opens is refused with
    /// `identity_intact`: it is what every linked device trusts.
    ///
    /// What happens, in an order where a crash at any step leaves a state the
    /// next start either finishes or reports: `device.json` moves into
    /// `identity-set-aside-<millis>/` first — so a start interrupted from here
    /// on mints a new Device ID rather than reusing the old one — then
    /// `agent.db` and its WAL. The keys this device holds are **kept** when both
    /// read back; if either cannot, the whole `secrets/` folder is set aside too
    /// and new keys are minted. A new descriptor is signed with a **new Device
    /// ID** and the old display name, and `device.json` is written to match.
    /// Content is untouched: it lives in the Store, not here. Linked devices
    /// must link this one again, and an account must sign in again, because
    /// both are bound to the Device ID that was set aside.
    pub fn reset(paths: &Paths, now_millis: i64) -> Result<PathBuf, DeviceError> {
        Self::reset_with(paths, || DeviceSecrets::for_paths(paths), now_millis)
    }

    fn reset_with(
        paths: &Paths,
        secrets: impl Fn() -> DeviceSecrets,
        now_millis: i64,
    ) -> Result<PathBuf, DeviceError> {
        if Self::open_with(paths, secrets(), now_millis).is_ok() {
            return Err(DeviceError::new(
                "identity_intact",
                "this device's identity is intact and is not reset",
            ));
        }
        let name = Device::open(&paths.device_file(), now_millis)
            .map(|device| device.name)
            .map_err(io_error)?;
        let aside = paths
            .agent_dir()
            .join(format!("identity-set-aside-{now_millis}"));
        std::fs::create_dir_all(&aside).map_err(io_error)?;
        set_aside(&paths.device_file(), &aside)?;
        let db = paths.identity_db();
        for suffix in ["-wal", "-shm", ""] {
            let mut file = db.clone().into_os_string();
            file.push(suffix);
            set_aside(Path::new(&file), &aside)?;
        }

        let secrets = secrets();
        let held = load_signing(&secrets)
            .and_then(|signing| load_endpoint(&secrets).map(|endpoint| (signing, endpoint)));
        let (signing, endpoint) = match held {
            Ok(keys) => keys,
            Err(_) => {
                secrets.retire_into(&aside, paths)?;
                let signing = DeviceSigningKey::generate();
                let endpoint = ReplicationEndpointSecret::generate();
                secrets.store_new(
                    SecretName::DeviceSigning,
                    &SecretBytes(signing.secret_bytes()),
                )?;
                secrets.store_new(
                    SecretName::ReplicationEndpoint,
                    &SecretBytes(endpoint.secret_bytes()),
                )?;
                (signing, endpoint)
            }
        };
        let descriptor = DeviceDescriptorV1::new(
            DeviceId::new(),
            name.clone(),
            &signing,
            endpoint.endpoint_id(),
            now_millis,
        );
        let mut repository = SqliteTrustRepository::open(&db)?;
        repository.initialize(&descriptor)?;
        let device = Device {
            id: descriptor.device_id.to_storage(),
            name,
            created_at_millis: now_millis,
        };
        let body = serde_json::to_vec_pretty(&device)
            .map_err(|error| DeviceError::new("identity_io", error.to_string()))?;
        std::fs::write(paths.device_file(), body).map_err(io_error)?;
        Ok(aside)
    }
}

/// Move `path` into `aside` under its own name, if it exists.
fn set_aside(path: &Path, aside: &Path) -> Result<(), DeviceError> {
    let Some(name) = path.file_name() else {
        return Ok(());
    };
    match std::fs::rename(path, aside.join(name)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn load_signing(store: &impl SecretStore) -> Result<DeviceSigningKey, DeviceError> {
    store
        .load(SecretName::DeviceSigning)?
        .map(|bytes| DeviceSigningKey::from_secret_bytes(bytes.0))
        .ok_or_else(|| DeviceError::new("identity_unavailable", "device signing key is missing"))
}

fn load_endpoint(store: &impl SecretStore) -> Result<ReplicationEndpointSecret, DeviceError> {
    store
        .load(SecretName::ReplicationEndpoint)?
        .map(|bytes| ReplicationEndpointSecret::from_secret_bytes(bytes.0))
        .ok_or_else(|| {
            DeviceError::new(
                "identity_unavailable",
                "replication endpoint key is missing",
            )
        })
}

fn validate_keys(
    descriptor: &DeviceDescriptorV1,
    signing: &DeviceSigningKey,
    endpoint: &ReplicationEndpointSecret,
) -> Result<(), DeviceError> {
    if descriptor.signing_public_key != signing.public_key()
        || descriptor.endpoint_id != endpoint.endpoint_id()
        || descriptor.verify().is_err()
    {
        return Err(DeviceError::new(
            "identity_unavailable",
            "protected keys do not match the authoritative public identity",
        ));
    }
    Ok(())
}

/// Key material in a file behind whatever protection this platform has: DPAPI
/// on Windows, an owner-only header-checked file on an **internal** Mac build
/// (a signed one keeps the secrets in the Keychain instead — [`keychain`]) and
/// on Linux, and a refusal everywhere else. On a signed Mac this is also the format the
/// Keychain migration reads once.
pub struct ProtectedSecretStore {
    directory: PathBuf,
}

impl ProtectedSecretStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
    fn ensure_directory(&self) -> Result<(), DeviceError> {
        let mut directories = std::fs::DirBuilder::new();
        directories.recursive(true);
        #[cfg(unix)]
        {
            // 0700: the directory is the guarantee, the file's own mode is
            // decoration — the same reasoning as koru-ipc's socket parent.
            use std::os::unix::fs::DirBuilderExt;
            directories.mode(0o700);
        }
        directories.create(&self.directory).map_err(io_error)
    }
    fn path(&self, name: SecretName) -> PathBuf {
        // The extension says what the bytes are. A file of macOS bytes named
        // `.dpapi` would lie to whoever finds it.
        let extension = if cfg!(windows) { "dpapi" } else { "secret" };
        self.directory.join(match name {
            SecretName::DeviceSigning => format!("device-signing.{extension}"),
            SecretName::ReplicationEndpoint => format!("replication-endpoint.{extension}"),
        })
    }
}

impl SecretStore for ProtectedSecretStore {
    fn load(&self, name: SecretName) -> Result<Option<SecretBytes>, DeviceError> {
        let path = self.path(name);
        let Some(ciphertext) = std::fs::read(&path)
            .map(Some)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(error)
                }
            })
            .map_err(io_error)?
        else {
            return Ok(None);
        };
        let plaintext = protect::unprotect(&ciphertext)?;
        let bytes: [u8; 32] = plaintext.try_into().map_err(|_| {
            DeviceError::new("secret_corrupt", "protected secret has the wrong length")
        })?;
        Ok(Some(SecretBytes(bytes)))
    }

    fn store_new(&self, name: SecretName, value: &SecretBytes) -> Result<(), DeviceError> {
        self.ensure_directory()?;
        let destination = self.path(name);
        if destination.exists() {
            return Err(DeviceError::new(
                "secret_exists",
                "refusing to replace protected key material",
            ));
        }
        let ciphertext = protect::protect(&value.0)?;
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            destination
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("secret"),
            uuid::Uuid::new_v4()
        ));
        let result = (|| -> Result<(), DeviceError> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(io_error)?;
            file.write_all(&ciphertext).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            std::fs::rename(&temporary, &destination).map_err(io_error)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(windows)]
mod protect {
    use std::ptr;

    use koru_device::DeviceError;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, DeviceError> {
        crypt(plaintext, true)
    }
    pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, DeviceError> {
        crypt(ciphertext, false)
    }

    fn crypt(input: &[u8], encrypt: bool) -> Result<Vec<u8>, DeviceError> {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(input.len())
                .map_err(|_| DeviceError::new("secret_store", "secret is too large"))?,
            pbData: input.as_ptr().cast_mut(),
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: ptr::null_mut(),
        };
        let ok = unsafe {
            if encrypt {
                CryptProtectData(
                    &mut input,
                    ptr::null(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            } else {
                CryptUnprotectData(
                    &mut input,
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut output,
                )
            }
        };
        if ok == 0 {
            return Err(DeviceError::new(
                "secret_store",
                std::io::Error::last_os_error().to_string(),
            ));
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe {
            LocalFree(output.pbData.cast());
        }
        Ok(bytes)
    }
}

/// macOS: an owner-only file, deliberately not the Keychain — **and the
/// condition that made that deliberate has expired.**
///
/// Keychain ACLs key on the code signature, and a dev-parity build
/// (`./koru.sh run`) is re-signed ad hoc on every rebuild, so to the keychain
/// each build is a different app: prompts or silent denials on every launch.
/// [ADR-133] therefore chose a file *"until a signed bundle exists"*. Since
/// M17 (2026-09-24) that sentence is honoured: a signed build keeps the two
/// secrets in the Keychain ([`super::keychain`]) and reads this format only
/// once, to copy it in; this arm is what an **internal** build still uses —
/// a 0700 directory and a 0600 file, the protection an SSH key accepts, with
/// the bytes plaintext at rest.
///
/// [ADR-133]: ../../../docs/decisions/ADR-126-150.md
///
/// The header names the format, so corrupt bytes fail closed
/// (`corrupt_ciphertext_is_never_replaced` keeps its meaning) and a future
/// Keychain-backed format is distinguishable from this one instead of being
/// misread as it.
///
/// **Linux uses this arm too, for now.** There is no signed bundle there and no
/// adapter for the desktop's Secret Service yet, and without a protected
/// identity the Agent starts no device network and so no gateway — Koru does
/// not open at all. An owner-only file is the macOS internal build's bargain,
/// taken knowingly; the header is what lets a Secret Service format replace it
/// later by reading this one once, as the Keychain did.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod protect {
    use koru_device::DeviceError;

    const MAGIC: &[u8] = b"koru-secret-v1\0";

    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, DeviceError> {
        let mut bytes = Vec::with_capacity(MAGIC.len() + plaintext.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(plaintext);
        Ok(bytes)
    }
    pub fn unprotect(protected: &[u8]) -> Result<Vec<u8>, DeviceError> {
        protected
            .strip_prefix(MAGIC)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| {
                DeviceError::new("secret_store", "protected secret has an unknown format")
            })
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod protect {
    use koru_device::DeviceError;
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, DeviceError> {
        Err(DeviceError::new(
            "secure_storage_unavailable",
            "this platform has no production secure-storage adapter",
        ))
    }
    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, DeviceError> {
        Err(DeviceError::new(
            "secure_storage_unavailable",
            "this platform has no production secure-storage adapter",
        ))
    }
}

fn io_error(error: std::io::Error) -> DeviceError {
    DeviceError::new("identity_io", error.to_string())
}

#[cfg(test)]
mod tests;
