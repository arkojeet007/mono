// SPDX-License-Identifier: MPL-2.0

//! Layer 1, against RFC 9180's own test vectors and against D91's hostile list.

use koru_device::DeviceSigningKey;
use koru_store_core::ids::WorkspaceId;
use rand_core::Rng as _;
use uuid::Uuid;

use super::*;
use crate::testkit::{VectorRng, unhex};

// ------------------------------------------------------- RFC 9180 vectors

/// RFC 9180 appendix A.2.1 — DHKEM(X25519, HKDF-SHA256), HKDF-SHA256,
/// ChaCha20Poly1305, Base mode, sequence number 0.
///
/// This is not a test of Koru's envelope; it is a test that the primitives the
/// envelope is built from are the ones the ADR names. If `hpke` were wired to a
/// different KEM, KDF or AEAD than the suite claims, this is where it would
/// show — and it would show as one clear failure rather than as every grant
/// vector being mysteriously wrong.
#[test]
fn rfc_9180_a_2_1_base_setup_opens() {
    let sk_rm = unhex("8057991eef8f1f1af18f4a9491d16a1ce333f695d4db8e38da75975c4478e0fb");
    let enc = unhex("1afa08d3dec047a643885163f1180476fa7ddb54c6a8029ea33f95796bf2ac4a");
    let info = unhex("4f6465206f6e2061204772656369616e2055726e");
    let aad = unhex("436f756e742d30");
    let ct = unhex(
        "1c5250d8034ec2b784ba2cfd69dbdb8af406cfe3ff938e131f0def8c8b60b4db\
         21993c62ce81883d2dd1b51a28",
    );
    let pt = unhex("4265617574792069732074727574682c20747275746820626561757479");

    let sk = <X25519HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&sk_rm).expect("skRm");
    let enc = <X25519HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&enc).expect("enc");
    let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, X25519HkdfSha256>(
        &OpModeR::Base,
        &sk,
        &enc,
        &info,
        &ct,
        &aad,
    )
    .expect("RFC 9180 A.2.1 must open");
    assert_eq!(opened, pt);
}

/// RFC 9180 appendix A.5.1 — the same mode, KDF and AEAD over
/// DHKEM(P-256, HKDF-SHA256). The recovery suite of D88, pinned to the RFC for
/// exactly the same reason.
#[test]
fn rfc_9180_a_5_1_base_setup_opens() {
    let sk_rm = unhex("a4d1c55836aa30f9b3fbb6ac98d338c877c2867dd3a77396d13f68d3ab150d3b");
    let enc = unhex(
        "04c07836a0206e04e31d8ae99bfd549380b072a1b1b82e563c935c0958278\
         24fc1559eac6fb9e3c70cd3193968994e7fe9781aa103f5b50e934b5b2f387e381291",
    );
    let info = unhex("4f6465206f6e2061204772656369616e2055726e");
    let aad = unhex("436f756e742d30");
    let ct = unhex(
        "6469c41c5c81d3aa85432531ecf6460ec945bde1eb428cb2fedf7a29f5a685b4\
         ccb0d057f03ea2952a27bb458b",
    );
    let pt = unhex("4265617574792069732074727574682c20747275746820626561757479");

    let sk = <DhP256HkdfSha256 as KemTrait>::PrivateKey::from_bytes(&sk_rm).expect("skRm");
    let enc = <DhP256HkdfSha256 as KemTrait>::EncappedKey::from_bytes(&enc).expect("enc");
    let opened = hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, DhP256HkdfSha256>(
        &OpModeR::Base,
        &sk,
        &enc,
        &info,
        &ct,
        &aad,
    )
    .expect("RFC 9180 A.5.1 must open");
    assert_eq!(opened, pt);
}

// ------------------------------------------------------------------ fixtures

struct Fixture {
    workspace: WorkspaceId,
    wek: WorkspaceEncryptionKey,
    device_secret: RecipientSecretKey,
    device: Recipient,
    recovery_secret: RecipientSecretKey,
    custody: WorkspaceCustody,
    author: DeviceSigningKey,
    author_id: Uuid,
    rng: VectorRng,
}

