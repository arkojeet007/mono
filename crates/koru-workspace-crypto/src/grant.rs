// SPDX-License-Identifier: MPL-2.0

//! **Layer 1 — the key grant, and nothing else.**
//!
//! A [`WekGrantV1`] carries one [`WorkspaceEncryptionKey`] to one recipient. It
//! is 272 bytes for an X25519 recipient and 305 for a P-256 one. **It is the
//! only place in Koru where public-key encryption appears**, and the only thing
//! it ever encrypts is 32 bytes of symmetric key.
//!
//! # Why that boundary is the whole point (D86)
//!
//! Changing the HPKE suite means reissuing grants for the same WEK. It does not
//! mean re-encrypting a blob, a segment, a snapshot or a checkpoint, because
//! none of those record which suite delivered their key — they record the *WEK
//! version*, and a WEK version is suite-agnostic. A design in which one HPKE
//! suite determined all customer-data encryption would make a suite change a
//! terabyte-scale migration, and would be rejected at the review gate.
//!
//! # The construction (D87)
//!
//! HPKE **Base mode** (RFC 9180 §5.1.1) over
//! DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + ChaCha20Poly1305, with the
//! complete envelope authenticated by the author's **existing Ed25519 identity
//! key** from `koru-device`.
//!
//! Base mode plus a detached signature rather than HPKE Auth mode, deliberately:
//! Auth mode would require the sender to hold a *KEM* private key, which would
//! mean inventing a second X25519 sender identity beside the Ed25519 one that
//! already exists and already means "this device". The split stays clean —
//! **X25519 is confidentiality and key delivery; Ed25519 is identity and
//! authorship** — and the signature covers the whole envelope rather than only
//! the KEM half. RFC 9180 §4.1 also notes that DHKEM's Auth mode is vulnerable
//! to key-compromise impersonation, which a signature is not.
//!
//! # Context binding
//!
//! The complete 128-byte header is the HPKE `info`, prefixed with a domain
//! label. `aad` is empty — RFC 9180 §8.1 says information applying to the whole
//! context belongs in `info`, and that a single-shot API should not use both.
//! A grant is single-shot by construction.
//!
//! So a ciphertext valid for workspace A, WEK version 3, device D, key id K is
//! valid for **nothing else**: change any of `format_version`, `suite_id`,
//! `purpose`, `recipient_type`, `author_type`, `workspace_id`, `wek_version`,
//! `recipient_id`, `recipient_key_id`, `recipient_key_version`,
//! `recovery_key_id`, `author_id` or `author_public_key` and the `info`
//! changes, the key schedule changes, and the AEAD tag fails.
//!
//! # Recipients need not share a KEM (D88)
//!
//! The suite is a **per-grant** field. A workspace's device grants can be
//! X25519 while its recovery grant is P-256, because the regional recovery
//! private key must stay inside KMS/HSM protection and a provider that cannot
//! do X25519 there can do P-256. Nothing in the workspace format assumes one
//! KEM.
//!
//! # HPKE provides no replay protection (RFC 9180 §9.7.3)
//!
//! A grant is a durable capability, not a message in a stream. Replaying one
//! delivers the same key to the same recipient it was already delivered to. The
//! things that *are* replay-sensitive — a revoked device, an old trust epoch, a
//! superseded WEK version — are refused by Koru's epochs, versions and state
//! machine (D22/D51/Plan 3.3), never by the cipher.

use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::{DhP256HkdfSha256, X25519HkdfSha256};
use hpke::{Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable};
use koru_device::{DevicePublicKey, DeviceSigningKey, SignatureBytes};
use koru_store_core::ids::WorkspaceId;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::codec::{Reader, Writer, constant_time_eq};
use crate::error::{CryptoError, Result};
use crate::keys::{KeyId, WekVersion, WorkspaceEncryptionKey};
use crate::regional::RecoveryKeyId;
use crate::sealed::KEY_LEN;
use crate::suites::{
    AuthorType, FORMAT_VERSION_V1, GrantSuite, Purpose, RecipientType, WRITER_SUITES,
};

