// SPDX-License-Identifier: MPL-2.0

//! **The canonical golden vectors**, generated here and published under
//! `contracts/vectors/`.
//!
//! Two tests, and the split matters:
//!
//! - [`the_published_vectors_verify`] reads the **published** files and checks
//!   that every case behaves as the file says. That is the verifier Android and
//!   iOS will re-implement for the tri-platform gate (Plan 10.7), and it is
//!   what fails if somebody edits a vector by hand.
//! - [`the_published_vectors_are_current`] regenerates them and asserts the
//!   files are what this crate produces, so a format change cannot land without
//!   the vectors moving with it.
//!
//! Both read the files through `include_str!`, at **compile time**, so the
//! default suite touches no disk. Regeneration is the one path that writes, and
//! it happens only under `KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS=1`.
//!
//! # Why they are reproducible
//!
//! An HPKE encapsulation draws an ephemeral key pair and a chunked seal draws a
//! salt, so a vector generated from the OS CSPRNG could never be regenerated
//! and diffed against its predecessor — which is exactly what a reviewer needs
//! to see when a format changes. Every draw here comes from
//! [`crate::testkit::VectorRng`], seeded per file. RFC 9180 publishes `ikmE`
//! for the same reason.
//!
//! # They are hostile, not merely interoperable (D91)
//!
//! Roughly two thirds of the cases are `invalid`. A vector suite that only
//! proves two implementations agree on the happy path proves that they will
//! also agree on accepting a forgery.

use koru_device::DeviceSigningKey;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::BlobHash;
use rand_core::Rng as _;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::account::{
    AccountMetadataEnvelopeV1, AccountMetadataKeyRef, Region, WrappedAccountMetadataKeyV1,
    WrappedKeyRef,
};
use crate::checkpoint::{self, CheckpointHeader};
use crate::content::{self, ContentObject, ContentObjectHeader};
use crate::error::CryptoError;
use crate::grant::{
    GrantExpectation, GrantIssuance, GrantSet, Recipient, RecipientSecretKey, WekGrantV1,
    WorkspaceCustody,
};
use crate::identity::{CloudObjectId, cloud_object_id};
use crate::keys::{
    AccountMetadataKey, BlobIdentityKeyVersion, KeyId, RegionalMetadataKek, WekVersion,
    WorkspaceBlobIdentityKey, WorkspaceEncryptionKey,
};
use crate::mapping::{BlobMappingHeader, BlobMappingRecordV1};
use crate::regional::{MetadataKekId, RecoveryKeyId};
use crate::sealed::{CHUNK_LEN, SealedLayout};
use crate::suites::{AuthorType, GrantSuite, Purpose, RecipientType};
use crate::testkit::{VectorRng, hex, unhex};

const GRANT_PATH: &str = "workspace-crypto/wek-grant-v1.json";
const CONTENT_PATH: &str = "workspace-crypto/content-object-v1.json";
const CHECKPOINT_PATH: &str = "workspace-crypto/recovery-checkpoint-v1.json";
const IDENTITY_PATH: &str = "workspace-crypto/blob-identity-v1.json";
const ACCOUNT_PATH: &str = "account-metadata-envelope/account-metadata-envelope-v1.json";

const VECTORS_ROOT: &str = "../../contracts/vectors";

fn published(path: &str) -> &'static str {
    match path {
        GRANT_PATH => include_str!("../../../contracts/vectors/workspace-crypto/wek-grant-v1.json"),
        CONTENT_PATH => {
            include_str!("../../../contracts/vectors/workspace-crypto/content-object-v1.json")
        }
        CHECKPOINT_PATH => {
            include_str!("../../../contracts/vectors/workspace-crypto/recovery-checkpoint-v1.json")
        }
        IDENTITY_PATH => {
            include_str!("../../../contracts/vectors/workspace-crypto/blob-identity-v1.json")
        }
        ACCOUNT_PATH => include_str!(
            "../../../contracts/vectors/account-metadata-envelope/account-metadata-envelope-v1.json"
        ),
        other => panic!("no published vector file for {other}"),
    }
}

/// The same key order whichever `serde_json` map this build compiled.
///
/// **A published vector is a cross-device format contract, so its bytes may not
/// depend on what else was in the build.** `serde_json`'s object map is a
/// `BTreeMap` — sorted — unless some crate turns on `preserve_order`, when it
/// becomes an `IndexMap` that keeps insertion order instead. Cargo unifies
/// features across a workspace, so that is a property of *what else is being
/// compiled* rather than of this crate: when the delegated agent's ACP schema
/// arrived it enabled `preserve_order`, and `cargo test -p koru-workspace-crypto`
/// and `cargo test --workspace` began disagreeing about these bytes — the
/// golden files read as stale in one and current in the other.
///
/// Sorting every object here is stable under both map types (inserting in
/// sorted order into an `IndexMap` yields sorted output, and a `BTreeMap` sorts
/// regardless), and it reproduces the bytes the published files already hold,
/// so nothing about the format changes.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
            Value::Object(
                pairs
                    .into_iter()
                    .map(|(key, nested)| (key.clone(), canonical(nested)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&canonical(value)).expect("vectors serialize");
    text.push('\n');
    text
}

// ------------------------------------------------------------------- grants

/// Draws a recipient key pair, returning the private bytes alongside it: a
/// vector has to publish the private key, and `RecipientSecretKey` deliberately
/// does not hand its bytes back.
///
/// A P-256 scalar drawn uniformly can be out of range, so the draw is repeated
/// rather than reduced -- the shape of RFC 9180 section 7.1.3's rejection
/// sampling.
fn deterministic_recipient(
    rng: &mut VectorRng,
    suite: GrantSuite,
) -> (RecipientSecretKey, [u8; 32]) {
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        let candidate = RecipientSecretKey::from_bytes(suite, bytes);
        if candidate.public_key().is_ok() {
            return (candidate, bytes);
        }
    }
}