fn fixture(recovery_suite: GrantSuite) -> Fixture {
    let mut rng = VectorRng::new(b"koru:grant-tests");
    let workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x11; 16]));
    let wek = WorkspaceEncryptionKey::generate(&mut rng);

    let mut device_bytes = [0u8; 32];
    rng.fill_bytes(&mut device_bytes);
    let device_secret =
        RecipientSecretKey::from_bytes(GrantSuite::X25519HkdfSha256ChaCha20Poly1305, device_bytes);
    let device = Recipient {
        recipient_type: RecipientType::Device,
        recipient_id: Uuid::from_bytes([0x22; 16]),
        recipient_key_id: KeyId::from_bytes([0x33; 16]),
        recipient_key_version: 1,
        public_key: device_secret.public_key().expect("device public key"),
    };

    // A P-256 scalar drawn uniformly can be out of range; redraw rather than
    // reduce, which is the shape of RFC 9180 §7.1.3's rejection sampling.
    let recovery_secret = loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        let candidate = RecipientSecretKey::from_bytes(recovery_suite, bytes);
        if candidate.public_key().is_ok() {
            break candidate;
        }
    };
    let custody = WorkspaceCustody::koru_managed(
        RecoveryKeyId::new(KeyId::from_bytes([0x44; 16])),
        Uuid::from_bytes([0x55; 16]),
        KeyId::from_bytes([0x66; 16]),
        7,
        recovery_secret.public_key().expect("recovery public key"),
    )
    .expect("custody");

    Fixture {
        workspace,
        wek,
        device_secret,
        device,
        recovery_secret,
        custody,
        author: DeviceSigningKey::from_secret_bytes([0x77; 32]),
        author_id: Uuid::from_bytes([0x88; 16]),
        rng,
    }
}

impl Fixture {
    fn issue(&mut self, custody: &WorkspaceCustody) -> GrantSet {
        GrantSet::issue(
            custody,
            &GrantIssuance {
                workspace_id: self.workspace,
                wek_version: WekVersion(3),
                author_type: AuthorType::Device,
                author_id: self.author_id,
            },
            &self.wek,
            std::slice::from_ref(&self.device),
            &self.author,
            &mut self.rng,
        )
        .expect("issue")
    }

    fn device_expectation(&self) -> GrantExpectation {
        GrantExpectation {
            workspace_id: self.workspace,
            wek_version: WekVersion(3),
            recipient_type: RecipientType::Device,
            recipient_id: self.device.recipient_id,
            recipient_key_id: self.device.recipient_key_id,
            recipient_key_version: self.device.recipient_key_version,
        }
    }

    fn recovery_expectation(&self) -> GrantExpectation {
        GrantExpectation {
            workspace_id: self.workspace,
            wek_version: WekVersion(3),
            recipient_type: RecipientType::RegionalRecovery,
            recipient_id: Uuid::from_bytes([0x55; 16]),
            recipient_key_id: KeyId::from_bytes([0x66; 16]),
            recipient_key_version: 7,
        }
    }
}

// --------------------------------------------------------------- round trip

#[test]
fn a_device_grant_round_trips_through_its_encoding() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let grant = set.grants()[0].clone();

    let bytes = grant.encode();
    assert_eq!(
        bytes.len(),
        GRANT_HEADER_LEN + 32 + GRANT_CIPHERTEXT_LEN + GRANT_SIGNATURE_LEN
    );
    let decoded = WekGrantV1::decode(&bytes).expect("decode");
    assert_eq!(decoded, grant);

    decoded
        .verify_signature(&f.author.public_key())
        .expect("signature");
    let opened = decoded
        .open(&f.device_expectation(), &f.device_secret)
        .expect("open");
    assert_eq!(opened.as_bytes(), f.wek.as_bytes());
}

#[test]
fn a_recovery_grant_may_use_a_different_kem_from_the_devices() {
    // D88: the recovery grant's KEM is a per-grant property. Here the devices
    // are on X25519 and the recovery authority is on P-256, in one workspace,
    // for one WEK version.
    let mut f = fixture(GrantSuite::P256HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);

    assert_eq!(
        set.grants()[0].header().suite,
        GrantSuite::X25519HkdfSha256ChaCha20Poly1305
    );
    let recovery = set.koru_recovery_grant().expect("recovery grant").clone();
    assert_eq!(
        recovery.header().suite,
        GrantSuite::P256HkdfSha256ChaCha20Poly1305
    );
    assert_eq!(recovery.encode().len(), GRANT_HEADER_LEN + 65 + 48 + 64);

    let opened = recovery
        .open(&f.recovery_expectation(), &f.recovery_secret)
        .expect("recovery open");
    assert_eq!(opened.as_bytes(), f.wek.as_bytes());
}

// -------------------------------------------------------- D89, the invariant

#[test]
fn a_customer_managed_workspace_has_no_koru_recovery_grant() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let set = f.issue(&WorkspaceCustody::CustomerManaged);

    assert!(!set.has_koru_recovery_grant());
    assert!(set.koru_recovery_grant().is_none());
    assert_eq!(set.grants().len(), 1);
    for grant in set.grants() {
        assert_ne!(
            grant.header().recipient_type,
            RecipientType::RegionalRecovery
        );
        // And the field that names a recovery authority is NONE, as it is on
        // every non-recovery grant of every workspace — so it promises nothing
        // about custody either way.
        assert!(grant.header().recovery_key_id.key_id().is_none());
    }
}