/// Domain label for the HPKE `info`. Follows `koru-device`'s existing
/// `koru:<name>:v1\0` convention so that no Koru signing or derivation domain
/// can be a prefix of another.
const GRANT_INFO_DOMAIN: &[u8] = b"koru:wek-grant:v1\0";
/// Domain label for the Ed25519 signature. Distinct from the `info` label: two
/// uses, two domains.
const GRANT_SIGNATURE_DOMAIN: &[u8] = b"koru:wek-grant-signature:v1\0";

/// The fixed width of [`GrantHeader`] on the wire.
pub const GRANT_HEADER_LEN: usize = 2  // format_version
    + 2  // suite_id
    + 2  // purpose
    + 1  // recipient_type
    + 1  // author_type
    + 16 // workspace_id
    + 4  // wek_version
    + 16 // recipient_id
    + 16 // recipient_key_id
    + 4  // recipient_key_version
    + 16 // recovery_key_id
    + 16 // author_id
    + 32; // author_public_key

/// The HPKE ciphertext: a 32-byte WEK plus a 16-byte tag.
pub const GRANT_CIPHERTEXT_LEN: usize = KEY_LEN + 16;
/// Ed25519.
pub const GRANT_SIGNATURE_LEN: usize = 64;

// ------------------------------------------------------------- recipient keys

/// A recipient's HPKE public key. **The suite travels with the key**, which is
/// what makes "recipients need not share a KEM" a property of the data rather
/// than of a convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecipientPublicKey {
    /// `Npk` = 32. RFC 9180 §7.1.1: X25519 serialization is the identity.
    X25519([u8; 32]),
    /// `Npk` = 65, the uncompressed SEC1 point. RFC 9180 §7.1.4 requires
    /// partial public-key validation, which `p256` performs on deserialization.
    P256(Box<[u8; 65]>),
}

impl RecipientPublicKey {
    pub fn suite(&self) -> GrantSuite {
        match self {
            Self::X25519(_) => GrantSuite::X25519HkdfSha256ChaCha20Poly1305,
            Self::P256(_) => GrantSuite::P256HkdfSha256ChaCha20Poly1305,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::X25519(bytes) => bytes.as_slice(),
            Self::P256(bytes) => bytes.as_slice(),
        }
    }

    pub fn from_bytes(suite: GrantSuite, bytes: &[u8]) -> Result<Self> {
        match suite {
            GrantSuite::X25519HkdfSha256ChaCha20Poly1305 => Ok(Self::X25519(
                bytes.try_into().map_err(|_| CryptoError::InvalidKey)?,
            )),
            GrantSuite::P256HkdfSha256ChaCha20Poly1305 => {
                let array: [u8; 65] = bytes.try_into().map_err(|_| CryptoError::InvalidKey)?;
                Ok(Self::P256(Box::new(array)))
            }
        }
    }
}

/// A recipient's HPKE private key. `Nsk` = 32 for both DHKEMs here.
///
/// **The regional recovery private key is not meant to live in this type.**
/// D88 requires it to stay inside KMS/HSM protection and be callable only
/// through the recovery service. This type exists so the format can be
/// exercised and its golden vectors verified; Plan 2.2 owns the custody seam,
/// and the format document records what a KMS has to be able to do for the
/// server side of a recovery grant to work at all.
pub struct RecipientSecretKey {
    suite: GrantSuite,
    bytes: Zeroizing<[u8; 32]>,
}

impl RecipientSecretKey {
    pub fn from_bytes(suite: GrantSuite, bytes: [u8; 32]) -> Self {
        Self {
            suite,
            bytes: Zeroizing::new(bytes),
        }
    }

    pub fn suite(&self) -> GrantSuite {
        self.suite
    }

