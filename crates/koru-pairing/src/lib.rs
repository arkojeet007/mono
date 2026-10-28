// SPDX-License-Identifier: MPL-2.0

//! Short-lived invitation and transcript logic for bilateral device pairing.

use std::fmt;
use std::time::{Duration, Instant};

use base64::Engine as _;
use koru_account_session::{SignedDeviceAttestationV1, VerifiedDeviceAttestationV1};
use koru_device::{
    AllDataGrantV1, DeviceDescriptorV1, DeviceId, DevicePublicKey, DeviceSigningKey,
    ProfileGrantV1, RecordId, RelationshipId, ReplicationEndpointId, SignatureBytes,
    TrustRelationshipV1,
};
use koru_store_core::ProfileId;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

pub const PAIRING_VERSION: u32 = 1;
pub const ENROLLMENT_VERSION: u32 = 1;
pub const MAX_ENROLLMENT_RECEIPT_BYTES: usize = 48 * 1024;
pub const INVITATION_LIFETIME: Duration = Duration::from_secs(120);
const QR_PREFIX: &str = "koru-pair:v1:";
const COMPACT_QR_PREFIX: &str = "koru-pair:v1c:";
pub const PAIRING_LINK_PREFIX: &str = "https://koru.do/link-device#";

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InvitationId(uuid::Uuid);