#[test]
fn a_recovery_recipient_cannot_be_smuggled_in_as_an_ordinary_one() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let smuggled = Recipient {
        recipient_type: RecipientType::RegionalRecovery,
        ..f.device.clone()
    };
    let workspace = f.workspace;
    let author_id = f.author_id;
    let outcome = GrantSet::issue(
        &WorkspaceCustody::CustomerManaged,
        &GrantIssuance {
            workspace_id: workspace,
            wek_version: WekVersion(3),
            author_type: AuthorType::Device,
            author_id,
        },
        &f.wek,
        &[smuggled],
        &f.author,
        &mut f.rng,
    );
    assert_eq!(outcome.unwrap_err(), CryptoError::Forbidden);
}

#[test]
fn a_grant_that_claims_recovery_without_a_recovery_key_id_is_refused() {
    // The structural invariant, checked on both the writing and the reading
    // side: `recipient_type == RegionalRecovery` if and only if
    // `recovery_key_id != NONE`.
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let fields = GrantFields {
        recipient_type: RecipientType::RegionalRecovery,
        author_type: AuthorType::Device,
        workspace_id: f.workspace,
        wek_version: WekVersion(3),
        recipient_id: f.device.recipient_id,
        recipient_key_id: f.device.recipient_key_id,
        recipient_key_version: 1,
        recovery_key_id: RecoveryKeyId::new(KeyId::NONE),
        author_id: f.author_id,
    };
    let public_key = f.device.public_key.clone();
    let outcome = WekGrantV1::seal(fields, &public_key, &f.wek, &f.author, &mut f.rng);
    assert_eq!(outcome.unwrap_err(), CryptoError::ContextMismatch);
}

// --------------------------------------------------- D91, the hostile cases

/// Every byte of the header, flipped one at a time. The grant must refuse —
/// at the parse, at the expectation, at the signature or at the AEAD — and
/// never open.
#[test]
fn no_single_header_byte_may_be_changed() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let bytes = set.grants()[0].encode();
    let expectation = f.device_expectation();
    let author = f.author.public_key();

    for index in 0..GRANT_HEADER_LEN {
        for bit in [0x01u8, 0x80] {
            let mut tampered = bytes.clone();
            tampered[index] ^= bit;
            let outcome = WekGrantV1::decode(&tampered).and_then(|grant| {
                grant.verify_signature(&author)?;
                grant.open(&expectation, &f.device_secret)
            });
            assert!(
                outcome.is_err(),
                "byte {index} bit {bit:#x} of the header opened and must not have"
            );
        }
    }
}

#[test]
fn no_single_byte_of_the_encapsulation_or_ciphertext_may_be_changed() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let bytes = set.grants()[0].encode();
    let expectation = f.device_expectation();
    let author = f.author.public_key();

    for index in GRANT_HEADER_LEN..bytes.len() {
        let mut tampered = bytes.clone();
        tampered[index] ^= 0x01;
        let outcome = WekGrantV1::decode(&tampered).and_then(|grant| {
            grant.verify_signature(&author)?;
            grant.open(&expectation, &f.device_secret)
        });
        assert!(outcome.is_err(), "byte {index} opened and must not have");
    }
}

#[test]
fn a_grant_for_another_workspace_is_refused_before_decryption() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let mut expectation = f.device_expectation();
    expectation.workspace_id = WorkspaceId::from_uuid(Uuid::from_bytes([0x99; 16]));
    assert_eq!(
        set.grants()[0]
            .open(&expectation, &f.device_secret)
            .unwrap_err(),
        CryptoError::ContextMismatch
    );
}

#[test]
fn a_grant_for_another_recipient_is_refused() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);

    // Wrong recipient id, right key: refused on the expectation.
    let mut expectation = f.device_expectation();
    expectation.recipient_id = Uuid::from_bytes([0xaa; 16]);
    assert_eq!(
        set.grants()[0]
            .open(&expectation, &f.device_secret)
            .unwrap_err(),
        CryptoError::ContextMismatch
    );

    // Right expectation, wrong private key: refused by the KEM.
    let mut other = [0u8; 32];
    f.rng.fill_bytes(&mut other);
    let stranger =
        RecipientSecretKey::from_bytes(GrantSuite::X25519HkdfSha256ChaCha20Poly1305, other);
    assert_eq!(
        set.grants()[0]
            .open(&f.device_expectation(), &stranger)
            .unwrap_err(),
        CryptoError::Aead
    );
}