    /// Derives the matching public key, so a test or a vector generator can
    /// state one secret and get the pair.
    pub fn public_key(&self) -> Result<RecipientPublicKey> {
        match self.suite {
            GrantSuite::X25519HkdfSha256ChaCha20Poly1305 => {
                let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*self.bytes)
                    .map_err(|_| CryptoError::InvalidKey)?;
                let pk = <X25519HkdfSha256 as KemTrait>::sk_to_pk(&sk);
                RecipientPublicKey::from_bytes(self.suite, pk.to_bytes().as_slice())
            }
            GrantSuite::P256HkdfSha256ChaCha20Poly1305 => {
                let sk = <DhP256HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*self.bytes)
                    .map_err(|_| CryptoError::InvalidKey)?;
                let pk = <DhP256HkdfSha256 as KemTrait>::sk_to_pk(&sk);
                RecipientPublicKey::from_bytes(self.suite, pk.to_bytes().as_slice())
            }
        }
    }
}

impl std::fmt::Debug for RecipientSecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RecipientSecretKey({:?}, [REDACTED])", self.suite)
    }
}

// -------------------------------------------------------------------- header

/// Everything a grant says about itself, in the clear, and all of it bound into
/// the HPKE `info`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantHeader {
    pub format_version: u16,
    pub suite: GrantSuite,
    pub recipient_type: RecipientType,
    pub author_type: AuthorType,
    pub workspace_id: WorkspaceId,
    pub wek_version: WekVersion,
    /// The device, replica or recovery authority this grant is for.
    pub recipient_id: Uuid,
    pub recipient_key_id: KeyId,
    pub recipient_key_version: u32,
    /// **Meaningful only on a recovery grant** (D88). [`KeyId::NONE`] on every
    /// other kind, always — it is not a flag about custody, and D89's rule is
    /// enforced by the absence of the grant rather than by this field.
    pub recovery_key_id: RecoveryKeyId,
    pub author_id: Uuid,
    pub author_public_key: DevicePublicKey,
}

impl GrantHeader {
    /// The `purpose` every grant carries. It is a constant rather than a field
    /// on the struct because there is exactly one thing HPKE is used for here,
    /// and a settable purpose would invite a second.
    pub const PURPOSE: Purpose = Purpose::WekGrant;

    fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::with_capacity(GRANT_HEADER_LEN);
        writer
            .u16(self.format_version)
            .u16(self.suite.id())
            .u16(Self::PURPOSE.id())
            .u8(self.recipient_type.tag())
            .u8(self.author_type.tag())
            .uuid(self.workspace_id.as_uuid())
            .u32(self.wek_version.0)
            .uuid(self.recipient_id)
            .bytes(self.recipient_key_id.as_bytes())
            .u32(self.recipient_key_version)
            .bytes(self.recovery_key_id.key_id().as_bytes())
            .uuid(self.author_id)
            .bytes(self.author_public_key.as_bytes());
        let bytes = writer.finish();
        debug_assert_eq!(bytes.len(), GRANT_HEADER_LEN);
        bytes
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self> {
        let format_version = reader.u16()?;
        if format_version != FORMAT_VERSION_V1 {
            return Err(CryptoError::UnsupportedFormatVersion);
        }
        let suite = GrantSuite::from_id(reader.u16()?)?;
        if Purpose::from_id(reader.u16()?)? != Self::PURPOSE {
            return Err(CryptoError::PurposeMismatch);
        }
        let recipient_type = RecipientType::from_tag(reader.u8()?)?;
        let author_type = AuthorType::from_tag(reader.u8()?)?;
        let workspace_id = WorkspaceId::from_uuid(reader.uuid()?);
        let wek_version = WekVersion(reader.u32()?);
        let recipient_id = reader.uuid()?;
        let recipient_key_id = KeyId::from_bytes(reader.array::<16>()?);
        let recipient_key_version = reader.u32()?;
        let recovery_key_id = RecoveryKeyId::new(KeyId::from_bytes(reader.array::<16>()?));
        let author_id = reader.uuid()?;
        let author_public_key = DevicePublicKey::from_bytes(reader.array::<32>()?);

        let header = Self {
            format_version,
            suite,
            recipient_type,
            author_type,
            workspace_id,
            wek_version,
            recipient_id,
            recipient_key_id,
            recipient_key_version,
            recovery_key_id,
            author_id,
            author_public_key,
        };
        header.check_structural_invariants()?;
        Ok(header)
    }