impl InvitationId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
    pub fn to_storage(self) -> String {
        self.0.to_string()
    }
}
impl Default for InvitationId {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PairingSecret([u8; 32]);

impl PairingSecret {
    pub fn generate() -> Self {
        let mut bytes = [0; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }
    pub fn matches(&self, candidate: &Self) -> bool {
        constant_time_eq(&self.0, &candidate.0)
    }
}

impl Clone for PairingSecret {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}
impl PartialEq for PairingSecret {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}
impl Eq for PairingSecret {}
impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret([REDACTED])")
    }
}
impl Drop for PairingSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl Serialize for PairingSecret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}
impl<'de> Deserialize<'de> for PairingSecret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bytes = Vec::<u8>::deserialize(d)?;
        Ok(Self(bytes.try_into().map_err(|_| {
            serde::de::Error::custom("pairing secret must be 32 bytes")
        })?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrohPairingTarget {
    pub endpoint_id: ReplicationEndpointId,
    pub relay_hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInvitationV1 {
    pub version: u32,
    pub invitation_id: InvitationId,
    pub inviter: DeviceDescriptorV1,
    pub pairing_target: IrohPairingTarget,
    pub secret: PairingSecret,
    pub expires_in_seconds: u16,
}

impl PairingInvitationV1 {
    pub fn new(inviter: DeviceDescriptorV1, relay_hint: Option<String>) -> Self {
        Self {
            version: PAIRING_VERSION,
            invitation_id: InvitationId::new(),
            pairing_target: IrohPairingTarget {
                endpoint_id: inviter.endpoint_id,
                relay_hint,
            },
            inviter,
            secret: PairingSecret::generate(),
            expires_in_seconds: INVITATION_LIFETIME.as_secs() as u16,
        }
    }

    pub fn encode_qr(&self) -> Result<String, PairingError> {
        let bytes = postcard::to_allocvec(self).map_err(|_| PairingError::Malformed)?;
        Ok(format!(
            "{COMPACT_QR_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        ))
    }

    /// A clickable HTTPS form for QR codes. The apps copy only the part after
    /// `#` to the clipboard; `decode_link` accepts either.
    /// The fragment keeps the one-use secret out of ordinary web-server logs.
    pub fn encode_link_url(&self) -> Result<String, PairingError> {
        Ok(format!("{PAIRING_LINK_PREFIX}{}", self.encode_qr()?))
    }

    /// Accept both the current app-link URL and the legacy raw QR payload.
    pub fn decode_link(value: &str) -> Result<Self, PairingError> {
        let trimmed = value.trim();
        let payload = trimmed.strip_prefix(PAIRING_LINK_PREFIX).unwrap_or(trimmed);
        Self::decode_qr(payload)
    }

    pub fn decode_qr(value: &str) -> Result<Self, PairingError> {
        let (encoded, compact) = if let Some(encoded) = value.strip_prefix(COMPACT_QR_PREFIX) {
            (encoded, true)
        } else if let Some(encoded) = value.strip_prefix(QR_PREFIX) {
            (encoded, false)
        } else {
            return Err(PairingError::Malformed);
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| PairingError::Malformed)?;
        let invitation: Self = if compact {
            postcard::from_bytes(&bytes).map_err(|_| PairingError::Malformed)?
        } else {
            serde_json::from_slice(&bytes).map_err(|_| PairingError::Malformed)?
        };
        if invitation.version != PAIRING_VERSION {
            return Err(PairingError::UnsupportedVersion);
        }
        verify_descriptor(&invitation.inviter)?;
        if invitation.pairing_target.endpoint_id != invitation.inviter.endpoint_id {
            return Err(PairingError::EndpointMismatch);
        }
        Ok(invitation)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScopeV1 {
    AllData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingTranscriptV1 {
    pub version: u32,
    pub invitation_id: InvitationId,
    pub relationship_id: RelationshipId,
    pub inviter: DeviceDescriptorV1,
    pub joiner: DeviceDescriptorV1,
    pub grant_scope: GrantScopeV1,
    pub inviter_challenge: [u8; 32],
    pub joiner_challenge: [u8; 32],
}

impl PairingTranscriptV1 {
    pub fn new(invitation: &PairingInvitationV1, joiner: DeviceDescriptorV1) -> Self {
        let mut inviter_challenge = [0; 32];
        let mut joiner_challenge = [0; 32];
        OsRng.fill_bytes(&mut inviter_challenge);
        OsRng.fill_bytes(&mut joiner_challenge);
        Self {
            version: PAIRING_VERSION,
            invitation_id: invitation.invitation_id,
            relationship_id: RelationshipId::new(),
            inviter: invitation.inviter.clone(),
            joiner,
            grant_scope: GrantScopeV1::AllData,
            inviter_challenge,
            joiner_challenge,
        }
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = b"koru:pairing-transcript:v1\0".to_vec();
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(self.invitation_id.0.as_bytes());
        bytes.extend_from_slice(self.relationship_id.as_uuid().as_bytes());
        append_descriptor(&mut bytes, &self.inviter);
        append_descriptor(&mut bytes, &self.joiner);
        bytes.push(1);
        bytes.extend_from_slice(&self.inviter_challenge);
        bytes.extend_from_slice(&self.joiner_challenge);
        bytes
    }

    pub fn sign(
        &self,
        signer: DeviceId,
        key: &DeviceSigningKey,
    ) -> Result<PairingProofV1, PairingError> {
        let signature = sign_ceremony_proof(
            &self.signing_bytes(),
            signer,
            &self.inviter,
            &self.joiner,
            key,
        )?;
        Ok(PairingProofV1 { signer, signature })
    }

    pub fn verify_proof(&self, proof: &PairingProofV1) -> Result<(), PairingError> {
        verify_ceremony_proof(
            &self.signing_bytes(),
            proof.signer,
            proof.signature,
            &self.inviter,
            &self.joiner,
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentGrantScopeV1 {
    Profile,
}

/// The account-authorized enrollment fact signed independently by both
/// devices. This is deliberately not a `PairingTranscriptV1`: its domain and
/// canonical bytes cannot be accepted by the legacy QR ceremony.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentTranscriptV1 {
    pub version: u32,
    pub attestation_digest: [u8; 32],
    pub attestation_jti: String,
    pub profile_id: ProfileId,
    pub relationship_id: RelationshipId,
    pub existing_device: DeviceDescriptorV1,
    pub joining_device: DeviceDescriptorV1,
    pub grant_scope: EnrollmentGrantScopeV1,
    pub existing_challenge: [u8; 32],
    pub joining_challenge: [u8; 32],
}

impl fmt::Debug for EnrollmentTranscriptV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentTranscriptV1(<redacted>)")
    }
}

impl EnrollmentTranscriptV1 {
    pub fn new(
        attestation: &VerifiedDeviceAttestationV1,
        profile_id: ProfileId,
        existing_device: DeviceDescriptorV1,
        joining_device: DeviceDescriptorV1,
        joining_challenge: [u8; 32],
    ) -> Result<Self, PairingError> {
        let mut existing_challenge = [0; 32];
        OsRng.fill_bytes(&mut existing_challenge);
        let value = Self {
            version: ENROLLMENT_VERSION,
            attestation_digest: attestation.digest(),
            attestation_jti: attestation.claims().token_id.clone(),
            profile_id,
            relationship_id: RelationshipId::new(),
            existing_device,
            joining_device,
            grant_scope: EnrollmentGrantScopeV1::Profile,
            existing_challenge,
            joining_challenge,
        };
        value.verify_attestation(attestation)?;
        Ok(value)
    }

    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = b"koru:enrollment-transcript:v1\0".to_vec();
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.attestation_digest);
        append_variable(&mut bytes, self.attestation_jti.as_bytes());
        append_variable(&mut bytes, self.profile_id.to_storage().as_bytes());
        bytes.extend_from_slice(self.relationship_id.as_uuid().as_bytes());
        append_signed_descriptor(&mut bytes, &self.existing_device);
        append_signed_descriptor(&mut bytes, &self.joining_device);
        bytes.push(1);
        bytes.extend_from_slice(&self.existing_challenge);
        bytes.extend_from_slice(&self.joining_challenge);
        bytes
    }

    pub fn verify_attestation(
        &self,
        attestation: &VerifiedDeviceAttestationV1,
    ) -> Result<(), PairingError> {
        verify_descriptor(&self.existing_device)?;
        verify_descriptor(&self.joining_device)?;
        if self.version != ENROLLMENT_VERSION
            || self.grant_scope != EnrollmentGrantScopeV1::Profile
            || self.existing_device.device_id == self.joining_device.device_id
            || self.attestation_digest != attestation.digest()
            || self.attestation_jti != attestation.claims().token_id
            || self.joining_device.device_id != attestation.claims().device_id
            || self.joining_device.signing_public_key != attestation.claims().signing_public_key
            || self.joining_device.endpoint_id != attestation.claims().endpoint_id
            || !attestation
                .claims()
                .authorized_profile_ids
                .contains(&self.profile_id)
        {
            return Err(PairingError::TranscriptMismatch);
        }
        Ok(())
    }

    pub fn sign(
        &self,
        signer: DeviceId,
        key: &DeviceSigningKey,
    ) -> Result<EnrollmentProofV1, PairingError> {
        let signature = sign_ceremony_proof(
            &self.signing_bytes(),
            signer,
            &self.existing_device,
            &self.joining_device,
            key,
        )?;
        Ok(EnrollmentProofV1 { signer, signature })
    }

    pub fn verify_proof(&self, proof: &EnrollmentProofV1) -> Result<(), PairingError> {
        verify_ceremony_proof(
            &self.signing_bytes(),
            proof.signer,
            proof.signature,
            &self.existing_device,
            &self.joining_device,
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentProofV1 {
    pub signer: DeviceId,
    pub signature: SignatureBytes,
}

impl fmt::Debug for EnrollmentProofV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentProofV1(<redacted>)")
    }
}

/// The joining device's independently signed half of enrollment.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentJoinV1 {
    pub attestation: SignedDeviceAttestationV1,
    pub transcript: EnrollmentTranscriptV1,
    pub joining_proof: EnrollmentProofV1,
    pub relationship_created_at_millis: i64,
    pub joining_relationship_signature: SignatureBytes,
    pub joining_grant: ProfileGrantV1,
}

impl fmt::Debug for EnrollmentJoinV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentJoinV1(<redacted>)")
    }
}

impl EnrollmentJoinV1 {
    pub fn sign(
        attestation_evidence: SignedDeviceAttestationV1,
        verified_attestation: &VerifiedDeviceAttestationV1,
        transcript: EnrollmentTranscriptV1,
        key: &DeviceSigningKey,
        created_at_millis: i64,
    ) -> Result<Self, PairingError> {
        transcript.verify_attestation(verified_attestation)?;
        if attestation_evidence.digest() != verified_attestation.digest() {
            return Err(PairingError::AttestationMismatch);
        }
        let joining_proof = transcript.sign(transcript.joining_device.device_id, key)?;
        let relationship = enrollment_relationship_draft(&transcript, created_at_millis);
        let joining_relationship_signature = key.sign_message(&relationship.signing_bytes());
        let joining_grant = ProfileGrantV1::sign(
            transcript.relationship_id,
            RecordId::new(),
            transcript.profile_id,
            transcript.existing_device.device_id,
            transcript.joining_device.device_id,
            created_at_millis,
            key,
        );
        let value = Self {
            attestation: attestation_evidence,
            transcript,
            joining_proof,
            relationship_created_at_millis: created_at_millis,
            joining_relationship_signature,
            joining_grant,
        };
        value.verify(verified_attestation)?;
        Ok(value)
    }

    pub fn verify(&self, attestation: &VerifiedDeviceAttestationV1) -> Result<(), PairingError> {
        self.transcript.verify_attestation(attestation)?;
        if self.attestation.digest() != attestation.digest() {
            return Err(PairingError::AttestationMismatch);
        }
        self.transcript.verify_proof(&self.joining_proof)?;
        if self.joining_proof.signer != self.transcript.joining_device.device_id {
            return Err(PairingError::WrongDevice);
        }
        let relationship =
            enrollment_relationship_draft(&self.transcript, self.relationship_created_at_millis);
        if !self
            .transcript
            .joining_device
            .signing_public_key
            .verify_message(
                &relationship.signing_bytes(),
                self.joining_relationship_signature,
            )
        {
            return Err(PairingError::InvalidSignature);
        }
        verify_profile_grant(
            &self.joining_grant,
            &self.transcript,
            self.transcript.joining_device.device_id,
            self.transcript.existing_device.device_id,
            self.transcript.joining_device.signing_public_key,
        )
    }

    pub fn complete(
        &self,
        attestation: &VerifiedDeviceAttestationV1,
        existing_key: &DeviceSigningKey,
    ) -> Result<PreparedEnrollmentV1, PairingError> {
        self.verify(attestation)?;
        let existing_proof = self
            .transcript
            .sign(self.transcript.existing_device.device_id, existing_key)?;
        let draft =
            enrollment_relationship_draft(&self.transcript, self.relationship_created_at_millis);
        let existing_signature = existing_key.sign_message(&draft.signing_bytes());
        let relationship = attach_relationship_signatures(
            draft,
            existing_signature,
            self.joining_relationship_signature,
        );
        let existing_grant = ProfileGrantV1::sign(
            self.transcript.relationship_id,
            RecordId::new(),
            self.transcript.profile_id,
            self.transcript.joining_device.device_id,
            self.transcript.existing_device.device_id,
            self.relationship_created_at_millis,
            existing_key,
        );
        PreparedEnrollmentV1::assemble(
            self.attestation.clone(),
            self.transcript.clone(),
            existing_proof,
            self.joining_proof.clone(),
            relationship,
            existing_grant,
            self.joining_grant.clone(),
            attestation,
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedEnrollmentV1 {
    pub attestation: SignedDeviceAttestationV1,
    pub transcript: EnrollmentTranscriptV1,
    pub existing_proof: EnrollmentProofV1,
    pub joining_proof: EnrollmentProofV1,
    pub relationship: TrustRelationshipV1,
    pub existing_grant: ProfileGrantV1,
    pub joining_grant: ProfileGrantV1,
}

impl fmt::Debug for PreparedEnrollmentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedEnrollmentV1(<redacted>)")
    }
}

/// Frames carried only on the dedicated enrollment ALPN. The account worker
/// added in Plan 4.5 owns sequencing and policy; this enum freezes the bounded
/// transport vocabulary without putting hosted discovery into the peer layer.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrollmentFrameV1 {
    Begin {
        attestation: SignedDeviceAttestationV1,
        profile_id: ProfileId,
        existing_device_id: DeviceId,
        joining_device: DeviceDescriptorV1,
        joining_challenge: [u8; 32],
    },
    Challenge {
        transcript: EnrollmentTranscriptV1,
    },
    Join {
        join: Box<EnrollmentJoinV1>,
    },
    Complete {
        prepared: Box<PreparedEnrollmentV1>,
    },
    Error {
        code: String,
    },
}

impl fmt::Debug for EnrollmentFrameV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EnrollmentFrameV1(<redacted>)")
    }
}

impl PreparedEnrollmentV1 {
    /// Canonical v1 receipt encoding used by the hosted provisioning route.
    pub fn encode_receipt(&self) -> Result<String, PairingError> {
        let bytes = postcard::to_allocvec(self).map_err(|_| PairingError::Malformed)?;
        if bytes.len() > MAX_ENROLLMENT_RECEIPT_BYTES {
            return Err(PairingError::Malformed);
        }
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn decode_receipt(value: &str) -> Result<Self, PairingError> {
        if value.is_empty() || value.len() > (MAX_ENROLLMENT_RECEIPT_BYTES * 4 / 3 + 4) {
            return Err(PairingError::Malformed);
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| PairingError::Malformed)?;
        if bytes.is_empty() || bytes.len() > MAX_ENROLLMENT_RECEIPT_BYTES {
            return Err(PairingError::Malformed);
        }
        let (receipt, remainder): (Self, &[u8]) =
            postcard::take_from_bytes(&bytes).map_err(|_| PairingError::Malformed)?;
        if !remainder.is_empty()
            || receipt.transcript.version != ENROLLMENT_VERSION
            || receipt.encode_receipt().as_deref() != Ok(value)
        {
            return Err(PairingError::Malformed);
        }
        Ok(receipt)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn assemble(
        attestation_evidence: SignedDeviceAttestationV1,
        transcript: EnrollmentTranscriptV1,
        existing_proof: EnrollmentProofV1,
        joining_proof: EnrollmentProofV1,
        relationship: TrustRelationshipV1,
        existing_grant: ProfileGrantV1,
        joining_grant: ProfileGrantV1,
        attestation: &VerifiedDeviceAttestationV1,
    ) -> Result<Self, PairingError> {
        let value = Self {
            attestation: attestation_evidence,
            transcript,
            existing_proof,
            joining_proof,
            relationship,
            existing_grant,
            joining_grant,
        };
        value.verify(attestation)?;
        Ok(value)
    }

    pub fn verify(&self, attestation: &VerifiedDeviceAttestationV1) -> Result<(), PairingError> {
        self.transcript.verify_attestation(attestation)?;
        if self.attestation.digest() != attestation.digest() {
            return Err(PairingError::AttestationMismatch);
        }
        self.transcript.verify_proof(&self.existing_proof)?;
        self.transcript.verify_proof(&self.joining_proof)?;
        if self.existing_proof.signer != self.transcript.existing_device.device_id
            || self.joining_proof.signer != self.transcript.joining_device.device_id
        {
            return Err(PairingError::WrongDevice);
        }
        if self.relationship.relationship_id != self.transcript.relationship_id
            || self.relationship.inviter != self.transcript.existing_device
            || self.relationship.joiner != self.transcript.joining_device
            || self.relationship.pairing_transcript_hash
                != enrollment_transcript_hash(&self.transcript)
        {
            return Err(PairingError::TranscriptMismatch);
        }
        self.relationship
            .clone()
            .verify()
            .map_err(|_| PairingError::InvalidSignature)?;
        verify_profile_grant(
            &self.existing_grant,
            &self.transcript,
            self.transcript.existing_device.device_id,
            self.transcript.joining_device.device_id,
            self.transcript.existing_device.signing_public_key,
        )?;
        verify_profile_grant(
            &self.joining_grant,
            &self.transcript,
            self.transcript.joining_device.device_id,
            self.transcript.existing_device.device_id,
            self.transcript.joining_device.signing_public_key,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingProofV1 {
    pub signer: DeviceId,
    pub signature: SignatureBytes,
}

/// The joiner's signed half of a pairing exchange. It is safe to send only on
/// the short-lived pairing ALPN: the invitation secret authorizes one claim,
/// while every durable fact is independently bound to both device identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingJoinV1 {
    pub secret: PairingSecret,
    pub transcript: PairingTranscriptV1,
    pub joiner_proof: PairingProofV1,
    pub relationship_created_at_millis: i64,
    pub joiner_relationship_signature: SignatureBytes,
    pub joiner_grant: AllDataGrantV1,
}

impl PairingJoinV1 {
    pub fn sign(
        invitation: &PairingInvitationV1,
        joiner: DeviceDescriptorV1,
        key: &DeviceSigningKey,
        created_at_millis: i64,
    ) -> Result<Self, PairingError> {
        let transcript = PairingTranscriptV1::new(invitation, joiner);
        let joiner_proof = transcript.sign(transcript.joiner.device_id, key)?;
        let relationship = relationship_draft(&transcript, created_at_millis);
        let joiner_relationship_signature = key.sign_message(&relationship.signing_bytes());
        let joiner_grant = AllDataGrantV1::sign(
            transcript.relationship_id,
            transcript.inviter.device_id,
            transcript.joiner.device_id,
            RecordId::new(),
            created_at_millis,
            key,
        );
        let value = Self {
            secret: invitation.secret.clone(),
            transcript,
            joiner_proof,
            relationship_created_at_millis: created_at_millis,
            joiner_relationship_signature,
            joiner_grant,
        };
        value.verify(invitation)?;
        Ok(value)
    }

    pub fn verify(&self, invitation: &PairingInvitationV1) -> Result<(), PairingError> {
        if !invitation.secret.matches(&self.secret) {
            return Err(PairingError::WrongSecret);
        }
        if self.transcript.invitation_id != invitation.invitation_id
            || self.transcript.inviter != invitation.inviter
            || self.transcript.grant_scope != GrantScopeV1::AllData
            || self.transcript.joiner.device_id == invitation.inviter.device_id
        {
            return Err(PairingError::TranscriptMismatch);
        }
        verify_descriptor(&self.transcript.joiner)?;
        self.transcript.verify_proof(&self.joiner_proof)?;
        if self.joiner_proof.signer != self.transcript.joiner.device_id {
            return Err(PairingError::WrongDevice);
        }
        let relationship =
            relationship_draft(&self.transcript, self.relationship_created_at_millis);
        if !self.transcript.joiner.signing_public_key.verify_message(
            &relationship.signing_bytes(),
            self.joiner_relationship_signature,
        ) {
            return Err(PairingError::InvalidSignature);
        }
        verify_grant(
            &self.joiner_grant,
            self.transcript.relationship_id,
            self.transcript.joiner.device_id,
            self.transcript.inviter.device_id,
            self.transcript.joiner.signing_public_key,
        )
    }

    pub fn complete(
        &self,
        invitation: &PairingInvitationV1,
        inviter_key: &DeviceSigningKey,
    ) -> Result<PreparedPairingV1, PairingError> {
        self.verify(invitation)?;
        let inviter_proof = self
            .transcript
            .sign(self.transcript.inviter.device_id, inviter_key)?;
        let draft = relationship_draft(&self.transcript, self.relationship_created_at_millis);
        let inviter_signature = inviter_key.sign_message(&draft.signing_bytes());
        let relationship = attach_relationship_signatures(
            draft,
            inviter_signature,
            self.joiner_relationship_signature,
        );
        let inviter_grant = AllDataGrantV1::sign(
            self.transcript.relationship_id,
            self.transcript.joiner.device_id,
            self.transcript.inviter.device_id,
            RecordId::new(),
            self.relationship_created_at_millis,
            inviter_key,
        );
        PreparedPairingV1::assemble(
            self.transcript.clone(),
            inviter_proof,
            self.joiner_proof.clone(),
            relationship,
            inviter_grant,
            self.joiner_grant.clone(),
        )
    }
}

/// A mutually signed pairing result. Replaying it is idempotent because every
/// relationship and grant id is included in the signed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedPairingV1 {
    pub transcript: PairingTranscriptV1,
    pub inviter_proof: PairingProofV1,
    pub joiner_proof: PairingProofV1,
    pub relationship: TrustRelationshipV1,
    pub inviter_grant: AllDataGrantV1,
    pub joiner_grant: AllDataGrantV1,
}

impl PreparedPairingV1 {
    /// Fixture helper. Production devices sign the same drafts separately and
    /// exchange them over the pairing stream before calling [`Self::assemble`].
    pub fn sign_for_test(
        transcript: PairingTranscriptV1,
        inviter_key: &DeviceSigningKey,
        joiner_key: &DeviceSigningKey,
        created_at_millis: i64,
    ) -> Result<Self, PairingError> {
        let inviter_proof = transcript.sign(transcript.inviter.device_id, inviter_key)?;
        let joiner_proof = transcript.sign(transcript.joiner.device_id, joiner_key)?;
        let draft = relationship_draft(&transcript, created_at_millis);
        let bytes = draft.signing_bytes();
        let relationship = draft.with_signatures(
            inviter_key.sign_message(&bytes),
            joiner_key.sign_message(&bytes),
        );
        let inviter_grant = AllDataGrantV1::sign(
            transcript.relationship_id,
            transcript.joiner.device_id,
            transcript.inviter.device_id,
            RecordId::new(),
            created_at_millis,
            inviter_key,
        );
        let joiner_grant = AllDataGrantV1::sign(
            transcript.relationship_id,
            transcript.inviter.device_id,
            transcript.joiner.device_id,
            RecordId::new(),
            created_at_millis,
            joiner_key,
        );
        Self::assemble(
            transcript,
            inviter_proof,
            joiner_proof,
            relationship,
            inviter_grant,
            joiner_grant,
        )
    }

    pub fn assemble(
        transcript: PairingTranscriptV1,
        inviter_proof: PairingProofV1,
        joiner_proof: PairingProofV1,
        relationship: TrustRelationshipV1,
        inviter_grant: AllDataGrantV1,
        joiner_grant: AllDataGrantV1,
    ) -> Result<Self, PairingError> {
        let value = Self {
            transcript,
            inviter_proof,
            joiner_proof,
            relationship,
            inviter_grant,
            joiner_grant,
        };
        value.verify()?;
        Ok(value)
    }

    pub fn verify(&self) -> Result<(), PairingError> {
        if self.transcript.version != PAIRING_VERSION
            || self.transcript.grant_scope != GrantScopeV1::AllData
        {
            return Err(PairingError::UnsupportedVersion);
        }
        self.transcript.verify_proof(&self.inviter_proof)?;
        self.transcript.verify_proof(&self.joiner_proof)?;
        if self.inviter_proof.signer != self.transcript.inviter.device_id
            || self.joiner_proof.signer != self.transcript.joiner.device_id
        {
            return Err(PairingError::WrongDevice);
        }
        if self.relationship.relationship_id != self.transcript.relationship_id
            || self.relationship.inviter != self.transcript.inviter
            || self.relationship.joiner != self.transcript.joiner
            || self.relationship.pairing_transcript_hash != transcript_hash(&self.transcript)
        {
            return Err(PairingError::TranscriptMismatch);
        }
        self.relationship
            .clone()
            .verify()
            .map_err(|_| PairingError::InvalidSignature)?;
        verify_grant(
            &self.inviter_grant,
            self.transcript.relationship_id,
            self.transcript.inviter.device_id,
            self.transcript.joiner.device_id,
            self.transcript.inviter.signing_public_key,
        )?;
        verify_grant(
            &self.joiner_grant,
            self.transcript.relationship_id,
            self.transcript.joiner.device_id,
            self.transcript.inviter.device_id,
            self.transcript.joiner.signing_public_key,
        )?;
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingStage {
    Idle,
    Displaying,
    Connecting,
    Confirming,
    Preparing,
    Finishing,
    Succeeded,
    Expired,
    Cancelled,
    Failed,
}

pub struct InvitationSession {
    pub invitation: PairingInvitationV1,
    deadline: Instant,
    stage: PairingStage,
    claimed_by: Option<DeviceId>,
}

impl InvitationSession {
    pub fn new(inviter: DeviceDescriptorV1, relay_hint: Option<String>, now: Instant) -> Self {
        Self {
            invitation: PairingInvitationV1::new(inviter, relay_hint),
            deadline: now + INVITATION_LIFETIME,
            stage: PairingStage::Displaying,
            claimed_by: None,
        }
    }
    pub fn stage(&self, now: Instant) -> PairingStage {
        if now >= self.deadline
            && !matches!(
                self.stage,
                PairingStage::Succeeded | PairingStage::Cancelled
            )
        {
            PairingStage::Expired
        } else {
            self.stage
        }
    }
    pub fn claim(
        &mut self,
        secret: &PairingSecret,
        joiner: DeviceId,
        now: Instant,
    ) -> Result<(), PairingError> {
        if now >= self.deadline {
            self.stage = PairingStage::Expired;
            return Err(PairingError::Expired);
        }
        if self.claimed_by.is_some() {
            return Err(PairingError::AlreadyClaimed);
        }
        if !self.invitation.secret.matches(secret) {
            return Err(PairingError::WrongSecret);
        }
        self.claimed_by = Some(joiner);
        self.stage = PairingStage::Connecting;
        Ok(())
    }
    pub fn cancel(&mut self) {
        if self.stage != PairingStage::Succeeded {
            self.stage = PairingStage::Cancelled;
        }
    }

    pub fn preparing(&mut self) {
        if !matches!(
            self.stage,
            PairingStage::Cancelled | PairingStage::Succeeded
        ) {
            self.stage = PairingStage::Preparing;
        }
    }

    pub fn succeed(&mut self) {
        if self.stage != PairingStage::Cancelled {
            self.stage = PairingStage::Succeeded;
        }
    }

    pub fn fail(&mut self) {
        if !matches!(
            self.stage,
            PairingStage::Cancelled | PairingStage::Succeeded
        ) {
            self.stage = PairingStage::Failed;
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PairingError {
    Malformed,
    UnsupportedVersion,
    InvalidSignature,
    EndpointMismatch,
    Expired,
    AlreadyClaimed,
    WrongSecret,
    WrongDevice,
    TranscriptMismatch,
    AttestationMismatch,
}

pub fn transcript_hash(transcript: &PairingTranscriptV1) -> [u8; 32] {
    Sha256::digest(transcript.signing_bytes()).into()
}

pub fn relationship_draft(
    transcript: &PairingTranscriptV1,
    created_at_millis: i64,
) -> TrustRelationshipV1 {
    relationship_draft_from_parts(
        transcript.relationship_id,
        transcript.inviter.clone(),
        transcript.joiner.clone(),
        transcript_hash(transcript),
        created_at_millis,
    )
}

pub fn enrollment_transcript_hash(transcript: &EnrollmentTranscriptV1) -> [u8; 32] {
    Sha256::digest(transcript.signing_bytes()).into()
}

pub fn enrollment_relationship_draft(
    transcript: &EnrollmentTranscriptV1,
    created_at_millis: i64,
) -> TrustRelationshipV1 {
    relationship_draft_from_parts(
        transcript.relationship_id,
        transcript.existing_device.clone(),
        transcript.joining_device.clone(),
        enrollment_transcript_hash(transcript),
        created_at_millis,
    )
}

pub fn verify_enrollment_responder_transport(
    transcript: &EnrollmentTranscriptV1,
    local_endpoint: ReplicationEndpointId,
    remote_endpoint: ReplicationEndpointId,
) -> Result<(), PairingError> {
    if transcript.existing_device.endpoint_id != local_endpoint
        || transcript.joining_device.endpoint_id != remote_endpoint
    {
        return Err(PairingError::EndpointMismatch);
    }
    Ok(())
}

pub fn verify_enrollment_joiner_transport(
    transcript: &EnrollmentTranscriptV1,
    local_endpoint: ReplicationEndpointId,
    remote_endpoint: ReplicationEndpointId,
) -> Result<(), PairingError> {
    if transcript.joining_device.endpoint_id != local_endpoint
        || transcript.existing_device.endpoint_id != remote_endpoint
    {
        return Err(PairingError::EndpointMismatch);
    }
    Ok(())
}

fn verify_grant(
    grant: &AllDataGrantV1,
    relationship_id: RelationshipId,
    issued_by: DeviceId,
    device_id: DeviceId,
    issuer_key: koru_device::DevicePublicKey,
) -> Result<(), PairingError> {
    if grant.relationship_id != relationship_id
        || grant.issued_by != issued_by
        || grant.device_id != device_id
    {
        return Err(PairingError::TranscriptMismatch);
    }
    grant
        .clone()
        .verify(issuer_key)
        .map(|_| ())
        .map_err(|_| PairingError::InvalidSignature)
}

fn append_descriptor(bytes: &mut Vec<u8>, descriptor: &DeviceDescriptorV1) {
    bytes.extend_from_slice(descriptor.device_id.as_uuid().as_bytes());
    bytes.extend_from_slice(descriptor.signing_public_key.as_bytes());
    bytes.extend_from_slice(descriptor.endpoint_id.as_bytes());
}

fn append_signed_descriptor(bytes: &mut Vec<u8>, descriptor: &DeviceDescriptorV1) {
    bytes.extend_from_slice(&descriptor.version.to_be_bytes());
    bytes.extend_from_slice(descriptor.device_id.as_uuid().as_bytes());
    append_variable(bytes, descriptor.display_name.as_bytes());
    bytes.extend_from_slice(descriptor.signing_public_key.as_bytes());
    bytes.extend_from_slice(descriptor.endpoint_id.as_bytes());
    bytes.extend_from_slice(&descriptor.created_at_millis.to_be_bytes());
    bytes.extend_from_slice(descriptor.endpoint_binding_signature.as_bytes());
}

fn append_variable(bytes: &mut Vec<u8>, value: &[u8]) {
    let length = u32::try_from(value.len()).expect("ceremony fields are bounded to u32 bytes");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
}

fn verify_descriptor(descriptor: &DeviceDescriptorV1) -> Result<(), PairingError> {
    descriptor
        .verify()
        .map_err(|_| PairingError::InvalidSignature)
}

fn ceremony_key(
    signer: DeviceId,
    first: &DeviceDescriptorV1,
    second: &DeviceDescriptorV1,
) -> Result<DevicePublicKey, PairingError> {
    if signer == first.device_id {
        Ok(first.signing_public_key)
    } else if signer == second.device_id {
        Ok(second.signing_public_key)
    } else {
        Err(PairingError::WrongDevice)
    }
}

fn sign_ceremony_proof(
    signing_bytes: &[u8],
    signer: DeviceId,
    first: &DeviceDescriptorV1,
    second: &DeviceDescriptorV1,
    key: &DeviceSigningKey,
) -> Result<SignatureBytes, PairingError> {
    if key.public_key() != ceremony_key(signer, first, second)? {
        return Err(PairingError::WrongDevice);
    }
    Ok(key.sign_message(signing_bytes))
}

fn verify_ceremony_proof(
    signing_bytes: &[u8],
    signer: DeviceId,
    signature: SignatureBytes,
    first: &DeviceDescriptorV1,
    second: &DeviceDescriptorV1,
) -> Result<(), PairingError> {
    ceremony_key(signer, first, second)?
        .verify_message(signing_bytes, signature)
        .then_some(())
        .ok_or(PairingError::InvalidSignature)
}

fn relationship_draft_from_parts(
    relationship_id: RelationshipId,
    first: DeviceDescriptorV1,
    second: DeviceDescriptorV1,
    transcript_hash: [u8; 32],
    created_at_millis: i64,
) -> TrustRelationshipV1 {
    TrustRelationshipV1::unsigned(
        relationship_id,
        first,
        second,
        transcript_hash,
        created_at_millis,
    )
}

fn attach_relationship_signatures(
    draft: TrustRelationshipV1,
    first_signature: SignatureBytes,
    second_signature: SignatureBytes,
) -> TrustRelationshipV1 {
    draft.with_signatures(first_signature, second_signature)
}

fn verify_profile_grant(
    grant: &ProfileGrantV1,
    transcript: &EnrollmentTranscriptV1,
    issued_by: DeviceId,
    grantee: DeviceId,
    issuer_key: DevicePublicKey,
) -> Result<(), PairingError> {
    if grant.relationship_id != transcript.relationship_id
        || grant.profile_id != transcript.profile_id
        || grant.issued_by != issued_by
        || grant.grantee_device_id != grantee
    {
        return Err(PairingError::TranscriptMismatch);
    }
    grant
        .clone()
        .verify(issuer_key)
        .map(|_| ())
        .map_err(|_| PairingError::InvalidSignature)
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for i in 0..32 {
        difference |= left[i] ^ right[i];
    }
    difference == 0
}

#[cfg(test)]
mod tests;