#[test]
fn a_modified_wek_version_is_refused() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let mut expectation = f.device_expectation();
    expectation.wek_version = WekVersion(4);
    assert_eq!(
        set.grants()[0]
            .open(&expectation, &f.device_secret)
            .unwrap_err(),
        CryptoError::ContextMismatch
    );
}

#[test]
fn a_recovery_grant_does_not_open_under_another_regions_recovery_key() {
    // "Cross-region recovery" needs no field asserting a region: the regional
    // key hierarchies are independent (D30/D7), so an EU recovery key is simply
    // a different key and the KEM refuses. This is that, made explicit.
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let recovery = set.koru_recovery_grant().expect("recovery grant").clone();

    let mut other_region = [0u8; 32];
    f.rng.fill_bytes(&mut other_region);
    let eu =
        RecipientSecretKey::from_bytes(GrantSuite::X25519HkdfSha256ChaCha20Poly1305, other_region);
    assert_eq!(
        recovery.open(&f.recovery_expectation(), &eu).unwrap_err(),
        CryptoError::Aead
    );
}

#[test]
fn an_unknown_suite_fails_closed() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let mut bytes = set.grants()[0].encode();
    bytes[2..4].copy_from_slice(&0xbeefu16.to_be_bytes());
    assert_eq!(
        WekGrantV1::decode(&bytes).unwrap_err(),
        CryptoError::UnsupportedSuite
    );
}

#[test]
fn an_unknown_format_version_is_told_apart_from_a_malformed_record() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let mut bytes = set.grants()[0].encode();
    bytes[0..2].copy_from_slice(&2u16.to_be_bytes());
    assert_eq!(
        WekGrantV1::decode(&bytes).unwrap_err(),
        CryptoError::UnsupportedFormatVersion
    );
}

#[test]
fn a_suite_outside_the_writer_policy_is_unselectable_for_a_new_write() {
    // P-256 is readable everywhere and writable only for the recovery
    // recipient. Offering a P-256 device key for a new device grant is the
    // shape of "a deprecated suite must not be selectable for new writes": the
    // writer refuses while `decode` goes on understanding the id.
    let mut f = fixture(GrantSuite::P256HkdfSha256ChaCha20Poly1305);
    let p256_device = f.recovery_secret.public_key().expect("public key");
    let fields = GrantFields {
        recipient_type: RecipientType::Device,
        author_type: AuthorType::Device,
        workspace_id: f.workspace,
        wek_version: WekVersion(3),
        recipient_id: f.device.recipient_id,
        recipient_key_id: f.device.recipient_key_id,
        recipient_key_version: 1,
        recovery_key_id: RecoveryKeyId::new(KeyId::NONE),
        author_id: f.author_id,
    };
    let outcome = WekGrantV1::seal(fields, &p256_device, &f.wek, &f.author, &mut f.rng);
    assert_eq!(outcome.unwrap_err(), CryptoError::DeprecatedSuiteForWrite);
}

#[test]
fn a_signature_from_another_device_is_refused() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let impostor = DeviceSigningKey::from_secret_bytes([0xee; 32]);
    assert_eq!(
        set.grants()[0]
            .verify_signature(&impostor.public_key())
            .unwrap_err(),
        CryptoError::Signature
    );
}

#[test]
fn a_truncated_or_extended_envelope_is_malformed() {
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let bytes = set.grants()[0].encode();

    for cut in 0..bytes.len() {
        assert!(
            WekGrantV1::decode(&bytes[..cut]).is_err(),
            "a {cut}-byte prefix must not parse"
        );
    }
    let mut extended = bytes.clone();
    extended.push(0);
    assert_eq!(
        WekGrantV1::decode(&extended).unwrap_err(),
        CryptoError::Malformed
    );
}

#[test]
fn a_replayed_grant_is_the_same_grant_and_the_cipher_does_not_say_so() {
    // RFC 9180 §9.7.3: HPKE provides no replay protection. Presenting a valid
    // grant a second time delivers the same key to the same recipient, and this
    // crate cannot and does not pretend otherwise. What refuses a stale grant is
    // Koru's epochs and versions — a revoked device leaves the trust view, a
    // superseded WEK version is not the current one — never the cipher.
    let mut f = fixture(GrantSuite::X25519HkdfSha256ChaCha20Poly1305);
    let custody = f.custody.clone();
    let set = f.issue(&custody);
    let first = set.grants()[0]
        .open(&f.device_expectation(), &f.device_secret)
        .expect("first");
    let second = set.grants()[0]
        .open(&f.device_expectation(), &f.device_secret)
        .expect("second");
    assert_eq!(first.as_bytes(), second.as_bytes());
}