    /// The invariants that hold for **every** grant, whoever wrote it.
    ///
    /// `recovery_key_id` is `NONE` unless this is a recovery grant, and is not
    /// `NONE` when it is. A reader enforces both, which is what stops a
    /// mis-issued grant from being read as a different kind of grant.
    fn check_structural_invariants(&self) -> Result<()> {
        let is_recovery = self.recipient_type == RecipientType::RegionalRecovery;
        let names_a_recovery_key = !self.recovery_key_id.key_id().is_none();
        if is_recovery != names_a_recovery_key {
            return Err(CryptoError::ContextMismatch);
        }
        if self.recipient_key_id.is_none() {
            return Err(CryptoError::ContextMismatch);
        }
        Ok(())
    }

    fn info(&self) -> Vec<u8> {
        let mut info = Vec::with_capacity(GRANT_INFO_DOMAIN.len() + GRANT_HEADER_LEN);
        info.extend_from_slice(GRANT_INFO_DOMAIN);
        info.extend_from_slice(&self.encode());
        info
    }
}

/// What a reader believes it is opening. **`open` cannot be called without
/// one**, which is the API shape of "a valid ciphertext for workspace A must
/// not be transplantable into another semantic context": the reader states its
/// context, and a mismatch is refused before any key is touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantExpectation {
    pub workspace_id: WorkspaceId,
    pub wek_version: WekVersion,
    pub recipient_type: RecipientType,
    pub recipient_id: Uuid,
    pub recipient_key_id: KeyId,
    pub recipient_key_version: u32,
}

impl GrantExpectation {
    fn check(&self, header: &GrantHeader) -> Result<()> {
        let matches = header.workspace_id == self.workspace_id
            && header.wek_version == self.wek_version
            && header.recipient_type == self.recipient_type
            && header.recipient_id == self.recipient_id
            && constant_time_eq(
                header.recipient_key_id.as_bytes(),
                self.recipient_key_id.as_bytes(),
            )
            && header.recipient_key_version == self.recipient_key_version;
        if matches {
            Ok(())
        } else {
            Err(CryptoError::ContextMismatch)
        }
    }
}

// ------------------------------------------------------------------ envelope

/// One sealed workspace encryption key, for one recipient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WekGrantV1 {
    header: GrantHeader,
    encapsulation: Vec<u8>,
    ciphertext: [u8; GRANT_CIPHERTEXT_LEN],
    signature: SignatureBytes,
}

impl WekGrantV1 {
    /// Seals `wek` to `recipient`, and signs the complete envelope.
    ///
    /// The suite is the recipient key's own — it is not chosen here and is not
    /// negotiated with anybody. [`GrantSuite::writable_for`] is the writer
    /// policy, and it refuses a deprecated or wrong-shaped suite for a new
    /// write while readers go on understanding it.
    pub fn seal(
        header_fields: GrantFields,
        recipient: &RecipientPublicKey,
        wek: &WorkspaceEncryptionKey,
        author: &DeviceSigningKey,
        rng: &mut impl rand_core::CryptoRng,
    ) -> Result<Self> {
        let suite = recipient.suite();
        if !suite.writable_for(header_fields.recipient_type) {
            return Err(CryptoError::DeprecatedSuiteForWrite);
        }
        let header = GrantHeader {
            format_version: FORMAT_VERSION_V1,
            suite,
            recipient_type: header_fields.recipient_type,
            author_type: header_fields.author_type,
            workspace_id: header_fields.workspace_id,
            wek_version: header_fields.wek_version,
            recipient_id: header_fields.recipient_id,
            recipient_key_id: header_fields.recipient_key_id,
            recipient_key_version: header_fields.recipient_key_version,
            recovery_key_id: header_fields.recovery_key_id,
            author_id: header_fields.author_id,
            author_public_key: author.public_key(),
        };
        header.check_structural_invariants()?;
        let info = header.info();

        let (encapsulation, ciphertext) = match recipient {
            RecipientPublicKey::X25519(bytes) => {
                let pk = <X25519HkdfSha256 as KemTrait>::PublicKey::from_bytes(bytes)
                    .map_err(|_| CryptoError::InvalidKey)?;
                let (enc, ct) = hpke::single_shot_seal_with_rng::<
                    ChaCha20Poly1305,
                    HkdfSha256,
                    X25519HkdfSha256,
                >(
                    &OpModeS::Base, &pk, &info, wek.as_bytes(), b"", rng
                )
                .map_err(|_| CryptoError::Aead)?;
                (enc.to_bytes().to_vec(), ct)
            }
            RecipientPublicKey::P256(bytes) => {
                let pk = <DhP256HkdfSha256 as KemTrait>::PublicKey::from_bytes(bytes.as_slice())
                    .map_err(|_| CryptoError::InvalidKey)?;
                let (enc, ct) = hpke::single_shot_seal_with_rng::<
                    ChaCha20Poly1305,
                    HkdfSha256,
                    DhP256HkdfSha256,
                >(
                    &OpModeS::Base, &pk, &info, wek.as_bytes(), b"", rng
                )
                .map_err(|_| CryptoError::Aead)?;
                (enc.to_bytes().to_vec(), ct)
            }
        };

        let ciphertext: [u8; GRANT_CIPHERTEXT_LEN] =
            ciphertext.try_into().map_err(|_| CryptoError::Malformed)?;
        let signature = author.sign_message(&signing_message(
            &header.encode(),
            &encapsulation,
            &ciphertext,
        ));

        Ok(Self {
            header,
            encapsulation,
            ciphertext,
            signature,
        })
    }