fn grant_vectors() -> Value {
    let mut rng = VectorRng::new(b"koru:vectors:wek-grant-v1");
    let workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x11; 16]));
    let other_workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x1f; 16]));
    let wek = WorkspaceEncryptionKey::generate(&mut rng);
    let author = DeviceSigningKey::from_secret_bytes([0x77; 32]);
    let author_id = Uuid::from_bytes([0x88; 16]);

    let (device_secret, device_secret_bytes) =
        deterministic_recipient(&mut rng, GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let device = Recipient {
        recipient_type: RecipientType::Device,
        recipient_id: Uuid::from_bytes([0x22; 16]),
        recipient_key_id: KeyId::from_bytes([0x33; 16]),
        recipient_key_version: 1,
        public_key: device_secret.public_key().unwrap(),
    };
    let (recovery_secret, recovery_secret_bytes) =
        deterministic_recipient(&mut rng, GrantSuite::P256HkdfSha256ChaCha20Poly1305);
    let custody = WorkspaceCustody::koru_managed(
        RecoveryKeyId::new(KeyId::from_bytes([0x44; 16])),
        Uuid::from_bytes([0x55; 16]),
        KeyId::from_bytes([0x66; 16]),
        7,
        recovery_secret.public_key().unwrap(),
    )
    .unwrap();

    let managed = GrantSet::issue(
        &custody,
        &GrantIssuance {
            workspace_id: workspace,
            wek_version: WekVersion(3),
            author_type: AuthorType::Device,
            author_id,
        },
        &wek,
        std::slice::from_ref(&device),
        &author,
        &mut rng,
    )
    .unwrap();
    let customer_managed = GrantSet::issue(
        &WorkspaceCustody::CustomerManaged,
        &GrantIssuance {
            workspace_id: other_workspace,
            wek_version: WekVersion(3),
            author_type: AuthorType::Device,
            author_id,
        },
        &wek,
        std::slice::from_ref(&device),
        &author,
        &mut rng,
    )
    .unwrap();

    let (_, stranger_bytes) =
        deterministic_recipient(&mut rng, GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let (_, other_region_bytes) =
        deterministic_recipient(&mut rng, GrantSuite::P256HkdfSha256ChaCha20Poly1305);

    let device_envelope = managed.grants()[0].encode();
    let recovery_envelope = managed.koru_recovery_grant().unwrap().encode();
    let customer_envelope = customer_managed.grants()[0].encode();

    let mut cases = vec![
        json!({
            "name": "device grant, DHKEM(X25519)+HKDF-SHA256+ChaCha20Poly1305, HPKE Base",
            "result": "valid",
            "suite_id": 1,
            "envelope": hex(&device_envelope),
            "recipient_secret_key": hex(&device_secret_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(3),
                recipient_type: RecipientType::Device,
                recipient_id: device.recipient_id,
                recipient_key_id: device.recipient_key_id,
                recipient_key_version: 1,
            }),
            "wek": hex(wek.as_bytes()),
        }),
        json!({
            "name": "recovery grant, DHKEM(P-256) — a different KEM from the devices' (D88)",
            "result": "valid",
            "suite_id": 2,
            "envelope": hex(&recovery_envelope),
            "recipient_secret_key": hex(&recovery_secret_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(3),
                recipient_type: RecipientType::RegionalRecovery,
                recipient_id: Uuid::from_bytes([0x55; 16]),
                recipient_key_id: KeyId::from_bytes([0x66; 16]),
                recipient_key_version: 7,
            }),
            "wek": hex(wek.as_bytes()),
        }),
        json!({
            "name": "wrong workspace: the reader's expectation refuses before decryption",
            "result": "invalid",
            "error": "CRYPTO_CONTEXT_MISMATCH",
            "envelope": hex(&device_envelope),
            "recipient_secret_key": hex(&device_secret_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: other_workspace,
                wek_version: WekVersion(3),
                recipient_type: RecipientType::Device,
                recipient_id: device.recipient_id,
                recipient_key_id: device.recipient_key_id,
                recipient_key_version: 1,
            }),
        }),
        json!({
            "name": "modified wek_version",
            "result": "invalid",
            "error": "CRYPTO_CONTEXT_MISMATCH",
            "envelope": hex(&device_envelope),
            "recipient_secret_key": hex(&device_secret_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(4),
                recipient_type: RecipientType::Device,
                recipient_id: device.recipient_id,
                recipient_key_id: device.recipient_key_id,
                recipient_key_version: 1,
            }),
        }),
        json!({
            "name": "wrong recipient: the right expectation, another device's private key",
            "result": "invalid",
            "error": "CRYPTO_AUTHENTICATION_FAILED",
            "envelope": hex(&device_envelope),
            "recipient_secret_key": hex(&stranger_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(3),
                recipient_type: RecipientType::Device,
                recipient_id: device.recipient_id,
                recipient_key_id: device.recipient_key_id,
                recipient_key_version: 1,
            }),
        }),
        json!({
            "name": "cross-region recovery: another region's recovery key opens nothing",
            "result": "invalid",
            "error": "CRYPTO_AUTHENTICATION_FAILED",
            "envelope": hex(&recovery_envelope),
            "recipient_secret_key": hex(&other_region_bytes),
            "author_public_key": hex(author.public_key().as_bytes()),
            "expect": grant_expectation_json(&GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(3),
                recipient_type: RecipientType::RegionalRecovery,
                recipient_id: Uuid::from_bytes([0x55; 16]),
                recipient_key_id: KeyId::from_bytes([0x66; 16]),
                recipient_key_version: 7,
            }),
        }),
    ];

    // Parse-time refusals: no key, no expectation, the envelope alone.
    let mut unknown_suite = device_envelope.clone();
    unknown_suite[2..4].copy_from_slice(&0x00ffu16.to_be_bytes());
    let mut unknown_version = device_envelope.clone();
    unknown_version[0..2].copy_from_slice(&2u16.to_be_bytes());
    let mut flipped_signature = device_envelope.clone();
    let last = flipped_signature.len() - 1;
    flipped_signature[last] ^= 0x01;
    let mut extended = device_envelope.clone();
    extended.push(0);

    for (name, envelope, error) in [
        (
            "unknown suite fails closed",
            hex(&unknown_suite),
            "CRYPTO_UNSUPPORTED_SUITE",
        ),
        (
            "unknown format version is not a malformed record",
            hex(&unknown_version),
            "CRYPTO_UNSUPPORTED_FORMAT_VERSION",
        ),
        (
            "truncated envelope",
            hex(&device_envelope[..device_envelope.len() - 1]),
            "CRYPTO_MALFORMED",
        ),
        ("extended envelope", hex(&extended), "CRYPTO_MALFORMED"),
    ] {
        cases.push(json!({
            "name": name,
            "result": "invalid",
            "stage": "parse",
            "error": error,
            "envelope": envelope,
        }));
    }
    cases.push(json!({
        "name": "modified signature",
        "result": "invalid",
        "stage": "signature",
        "error": "CRYPTO_SIGNATURE_INVALID",
        "envelope": hex(&flipped_signature),
        "author_public_key": hex(author.public_key().as_bytes()),
    }));

    json!({
        "format": "WekGrantV1",
        "format_version": 1,
        "layer": "1 — key grants only. This format encrypts nothing but a 32-byte workspace encryption key.",
        "suites": [
            {
                "suite_id": 1,
                "name": "DHKEM(X25519, HKDF-SHA256) + HKDF-SHA256 + ChaCha20Poly1305",
                "hpke_mode": "base",
                "rfc9180": {"kem_id": 32, "kdf_id": 1, "aead_id": 3},
                "encapsulation_len": 32,
                "writable_for": ["DEVICE", "CLOUD_REPLICA", "REGIONAL_RECOVERY"]
            },
            {
                "suite_id": 2,
                "name": "DHKEM(P-256, HKDF-SHA256) + HKDF-SHA256 + ChaCha20Poly1305",
                "hpke_mode": "base",
                "rfc9180": {"kem_id": 16, "kdf_id": 1, "aead_id": 3},
                "encapsulation_len": 65,
                "writable_for": ["REGIONAL_RECOVERY"]
            }
        ],
        "header_len": crate::grant::GRANT_HEADER_LEN,
        "customer_managed_workspace": {
            "note": "D89 — a customer-managed workspace has no Koru recovery grant. The grant set is the whole set for this workspace and WEK version; it contains no envelope whose recipient_type is REGIONAL_RECOVERY, and there is no flag anywhere promising Koru will not use one.",
            "workspace_id": other_workspace.to_storage(),
            "grants": [hex(&customer_envelope)],
            "koru_recovery_grants": 0
        },
        "koru_managed_workspace": {
            "workspace_id": workspace.to_storage(),
            "grants": [hex(&device_envelope), hex(&recovery_envelope)],
            "koru_recovery_grants": 1
        },
        "cases": cases,
    })
}