    pub fn header(&self) -> &GrantHeader {
        &self.header
    }

    /// The envelope's bytes: header, encapsulation, ciphertext, signature.
    pub fn encode(&self) -> Vec<u8> {
        let mut writer = Writer::with_capacity(
            GRANT_HEADER_LEN
                + self.encapsulation.len()
                + GRANT_CIPHERTEXT_LEN
                + GRANT_SIGNATURE_LEN,
        );
        writer
            .bytes(&self.header.encode())
            .bytes(&self.encapsulation)
            .bytes(&self.ciphertext)
            .bytes(self.signature.as_bytes());
        writer.finish()
    }

    /// Parses an envelope. **Does not verify the signature and does not
    /// decrypt** — those are [`WekGrantV1::verify_signature`] and
    /// [`WekGrantV1::open`], in that order, because a caller that has not said
    /// whose signature it will accept has nothing to check against.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut reader = Reader::new(bytes);
        let header = GrantHeader::decode(&mut reader)?;
        let encapsulation = reader.take(header.suite.encapsulation_len())?.to_vec();
        let ciphertext = reader.array::<GRANT_CIPHERTEXT_LEN>()?;
        let signature = SignatureBytes::from_bytes(reader.array::<GRANT_SIGNATURE_LEN>()?);
        reader.finish()?;
        Ok(Self {
            header,
            encapsulation,
            ciphertext,
            signature,
        })
    }

    /// Verifies the Ed25519 signature over the complete envelope against
    /// `author`.
    ///
    /// **The caller supplies the key.** Verifying against the key carried
    /// inside the envelope would prove only that the envelope is
    /// self-consistent, which is worth nothing: the author must be checked
    /// against a trust view (`koru_device::TrustView`) that this crate has no
    /// business holding an opinion about.
    pub fn verify_signature(&self, author: &DevicePublicKey) -> Result<()> {
        if !constant_time_eq(author.as_bytes(), self.header.author_public_key.as_bytes()) {
            return Err(CryptoError::Signature);
        }
        let message = signing_message(&self.header.encode(), &self.encapsulation, &self.ciphertext);
        if author.verify_message(&message, self.signature) {
            Ok(())
        } else {
            Err(CryptoError::Signature)
        }
    }

    /// Opens the grant, having first checked that it is the grant the caller
    /// meant to open.
    pub fn open(
        &self,
        expectation: &GrantExpectation,
        secret: &RecipientSecretKey,
    ) -> Result<WorkspaceEncryptionKey> {
        expectation.check(&self.header)?;
        if secret.suite() != self.header.suite {
            return Err(CryptoError::UnsupportedSuite);
        }
        let info = self.header.info();

        let plaintext = match self.header.suite {
            GrantSuite::X25519HkdfSha256ChaCha20Poly1305 => {
                let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*secret.bytes)
                    .map_err(|_| CryptoError::InvalidKey)?;
                let enc =
                    <X25519HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&self.encapsulation)
                        .map_err(|_| CryptoError::InvalidKey)?;
                hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
                    &OpModeR::Base,
                    &sk,
                    &enc,
                    &info,
                    &self.ciphertext,
                    b"",
                )
                .map_err(|_| CryptoError::Aead)?
            }
            GrantSuite::P256HkdfSha256ChaCha20Poly1305 => {
                let sk = <DhP256HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&*secret.bytes)
                    .map_err(|_| CryptoError::InvalidKey)?;
                let enc =
                    <DhP256HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&self.encapsulation)
                        .map_err(|_| CryptoError::InvalidKey)?;
                hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>(
                    &OpModeR::Base,
                    &sk,
                    &enc,
                    &info,
                    &self.ciphertext,
                    b"",
                )
                .map_err(|_| CryptoError::Aead)?
            }
        };

        let plaintext = Zeroizing::new(plaintext);
        let bytes: [u8; KEY_LEN] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed)?;
        Ok(WorkspaceEncryptionKey::from_bytes(bytes))
    }
}

fn signing_message(header: &[u8], encapsulation: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        GRANT_SIGNATURE_DOMAIN.len() + header.len() + encapsulation.len() + ciphertext.len(),
    );
    message.extend_from_slice(GRANT_SIGNATURE_DOMAIN);
    message.extend_from_slice(header);
    message.extend_from_slice(encapsulation);
    message.extend_from_slice(ciphertext);
    message
}

/// The fields a caller supplies when sealing. Separate from [`GrantHeader`]
/// because `format_version`, `suite` and `author_public_key` are decided by the
/// writer policy and the key material, not by the caller.
#[derive(Clone, Debug)]
pub struct GrantFields {
    pub recipient_type: RecipientType,
    pub author_type: AuthorType,
    pub workspace_id: WorkspaceId,
    pub wek_version: WekVersion,
    pub recipient_id: Uuid,
    pub recipient_key_id: KeyId,
    pub recipient_key_version: u32,
    pub recovery_key_id: RecoveryKeyId,
    pub author_id: Uuid,
}

// --------------------------------------------------------- custody and D89

/// A recipient of the regional recovery grant.
///
/// **There is no public constructor.** The only way to obtain one is
/// [`WorkspaceCustody::koru_managed`], and the only way to use one is
/// [`GrantSet::issue`]. That is what makes "a customer-managed workspace has no
/// Koru recovery grant" a property of the code rather than a promise in a
/// document.
#[derive(Clone, Debug)]
pub struct RecoveryRecipient {
    recovery_key_id: RecoveryKeyId,
    recipient_id: Uuid,
    recipient_key_id: KeyId,
    recipient_key_version: u32,
    public_key: RecipientPublicKey,
}

/// How a workspace's WEK is governed.
#[derive(Clone, Debug)]
pub enum WorkspaceCustody {
    /// `KORU_MANAGED` (D12). The regional recovery authority holds a grant, so
    /// a person who loses every device can recover (D15).
    KoruManaged(Box<RecoveryRecipient>),
    /// `CUSTOMER_MANAGED` (D12). **This variant carries no field from which a
    /// Koru recovery grant could be built**, so [`GrantSet::issue`] cannot
    /// produce one, and no flag anywhere promises that Koru will merely decline
    /// to use one (D89).
    CustomerManaged,
}

impl WorkspaceCustody {
    pub fn koru_managed(
        recovery_key_id: RecoveryKeyId,
        recipient_id: Uuid,
        recipient_key_id: KeyId,
        recipient_key_version: u32,
        public_key: RecipientPublicKey,
    ) -> Result<Self> {
        if recovery_key_id.key_id().is_none() || recipient_key_id.is_none() {
            return Err(CryptoError::ContextMismatch);
        }
        Ok(Self::KoruManaged(Box::new(RecoveryRecipient {
            recovery_key_id,
            recipient_id,
            recipient_key_id,
            recipient_key_version,
            public_key,
        })))
    }
}