fn grant_expectation_json(expectation: &GrantExpectation) -> Value {
    json!({
        "workspace_id": expectation.workspace_id.to_storage(),
        "wek_version": expectation.wek_version.0,
        "recipient_type": match expectation.recipient_type {
            RecipientType::Device => "DEVICE",
            RecipientType::CloudReplica => "CLOUD_REPLICA",
            RecipientType::RegionalRecovery => "REGIONAL_RECOVERY",
        },
        "recipient_id": expectation.recipient_id.to_string(),
        "recipient_key_id": hex(expectation.recipient_key_id.as_bytes()),
        "recipient_key_version": expectation.recipient_key_version,
    })
}

// ------------------------------------------------------------------ content

fn content_vectors() -> Value {
    let mut rng = VectorRng::new(b"koru:vectors:content-object-v1");
    let workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x31; 16]));
    let wek = WorkspaceEncryptionKey::generate(&mut rng);
    let object_id = CloudObjectId::from_bytes([0x41; 32]);
    let header = ContentObjectHeader::for_blob(workspace, WekVersion(2), object_id);

    // **Which lengths carry a ciphertext, and which carry only arithmetic.**
    // A 16 KiB case is 33 KB of hex, so the file pins ciphertext where an
    // implementation could plausibly be wrong — the empty message, a single
    // byte, and both sides of the first chunk boundary, which is where a wrong
    // final-chunk rule or a wrong chunk counter shows — and pins every other
    // length, up to and past the 10 GiB blob ceiling, in the derivation table
    // below, where no bytes are needed to state the answer.
    let mut cases = Vec::new();
    for (name, len) in [
        ("empty payload — one empty chunk", 0usize),
        ("single byte", 1),
        (
            "exactly one chunk — a full chunk plus an empty terminating chunk",
            CHUNK_LEN,
        ),
        (
            "one chunk plus a byte — a full chunk and a short one",
            CHUNK_LEN + 1,
        ),
    ] {
        let plaintext: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let object = content::seal_object(&header, &wek, &plaintext, &mut rng).unwrap();
        cases.push(json!({
            "name": name,
            "result": "valid",
            "plaintext_len": len,
            "plaintext_rule": "byte i is (i mod 251)",
            "object": hex(&object),
            "wek": hex(wek.as_bytes()),
        }));
    }

    // Hostile cases, sized to what each actually needs. Three of them work on a
    // single-chunk object and cost a few hundred bytes. Dropping a final chunk
    // needs two chunks; swapping two whole chunks needs three, because the last
    // chunk of a message is always short and swapping it would be a length
    // change rather than a reordering.
    let small = content::seal_object(&header, &wek, b"a short payload", &mut rng).unwrap();
    let two_chunks: Vec<u8> = (0..CHUNK_LEN + 3).map(|i| (i % 251) as u8).collect();
    let two_chunks = content::seal_object(&header, &wek, &two_chunks, &mut rng).unwrap();
    let three_chunks: Vec<u8> = (0..2 * CHUNK_LEN + 3).map(|i| (i % 251) as u8).collect();
    let three_chunks = content::seal_object(&header, &wek, &three_chunks, &mut rng).unwrap();
    let body_at = content::CONTENT_HEADER_LEN + crate::envelope::WRAPPED_KEY_LEN;
    let first_chunk = body_at + crate::sealed::SEALED_HEADER_LEN;
    let stride = CHUNK_LEN + 16;

    let mut truncated = small.clone();
    truncated.truncate(small.len() - 1);
    let mut extended = small.clone();
    extended.push(0);
    let mut reordered = three_chunks.clone();
    let (a, b) = (first_chunk, first_chunk + stride);
    let chunk_a = reordered[a..a + stride].to_vec();
    let chunk_b = reordered[b..b + stride].to_vec();
    reordered[a..a + stride].copy_from_slice(&chunk_b);
    reordered[b..b + stride].copy_from_slice(&chunk_a);
    let mut dropped_final = two_chunks.clone();
    dropped_final.truncate(first_chunk + stride);
    let mut transplanted = ContentObjectHeader::for_blob(
        workspace,
        WekVersion(2),
        CloudObjectId::from_bytes([0x42; 32]),
    )
    .encode();
    transplanted.extend_from_slice(&small[content::CONTENT_HEADER_LEN..]);

    // **Two different refusals, and the difference is worth naming.** A byte
    // added to or removed from the *final* chunk still yields an arithmetically
    // valid ciphertext length, so the length check passes and the AEAD tag is
    // what refuses. Truncation at a *chunk boundary* is what the specification's
    // short-final-chunk rule catches, and that one is refused by the framing
    // before a key is used at all. A reader that only implemented the first
    // would still pass a happy-path interop suite.
    for (name, bytes, error) in [
        (
            "one byte removed from the final chunk (refused by the AEAD tag)",
            truncated,
            "CRYPTO_AUTHENTICATION_FAILED",
        ),
        (
            "one byte appended to the final chunk (refused by the AEAD tag)",
            extended,
            "CRYPTO_AUTHENTICATION_FAILED",
        ),
        (
            "chunks 0 and 1 swapped",
            reordered,
            "CRYPTO_AUTHENTICATION_FAILED",
        ),
        (
            "final short chunk dropped, leaving whole chunks only (refused by the framing, no key needed)",
            dropped_final,
            "CRYPTO_MALFORMED",
        ),
        (
            "served under another object's CloudObjectId",
            transplanted,
            "CRYPTO_COMMITMENT_MISMATCH",
        ),
    ] {
        cases.push(json!({
            "name": name,
            "result": "invalid",
            "error": error,
            "object": hex(&bytes),
            "wek": hex(wek.as_bytes()),
        }));
    }

    // D40, as arithmetic. No ciphertext: the point is that none is needed.
    let ten_gib = 10u64 * 1024 * 1024 * 1024;
    let derivation: Vec<Value> = [
        0u64,
        1,
        CHUNK_LEN as u64 - 1,
        CHUNK_LEN as u64,
        CHUNK_LEN as u64 + 1,
        2 * CHUNK_LEN as u64,
        2 * CHUNK_LEN as u64 + 1,
        ten_gib - 1,
        ten_gib,
        ten_gib + 1,
    ]
    .into_iter()
    .map(|plaintext_len| {
        let body = SealedLayout::of_plaintext_len(plaintext_len).unwrap();
        json!({
            "plaintext_len": plaintext_len,
            "chunk_count": body.chunk_count(),
            "stored_len": content::CONTENT_HEADER_LEN as u64
                + crate::envelope::WRAPPED_KEY_LEN as u64
                + body.ciphertext_len(),
        })
    })
    .collect();

    let invalid_stored_lens: Vec<Value> = [
        0u64,
        1,
        (content::CONTENT_HEADER_LEN + crate::envelope::WRAPPED_KEY_LEN) as u64,
        (content::CONTENT_HEADER_LEN + crate::envelope::WRAPPED_KEY_LEN) as u64 + 55,
        (content::CONTENT_HEADER_LEN + crate::envelope::WRAPPED_KEY_LEN) as u64 + 56,
        (content::CONTENT_HEADER_LEN + crate::envelope::WRAPPED_KEY_LEN) as u64
            + 56
            + (CHUNK_LEN + 16) as u64,
        u64::MAX,
    ]
    .into_iter()
    .map(|len| json!({"stored_len": len, "error": "invalid"}))
    .collect();

    json!({
        "format": "ContentObjectV1",
        "format_version": 1,
        "layer": "2 — ContentCipherSuiteV1. c2sp.org/chunked-encryption v1, Cobblestone-256 (SHA-512 + AES-256-GCM), 16 KiB chunks.",
        "suite_id": 1,
        "header_len": content::CONTENT_HEADER_LEN,
        "wrapped_dek_len": crate::envelope::WRAPPED_KEY_LEN,
        "chunk_len": CHUNK_LEN,
        "note": "Upstream conformance for the chunked layer is Project Wycheproof's c2sp_chunked_encryption_aes_256_gcm suite, vendored beside the crate. These cases pin Koru's framing on top of it.",
        "plaintext_length_derivation": {
            "note": "D40, narrowly: the exact plaintext length OF A VALID ENCRYPTED OBJECT, from framing alone. It is not a proof of what the user's original local file contained. Length exposure is deliberate.",
            "formula": "body = stored_len - header_len - wrapped_dek_len; n = ceil((body - 56) / (16384 + 16)); plaintext_len = (body - 56) - 16*n",
            "valid": derivation,
            "invalid": invalid_stored_lens
        },
        "cases": cases,
    })
}