/// One recipient that is not the recovery authority.
#[derive(Clone, Debug)]
pub struct Recipient {
    pub recipient_type: RecipientType,
    pub recipient_id: Uuid,
    pub recipient_key_id: KeyId,
    pub recipient_key_version: u32,
    pub public_key: RecipientPublicKey,
}

/// Every grant that exists for one WEK version of one workspace.
///
/// The set, not the individual grant, is where D89 is checkable: a
/// customer-managed workspace's set contains **no** grant whose
/// `recipient_type` is [`RecipientType::RegionalRecovery`], and a golden vector
/// pins that.
#[derive(Clone, Debug)]
pub struct GrantSet {
    grants: Vec<WekGrantV1>,
}

/// What every grant in one issuance shares: which workspace, which WEK
/// version, and who is signing.
///
/// A struct rather than four more parameters, because four positional values of
/// which two are UUIDs is a call site nobody can read and a transposition
/// nobody would catch.
#[derive(Clone, Debug)]
pub struct GrantIssuance {
    pub workspace_id: WorkspaceId,
    pub wek_version: WekVersion,
    pub author_type: AuthorType,
    pub author_id: Uuid,
}

impl GrantSet {
    /// Issues the complete grant set for `wek` at `issuance.wek_version`.
    ///
    /// The recovery grant is produced **only** in the
    /// [`WorkspaceCustody::KoruManaged`] branch, because that is the only
    /// branch that has a recipient to produce it for.
    pub fn issue(
        custody: &WorkspaceCustody,
        issuance: &GrantIssuance,
        wek: &WorkspaceEncryptionKey,
        recipients: &[Recipient],
        author: &DeviceSigningKey,
        rng: &mut impl rand_core::CryptoRng,
    ) -> Result<Self> {
        let mut grants = Vec::with_capacity(recipients.len() + 1);

        for recipient in recipients {
            if recipient.recipient_type == RecipientType::RegionalRecovery {
                // A recovery grant is not something a caller may pass in as an
                // ordinary recipient; it comes from custody or not at all.
                return Err(CryptoError::Forbidden);
            }
            grants.push(WekGrantV1::seal(
                GrantFields {
                    recipient_type: recipient.recipient_type,
                    author_type: issuance.author_type,
                    workspace_id: issuance.workspace_id,
                    wek_version: issuance.wek_version,
                    recipient_id: recipient.recipient_id,
                    recipient_key_id: recipient.recipient_key_id,
                    recipient_key_version: recipient.recipient_key_version,
                    recovery_key_id: RecoveryKeyId::new(KeyId::NONE),
                    author_id: issuance.author_id,
                },
                &recipient.public_key,
                wek,
                author,
                rng,
            )?);
        }

        if let WorkspaceCustody::KoruManaged(recovery) = custody {
            grants.push(WekGrantV1::seal(
                GrantFields {
                    recipient_type: RecipientType::RegionalRecovery,
                    author_type: issuance.author_type,
                    workspace_id: issuance.workspace_id,
                    wek_version: issuance.wek_version,
                    recipient_id: recovery.recipient_id,
                    recipient_key_id: recovery.recipient_key_id,
                    recipient_key_version: recovery.recipient_key_version,
                    recovery_key_id: recovery.recovery_key_id,
                    author_id: issuance.author_id,
                },
                &recovery.public_key,
                wek,
                author,
                rng,
            )?);
        }

        Ok(Self { grants })
    }

    pub fn grants(&self) -> &[WekGrantV1] {
        &self.grants
    }

    /// The Koru recovery grant, if one exists at all.
    pub fn koru_recovery_grant(&self) -> Option<&WekGrantV1> {
        self.grants
            .iter()
            .find(|g| g.header.recipient_type == RecipientType::RegionalRecovery)
    }

    pub fn has_koru_recovery_grant(&self) -> bool {
        self.koru_recovery_grant().is_some()
    }
}

/// The suite a new device grant is written with. Exposed so that a caller can
/// state the policy it is following rather than discovering it.
pub const DEVICE_GRANT_SUITE: GrantSuite = WRITER_SUITES.grant_device;

#[cfg(test)]
mod tests;