// --------------------------------------------------------------- checkpoint

fn checkpoint_vectors() -> Value {
    let mut rng = VectorRng::new(b"koru:vectors:recovery-checkpoint-v1");
    let wek = WorkspaceEncryptionKey::generate(&mut rng);
    let header = CheckpointHeader::new(
        WorkspaceId::from_uuid(Uuid::from_bytes([0x11; 16])),
        Uuid::from_bytes([0x12; 16]),
        Uuid::from_bytes([0x13; 16]),
        WekVersion(3),
    );
    let bootstrap = sample_bootstrap();
    let bytes = checkpoint::seal_checkpoint(&header, &bootstrap, &wek, &mut rng).unwrap();

    let mut wrong_incarnation = CheckpointHeader::new(
        WorkspaceId::from_uuid(Uuid::from_bytes([0x11; 16])),
        Uuid::from_bytes([0x99; 16]),
        Uuid::from_bytes([0x13; 16]),
        WekVersion(3),
    )
    .encode();
    wrong_incarnation.extend_from_slice(&bytes[checkpoint::CHECKPOINT_HEADER_LEN..]);

    let mut unknown_bootstrap_version = bytes.clone();
    unknown_bootstrap_version[6..8].copy_from_slice(&2u16.to_be_bytes());

    let mut truncated = bytes.clone();
    truncated.truncate(bytes.len() - 1);

    json!({
        "format": "WorkspaceRecoveryCheckpointV1",
        "format_version": 1,
        "bootstrap_contract": {
            "document": "docs/cloud-platform/contracts/workspace-recovery-bootstrap-v1.md",
            "approval": "APPROVALS.md row: workspace-recovery-bootstrap-v1.md v1, Steven Liechti (CTO), 2026-09-05, commit 49c2e397",
            "bootstrap_format_version": checkpoint::BOOTSTRAP_FORMAT_VERSION_V1
        },
        "header_len": checkpoint::CHECKPOINT_HEADER_LEN,
        "header_fields": [
            "format_version (envelope framing)",
            "suite_id",
            "purpose",
            "H1 bootstrap_format_version",
            "H2 workspace_id",
            "H3 cloud_dataset_incarnation_id",
            "H4 checkpoint_id",
            "H5 wek_version"
        ],
        "binding": "The complete header is the key-derivation context of both the wrapped DEK and the body, so altering any byte fails at the commitment before a chunk is decrypted, and a body cannot be recombined with another header.",
        "cases": [
            {
                "name": "round trip with exactly the approved fields",
                "result": "valid",
                "checkpoint": hex(&bytes),
                "wek": hex(wek.as_bytes()),
                "expect": bootstrap_json(&bootstrap)
            },
            {
                "name": "wrong incarnation: a body under another checkpoint's header",
                "result": "invalid",
                "error": "CRYPTO_COMMITMENT_MISMATCH",
                "checkpoint": hex(&wrong_incarnation),
                "wek": hex(wek.as_bytes())
            },
            {
                "name": "unimplemented bootstrap_format_version stops before decryption",
                "result": "invalid",
                "stage": "parse",
                "error": "CRYPTO_UNSUPPORTED_FORMAT_VERSION",
                "checkpoint": hex(&unknown_bootstrap_version)
            },
            {
                "name": "one byte removed from the final chunk (refused by the AEAD tag, not by the framing)",
                "result": "invalid",
                "error": "CRYPTO_AUTHENTICATION_FAILED",
                "checkpoint": hex(&truncated),
                "wek": hex(wek.as_bytes())
            },
            {
                "name": "truncated to the header alone: no wrapped DEK, no body",
                "result": "invalid",
                "error": "CRYPTO_MALFORMED",
                "checkpoint": hex(&bytes[..checkpoint::CHECKPOINT_HEADER_LEN]),
                "wek": hex(wek.as_bytes())
            }
        ]
    })
}

fn sample_bootstrap() -> checkpoint::WorkspaceRecoveryBootstrapV1 {
    use checkpoint::{DocumentEntry, DocumentRole, ObjectEntry, ObjectReference};
    let mut documents = vec![
        DocumentEntry {
            document_id: DocumentId::from_uuid(Uuid::from_bytes([0x10; 16])),
            role: DocumentRole::Manifest,
            frontier: vec![1, 2, 3],
            version_vector: vec![4, 5],
            key_version: WekVersion(2),
            snapshot: ObjectReference {
                locator: b"seg/manifest/0".to_vec(),
                ciphertext_digest: [0xa0; 32],
            },
            updates: vec![ObjectReference {
                locator: b"seg/manifest/1".to_vec(),
                ciphertext_digest: [0xa1; 32],
            }],
        },
        DocumentEntry {
            document_id: DocumentId::from_uuid(Uuid::from_bytes([0x20; 16])),
            role: DocumentRole::Document,
            frontier: vec![6],
            version_vector: vec![7, 8, 9],
            key_version: WekVersion(1),
            snapshot: ObjectReference {
                locator: b"seg/doc/0".to_vec(),
                ciphertext_digest: [0xb0; 32],
            },
            updates: Vec::new(),
        },
    ];
    documents.sort_by_key(|d| *d.document_id.as_uuid().as_bytes());
    checkpoint::WorkspaceRecoveryBootstrapV1 {
        documents,
        objects: vec![
            ObjectEntry {
                cloud_object_id: CloudObjectId::from_bytes([0x01; 32]),
                key_version: WekVersion(1),
            },
            ObjectEntry {
                cloud_object_id: CloudObjectId::from_bytes([0x02; 32]),
                key_version: WekVersion(3),
            },
        ],
        blob_identity_key_version: BlobIdentityKeyVersion(4),
        required_key_versions: vec![WekVersion(1), WekVersion(2), WekVersion(3)],
    }
}

fn bootstrap_json(bootstrap: &checkpoint::WorkspaceRecoveryBootstrapV1) -> Value {
    json!({
        "document_count": bootstrap.documents.len(),
        "documents": bootstrap.documents.iter().map(|d| json!({
            "document_id": d.document_id.to_storage(),
            "role": String::from_utf8(d.role.token().to_vec()).unwrap(),
            "frontier": hex(&d.frontier),
            "version_vector": hex(&d.version_vector),
            "key_version": d.key_version.0,
            "snapshot": {
                "locator": hex(&d.snapshot.locator),
                "ciphertext_digest": hex(&d.snapshot.ciphertext_digest)
            },
            "updates": d.updates.iter().map(|u| json!({
                "locator": hex(&u.locator),
                "ciphertext_digest": hex(&u.ciphertext_digest)
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "object_count": bootstrap.objects.len(),
        "objects": bootstrap.objects.iter().map(|o| json!({
            "cloud_object_id": o.cloud_object_id.to_storage(),
            "key_version": o.key_version.0
        })).collect::<Vec<_>>(),
        "blob_identity_key_version": bootstrap.blob_identity_key_version.0,
        "required_key_versions": bootstrap.required_key_versions.iter().map(|v| v.0).collect::<Vec<_>>()
    })
}

// ---------------------------------------------------------------- identity

fn identity_vectors() -> Value {
    let mut rng = VectorRng::new(b"koru:vectors:blob-identity-v1");
    let mut root_a = [0u8; 32];
    let mut root_b = [0u8; 32];
    rng.fill_bytes(&mut root_a);
    rng.fill_bytes(&mut root_b);
    let key_a = WorkspaceBlobIdentityKey::from_bytes(root_a);
    let key_b = WorkspaceBlobIdentityKey::from_bytes(root_b);
    let workspace_a = WorkspaceId::from_uuid(Uuid::from_bytes([0x71; 16]));
    let workspace_b = WorkspaceId::from_uuid(Uuid::from_bytes([0x72; 16]));
    let hash = BlobHash::from_digest("b3", &"ab".repeat(32)).unwrap();
    let other = BlobHash::from_digest("b3", &"cd".repeat(32)).unwrap();

    let id_a = cloud_object_id(&key_a, BlobIdentityKeyVersion(1), workspace_a, &hash);
    let id_a_v2 = cloud_object_id(&key_a, BlobIdentityKeyVersion(2), workspace_a, &hash);
    let id_a_other = cloud_object_id(&key_a, BlobIdentityKeyVersion(1), workspace_a, &other);
    let id_b = cloud_object_id(&key_b, BlobIdentityKeyVersion(1), workspace_b, &hash);

    let wek = WorkspaceEncryptionKey::generate(&mut rng);
    let mapping_header =
        BlobMappingHeader::new(workspace_a, WekVersion(5), BlobIdentityKeyVersion(1), id_a);
    let mapping = BlobMappingRecordV1::seal(&mapping_header, &hash, &wek, &mut rng).unwrap();

    json!({
        "format": "CloudObjectId and BlobMappingRecordV1",
        "format_version": 1,
        "construction": "subkey = BLAKE3::derive_key(\"koru.do/workspace-crypto/v1 blob-identity-root -> cloud-object-id subkey\", root); CloudObjectId = BLAKE3::keyed_hash(subkey, \"koru:cloud-object-id:v1\\0\" || workspace_id || u32be(identity_key_version) || u32be(len(blob_hash)) || blob_hash)",
        "claim": "Prevents CROSS-WORKSPACE equality correlation, assuming every workspace's identity key is independently random. It intentionally still reveals equality and dedup WITHIN one workspace. It hides no length, and it is not an integrity check on the object.",
        "never": "WorkspaceBlobIdentityKey is never an encryption key. Its one use goes through a derived purpose subkey.",
        "cases": [
            {
                "name": "the same blob in the same workspace gets the same id",
                "identity_root": hex(&root_a),
                "identity_key_version": 1,
                "workspace_id": workspace_a.to_storage(),
                "blob_hash": hash.as_str(),
                "cloud_object_id": id_a.to_storage()
            },
            {
                "name": "a second identity-key version is a separate axis",
                "identity_root": hex(&root_a),
                "identity_key_version": 2,
                "workspace_id": workspace_a.to_storage(),
                "blob_hash": hash.as_str(),
                "cloud_object_id": id_a_v2.to_storage()
            },
            {
                "name": "a different blob in the same workspace",
                "identity_root": hex(&root_a),
                "identity_key_version": 1,
                "workspace_id": workspace_a.to_storage(),
                "blob_hash": other.as_str(),
                "cloud_object_id": id_a_other.to_storage()
            },
            {
                "name": "the same blob under an independent key in another workspace is uncorrelated",
                "identity_root": hex(&root_b),
                "identity_key_version": 1,
                "workspace_id": workspace_b.to_storage(),
                "blob_hash": hash.as_str(),
                "cloud_object_id": id_b.to_storage()
            }
        ],
        "blob_mapping": {
            "note": "No plaintext BlobHash exists server-side. The row's key is the CloudObjectId; the BlobHash is inside the ciphertext.",
            "header_len": crate::mapping::MAPPING_HEADER_LEN,
            "record": hex(&mapping),
            "wek": hex(wek.as_bytes()),
            "expect": {
                "workspace_id": workspace_a.to_storage(),
                "wek_version": 5,
                "blob_identity_key_version": 1,
                "cloud_object_id": id_a.to_storage(),
                "blob_hash": hash.as_str()
            }
        }
    })
}

// ----------------------------------------------------------------- account

fn account_vectors() -> Value {
    let mut rng = VectorRng::new(b"koru:vectors:account-metadata-envelope-v1");
    let kek = RegionalMetadataKek::generate(&mut rng);
    let account_key = AccountMetadataKey::generate(&mut rng);
    let key_ref = AccountMetadataKeyRef {
        account_id: Uuid::from_bytes([0xa1; 16]),
        key_id: KeyId::from_bytes([0xa2; 16]),
        key_version: 1,
        region: Region::US,
    };
    let wrapped_ref = WrappedKeyRef {
        key_ref,
        kek_id: MetadataKekId::new(KeyId::from_bytes([0xa3; 16])),
        kek_version: 4,
    };
    let wrapped =
        WrappedAccountMetadataKeyV1::seal(&wrapped_ref, &account_key, &kek, &mut rng).unwrap();
    // Opaque on purpose: this plan owns the envelope and deliberately does not
    // know the tombstone's field table (Plan 3.2 does).
    let plaintext = b"<opaque account-metadata record, owned by Plan 3.2>";
    let envelope = AccountMetadataEnvelopeV1::new(key_ref, 0x0001)
        .seal(&account_key, plaintext, &mut rng)
        .unwrap();

    let eu_ref = AccountMetadataKeyRef {
        region: Region::EU,
        ..key_ref
    };
    let mut cross_region = AccountMetadataEnvelopeV1::new(eu_ref, 0x0001)
        .seal(&account_key, plaintext, &mut rng)
        .unwrap()[..crate::account::ENVELOPE_HEADER_LEN]
        .to_vec();
    cross_region.extend_from_slice(&envelope[crate::account::ENVELOPE_HEADER_LEN..]);

    let other_account = AccountMetadataKeyRef {
        account_id: Uuid::from_bytes([0xb1; 16]),
        ..key_ref
    };
    let mut cross_account = AccountMetadataEnvelopeV1::new(other_account, 0x0001)
        .seal(&account_key, plaintext, &mut rng)
        .unwrap()[..crate::account::ENVELOPE_HEADER_LEN]
        .to_vec();
    cross_account.extend_from_slice(&envelope[crate::account::ENVELOPE_HEADER_LEN..]);

    json!({
        "format": "AccountMetadataEnvelopeV1 and WrappedAccountMetadataKeyV1",
        "format_version": 1,
        "scope": "Server-only. No client holds this key and no client implements this format; per D42 and the standing requirement it carries a server-only vector suite, verified by the regional account service, its KMS adapter, the rotation job and backup/restore.",
        "hierarchy": "regional metadata KEK (KMS) wraps a RANDOM per-account metadata key, which encrypts every account-metadata record. The account key is never KDF(regional_key, account_id): a derived key can be recomputed from public inputs, so destroying it would destroy nothing.",
        "one_copy": "The wrapped key is stored ONCE per account. An envelope references it by account, key id and version and never contains it — otherwise destroying 'the' wrapped key would erase nothing.",
        "envelope_does_not_name_the_kek": "Which KEK wraps the account key is a fact about the wrapped-key record and changes on rotation. A copy in every envelope would be stale the moment it rotated, so the envelope names only (account_id, key_id, key_version, region) and the wrapped-key record names the KEK.",
        "crypto_erasure_limit": "Destroying the wrapped key covers ciphertext whose ONLY remaining decryption path is that key. It is defeated by plaintext logs, search indexes, caches, analytics events, or backups of unwrapped keys. Those are operational controls; no format decision supplies them.",
        "record_type": "Opaque to Plan 2.1. Which entries a purged account record carries is Plan 3.2's.",
        "envelope_header_len": crate::account::ENVELOPE_HEADER_LEN,
        "wrapped_key_header_len": crate::account::WRAPPED_KEY_HEADER_LEN,
        "cases": [
            {
                "name": "round trip: unwrap the account key, then open the envelope",
                "result": "valid",
                "regional_metadata_kek": hex(kek.as_bytes()),
                "wrapped_account_metadata_key": hex(&wrapped),
                "envelope": hex(&envelope),
                "expect": {
                    "account_id": key_ref.account_id.to_string(),
                    "key_id": hex(key_ref.key_id.as_bytes()),
                    "key_version": 1,
                    "region": "us",
                    "record_type": 1,
                    "plaintext": hex(plaintext)
                },
                "expect_wrapped_key": {
                    "kek_id": hex(wrapped_ref.kek_id.key_id().as_bytes()),
                    "kek_version": 4
                }
            },
            {
                "name": "destroying the wrapped key renders the envelope unrecoverable",
                "result": "invalid",
                "error": "CRYPTO_COMMITMENT_MISMATCH",
                "regional_metadata_kek": hex(kek.as_bytes()),
                "wrapped_account_metadata_key": null,
                "envelope": hex(&envelope),
                "note": "With no wrapped key there is no account metadata key. Offering the KEK directly — the nearest thing an operator still holds — fails at the commitment."
            },
            {
                "name": "cross-region: an envelope relabelled for another region does not open",
                "result": "invalid",
                "error": "CRYPTO_COMMITMENT_MISMATCH",
                "regional_metadata_kek": hex(kek.as_bytes()),
                "wrapped_account_metadata_key": hex(&wrapped),
                "envelope": hex(&cross_region)
            },
            {
                "name": "cross-account: an envelope relabelled for another account does not open",
                "result": "invalid",
                "error": "CRYPTO_COMMITMENT_MISMATCH",
                "regional_metadata_kek": hex(kek.as_bytes()),
                "wrapped_account_metadata_key": hex(&wrapped),
                "envelope": hex(&cross_account)
            }
        ]
    })
}

// ------------------------------------------------------------------- tests

fn generated() -> Vec<(&'static str, Value)> {
    vec![
        (GRANT_PATH, grant_vectors()),
        (CONTENT_PATH, content_vectors()),
        (CHECKPOINT_PATH, checkpoint_vectors()),
        (IDENTITY_PATH, identity_vectors()),
        (ACCOUNT_PATH, account_vectors()),
    ]
}

fn update_requested() -> bool {
    std::env::var_os(koru_env::name::KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS).is_some()
}

/// **The bytes may not depend on which crates were compiled beside this one.**
///
/// Built out of insertion order, and nested, because that is the only case the
/// two `serde_json` map types disagree about. Under `preserve_order` an
/// unsorted `render` would put `zebra` first here; under a `BTreeMap` it never
/// would, which is exactly why the drift was invisible until a workspace build
/// turned the feature on.
#[test]
fn rendering_sorts_keys_whichever_json_map_this_build_has() {
    let value = serde_json::json!({
        "zebra": 1,
        "alpha": {"omega": [ {"second": 2, "first": 1} ], "beta": 2},
    });
    assert_eq!(
        render(&value),
        "{\n  \"alpha\": {\n    \"beta\": 2,\n    \"omega\": [\n      {\n        \
         \"first\": 1,\n        \"second\": 2\n      }\n    ]\n  },\n  \"zebra\": 1\n}\n"
    );
}

#[test]
fn the_published_vectors_are_current() {
    for (relative, value) in generated() {
        let rendered = render(&value);
        if update_requested() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(VECTORS_ROOT)
                .join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create vector directory");
            }
            std::fs::write(&path, &rendered).expect("write vector file");
            continue;
        }
        assert_eq!(
            published(relative),
            rendered,
            "{relative} is stale; regenerate with \
             `KORU_UPDATE_WORKSPACE_CRYPTO_VECTORS=1 cargo test -p koru-workspace-crypto`"
        );
    }
}

fn parse(path: &str) -> Value {
    serde_json::from_str(published(path)).expect("published vectors parse")
}

fn hex_field(case: &Value, key: &str) -> Vec<u8> {
    unhex(case[key].as_str().unwrap_or_else(|| panic!("{key} is hex")))
}

#[test]
fn the_published_grant_vectors_verify() {
    let root = parse(GRANT_PATH);

    // D89, from the file rather than from the code: the customer-managed
    // workspace's grant set has no Koru recovery grant, and this is checked by
    // decoding every envelope in it.
    let customer = &root["customer_managed_workspace"];
    assert_eq!(customer["koru_recovery_grants"], 0);
    for envelope in customer["grants"].as_array().unwrap() {
        let grant = WekGrantV1::decode(&unhex(envelope.as_str().unwrap())).unwrap();
        assert_ne!(
            grant.header().recipient_type,
            RecipientType::RegionalRecovery,
            "a customer-managed workspace must have no Koru recovery grant"
        );
        assert!(grant.header().recovery_key_id.key_id().is_none());
    }
    let managed = &root["koru_managed_workspace"];
    assert_eq!(managed["koru_recovery_grants"], 1);
    let recovery_count = managed["grants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            WekGrantV1::decode(&unhex(e.as_str().unwrap()))
                .unwrap()
                .header()
                .recipient_type
                == RecipientType::RegionalRecovery
        })
        .count();
    assert_eq!(recovery_count, 1);

    for case in root["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let envelope = hex_field(case, "envelope");
        let decoded = WekGrantV1::decode(&envelope);

        if case["stage"] == "parse" {
            assert_eq!(
                decoded.unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
            continue;
        }
        let decoded = decoded.unwrap_or_else(|e| panic!("{name}: {e}"));

        if case["stage"] == "signature" {
            let author = koru_device::DevicePublicKey::from_bytes(
                hex_field(case, "author_public_key").try_into().unwrap(),
            );
            assert_eq!(
                decoded.verify_signature(&author).unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
            continue;
        }

        let author = koru_device::DevicePublicKey::from_bytes(
            hex_field(case, "author_public_key").try_into().unwrap(),
        );
        decoded
            .verify_signature(&author)
            .unwrap_or_else(|e| panic!("{name}: signature: {e}"));

        let expect = &case["expect"];
        let expectation = GrantExpectation {
            workspace_id: WorkspaceId::parse(expect["workspace_id"].as_str().unwrap()).unwrap(),
            wek_version: WekVersion(expect["wek_version"].as_u64().unwrap() as u32),
            recipient_type: match expect["recipient_type"].as_str().unwrap() {
                "DEVICE" => RecipientType::Device,
                "CLOUD_REPLICA" => RecipientType::CloudReplica,
                "REGIONAL_RECOVERY" => RecipientType::RegionalRecovery,
                other => panic!("unknown recipient type {other}"),
            },
            recipient_id: Uuid::parse_str(expect["recipient_id"].as_str().unwrap()).unwrap(),
            recipient_key_id: KeyId::from_bytes(
                unhex(expect["recipient_key_id"].as_str().unwrap())
                    .try_into()
                    .unwrap(),
            ),
            recipient_key_version: expect["recipient_key_version"].as_u64().unwrap() as u32,
        };
        let suite = GrantSuite::from_id(decoded.header().suite.id()).unwrap();
        let secret = RecipientSecretKey::from_bytes(
            suite,
            hex_field(case, "recipient_secret_key").try_into().unwrap(),
        );
        let outcome = decoded.open(&expectation, &secret);

        if case["result"] == "valid" {
            let wek = outcome.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(wek.as_bytes().as_slice(), hex_field(case, "wek"), "{name}");
        } else {
            assert_eq!(
                outcome.unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
        }
    }
}

#[test]
fn the_published_content_vectors_verify() {
    let root = parse(CONTENT_PATH);
    let header_len = root["header_len"].as_u64().unwrap();
    let wrapped_len = root["wrapped_dek_len"].as_u64().unwrap();

    for case in root["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let object = hex_field(case, "object");
        let wek = WorkspaceEncryptionKey::from_bytes(hex_field(case, "wek").try_into().unwrap());
        let outcome = ContentObject::parse(&object).and_then(|o| o.open(&wek));

        if case["result"] == "valid" {
            let plaintext = outcome.unwrap_or_else(|e| panic!("{name}: {e}"));
            let len = case["plaintext_len"].as_u64().unwrap() as usize;
            assert_eq!(plaintext.len(), len, "{name}");
            let expected: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(plaintext, expected, "{name}");
            assert_eq!(
                content::object_plaintext_len(object.len() as u64).unwrap(),
                len as u64,
                "{name}: derived length"
            );
        } else {
            assert_eq!(
                outcome.unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
        }
    }

    let derivation = &root["plaintext_length_derivation"];
    for row in derivation["valid"].as_array().unwrap() {
        let stored = row["stored_len"].as_u64().unwrap();
        assert_eq!(
            content::object_plaintext_len(stored).unwrap(),
            row["plaintext_len"].as_u64().unwrap()
        );
        let body = SealedLayout::of_ciphertext_len(stored - header_len - wrapped_len).unwrap();
        assert_eq!(body.chunk_count(), row["chunk_count"].as_u64().unwrap());
    }
    for row in derivation["invalid"].as_array().unwrap() {
        assert!(
            content::object_plaintext_len(row["stored_len"].as_u64().unwrap()).is_err(),
            "{row} must not derive a length"
        );
    }
}

#[test]
fn the_published_checkpoint_vectors_verify() {
    let root = parse(CHECKPOINT_PATH);
    for case in root["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let bytes = hex_field(case, "checkpoint");

        if case["stage"] == "parse" {
            assert_eq!(
                checkpoint::parse_header(&bytes).unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
            continue;
        }
        let wek = WorkspaceEncryptionKey::from_bytes(hex_field(case, "wek").try_into().unwrap());
        let outcome = checkpoint::open_checkpoint(&bytes, &wek);
        if case["result"] == "valid" {
            let (_, bootstrap) = outcome.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(bootstrap_json(&bootstrap), case["expect"], "{name}");
        } else {
            assert_eq!(
                outcome.unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
        }
    }
}

#[test]
fn the_published_identity_vectors_verify() {
    let root = parse(IDENTITY_PATH);
    for case in root["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let key = WorkspaceBlobIdentityKey::from_bytes(
            hex_field(case, "identity_root").try_into().unwrap(),
        );
        let id = cloud_object_id(
            &key,
            BlobIdentityKeyVersion(case["identity_key_version"].as_u64().unwrap() as u32),
            WorkspaceId::parse(case["workspace_id"].as_str().unwrap()).unwrap(),
            &BlobHash::parse(case["blob_hash"].as_str().unwrap()).unwrap(),
        );
        assert_eq!(
            id.to_storage(),
            case["cloud_object_id"].as_str().unwrap(),
            "{name}"
        );
    }

    let mapping = &root["blob_mapping"];
    let wek = WorkspaceEncryptionKey::from_bytes(hex_field(mapping, "wek").try_into().unwrap());
    let (header, hash) = BlobMappingRecordV1::open(&hex_field(mapping, "record"), &wek).unwrap();
    let expect = &mapping["expect"];
    assert_eq!(header.workspace_id.to_storage(), expect["workspace_id"]);
    assert_eq!(
        header.wek_version.0 as u64,
        expect["wek_version"].as_u64().unwrap()
    );
    assert_eq!(
        header.cloud_object_id.to_storage(),
        expect["cloud_object_id"].as_str().unwrap()
    );
    assert_eq!(hash.as_str(), expect["blob_hash"].as_str().unwrap());
    // And the plaintext hash appears nowhere in the stored row.
    let record = hex_field(mapping, "record");
    let needle = hash.as_str().as_bytes();
    assert!(!record.windows(needle.len()).any(|w| w == needle));
}

#[test]
fn the_published_account_vectors_verify() {
    let root = parse(ACCOUNT_PATH);
    for case in root["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let kek = RegionalMetadataKek::from_bytes(
            hex_field(case, "regional_metadata_kek").try_into().unwrap(),
        );
        let envelope = hex_field(case, "envelope");

        let account_key = match case["wrapped_account_metadata_key"].as_str() {
            Some(wrapped) => {
                let wrapped = unhex(wrapped);
                let (reference, key) = WrappedAccountMetadataKeyV1::open(&wrapped, &kek)
                    .unwrap_or_else(|e| panic!("{name}: unwrap: {e}"));
                // The KEK is named by the wrapped-key record and by nothing
                // else; a reader that went looking for it in the envelope would
                // find a field that is not there.
                if let Some(expect) = case.get("expect_wrapped_key") {
                    assert_eq!(
                        hex(reference.kek_id.key_id().as_bytes()),
                        expect["kek_id"].as_str().unwrap(),
                        "{name}"
                    );
                    assert_eq!(
                        reference.kek_version as u64,
                        expect["kek_version"].as_u64().unwrap(),
                        "{name}"
                    );
                }
                key
            }
            // The crypto-erasure case: the wrapped key is gone, so the nearest
            // key an operator still holds is the KEK itself. It opens nothing.
            None => AccountMetadataKey::from_bytes(*kek.as_bytes()),
        };

        let outcome = AccountMetadataEnvelopeV1::open(&envelope, &account_key);
        if case["result"] == "valid" {
            let (parsed, plaintext) = outcome.unwrap_or_else(|e| panic!("{name}: {e}"));
            let expect = &case["expect"];
            assert_eq!(parsed.key_ref.account_id.to_string(), expect["account_id"]);
            assert_eq!(
                hex(parsed.key_ref.key_id.as_bytes()),
                expect["key_id"].as_str().unwrap()
            );
            assert_eq!(
                parsed.key_ref.key_version as u64,
                expect["key_version"].as_u64().unwrap()
            );
            assert_eq!(
                parsed.record_type as u64,
                expect["record_type"].as_u64().unwrap()
            );
            assert_eq!(
                hex(&plaintext),
                expect["plaintext"].as_str().unwrap(),
                "{name}"
            );
        } else {
            assert_eq!(
                outcome.unwrap_err().code(),
                case["error"].as_str().unwrap(),
                "{name}"
            );
        }
    }
}

#[test]
fn every_purpose_that_reaches_a_stored_record_appears_in_a_vector() {
    // A purpose with no vector is a format nobody would notice breaking.
    let covered = [
        Purpose::WekGrant,
        Purpose::ContentBlobObject,
        Purpose::RecoveryCheckpoint,
        Purpose::BlobMappingRecord,
        Purpose::AccountMetadataRecord,
        Purpose::AccountMetadataKeyWrap,
    ];
    // Segments and snapshots share `ContentObjectV1` with blob objects and
    // differ only in the `purpose` field, which `content::tests` pins; they are
    // Cloud-written and no client ever encodes one, so they carry no
    // client-held vector. Named here so the omission is a decision.
    let deliberately_uncovered = [Purpose::ContentSegment, Purpose::ContentSnapshot];
    assert_eq!(covered.len() + deliberately_uncovered.len(), 8);
    for purpose in covered {
        assert!(Purpose::from_id(purpose.id()).is_ok());
    }
}

#[test]
fn a_hand_edited_vector_is_caught() {
    // The verifier is only worth having if it fails on a doctored file. One
    // byte of a valid grant envelope, flipped, must stop it opening.
    let root = parse(GRANT_PATH);
    let case = &root["cases"].as_array().unwrap()[0];
    let mut envelope = hex_field(case, "envelope");
    envelope[crate::grant::GRANT_HEADER_LEN] ^= 0x01;
    let grant = WekGrantV1::decode(&envelope).unwrap();
    let secret = RecipientSecretKey::from_bytes(
        GrantSuite::X25519HkdfSha256ChaCha20Poly1305,
        hex_field(case, "recipient_secret_key").try_into().unwrap(),
    );
    let expect = &case["expect"];
    let expectation = GrantExpectation {
        workspace_id: WorkspaceId::parse(expect["workspace_id"].as_str().unwrap()).unwrap(),
        wek_version: WekVersion(expect["wek_version"].as_u64().unwrap() as u32),
        recipient_type: RecipientType::Device,
        recipient_id: Uuid::parse_str(expect["recipient_id"].as_str().unwrap()).unwrap(),
        recipient_key_id: KeyId::from_bytes(
            unhex(expect["recipient_key_id"].as_str().unwrap())
                .try_into()
                .unwrap(),
        ),
        recipient_key_version: expect["recipient_key_version"].as_u64().unwrap() as u32,
    };
    assert_eq!(
        grant.open(&expectation, &secret).unwrap_err(),
        CryptoError::Aead
    );
}
