// SPDX-License-Identifier: MPL-2.0

//! **The guard on the boundary between the two layers.**
//!
//! D86 is a structural rule and structural rules rot quietly. The obvious way
//! to break this one is not a bad decision — it is somebody adding a
//! convenience: a helper on the content object that fetches its own grant, a
//! suite enum that covers both layers, a `WorkspaceKeys` struct that owns an
//! HPKE key and a WEK together. Each is one line and each makes an HPKE change
//! reach stored objects.
//!
//! So the boundary is asserted mechanically. `include_str!` reads the source at
//! **compile time**, so the default suite still touches no disk.

use crate::testkit::VectorRng;

/// Every module of layer 2 and every at-rest format, with its source.
const LAYER_TWO_SOURCES: &[(&str, &str)] = &[
    ("sealed.rs", include_str!("sealed.rs")),
    ("envelope.rs", include_str!("envelope.rs")),
    ("content.rs", include_str!("content.rs")),
    ("checkpoint.rs", include_str!("checkpoint.rs")),
    ("mapping.rs", include_str!("mapping.rs")),
    ("account.rs", include_str!("account.rs")),
    ("identity.rs", include_str!("identity.rs")),
];

/// A line that is only a doc comment or a comment. The boundary is about what
/// the code links, and the modules above talk about layer 1 constantly — that
/// is the point of them being documented.
fn is_prose(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with("*") || trimmed.starts_with("/*")
}

#[test]
fn no_bulk_content_format_reaches_layer_one() {
    // If any of these appears in executable code under layer 2, then changing
    // the HPKE suite has become a reason to rewrite stored objects, and the ADR
    // is rejected at its gate.
    const FORBIDDEN: &[&str] = &[
        "hpke",
        "crate::grant",
        "GrantSuite",
        "WekGrantV1",
        "X25519",
        "DhP256",
        "ChaCha20Poly1305",
        "RecipientPublicKey",
        "RecipientSecretKey",
    ];

    for (name, source) in LAYER_TWO_SOURCES {
        for (number, line) in source.lines().enumerate() {
            if is_prose(line) {
                continue;
            }
            for needle in FORBIDDEN {
                assert!(
                    !line.contains(needle),
                    "{name}:{} names `{needle}` in code. Layer 2 must not depend on \
                     layer 1: changing the HPKE suite must mean reissuing grants for \
                     the same WEK and re-encrypting nothing (D86).",
                    number + 1
                );
            }
        }
    }
}

#[test]
fn the_only_thing_that_crosses_the_boundary_is_a_workspace_encryption_key() {
    // Layer 1 hands over 32 bytes and knows nothing else about layer 2; layer 2
    // takes 32 bytes and knows nothing about where they came from. The proof is
    // that a grant's output is accepted by a content object with no adapter,
    // no conversion, and no shared type but the key itself.
    use crate::content::{ContentObject, ContentObjectHeader, seal_object};
    use crate::grant::{
        GrantExpectation, GrantIssuance, GrantSet, Recipient, RecipientSecretKey, WorkspaceCustody,
    };
    use crate::identity::CloudObjectId;
    use crate::keys::{KeyId, WekVersion, WorkspaceEncryptionKey};
    use crate::suites::{AuthorType, GrantSuite, RecipientType};
    use koru_device::DeviceSigningKey;
    use koru_store_core::ids::WorkspaceId;
    use rand_core::Rng as _;
    use uuid::Uuid;

    let mut rng = VectorRng::new(b"boundary");
    let workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x01; 16]));
    let wek = WorkspaceEncryptionKey::generate(&mut rng);

    // Layer 2 first, so that nothing about the grant can have influenced it.
    let header = ContentObjectHeader::for_blob(
        workspace,
        WekVersion(1),
        CloudObjectId::from_bytes([0x02; 32]),
    );
    let object = seal_object(&header, &wek, b"customer bytes", &mut rng).unwrap();

    // Layer 1, delivering the same key.
    let mut secret_bytes = [0u8; 32];
    rng.fill_bytes(&mut secret_bytes);
    let secret =
        RecipientSecretKey::from_bytes(GrantSuite::X25519HkdfSha256ChaCha20Poly1305, secret_bytes);
    let recipient = Recipient {
        recipient_type: RecipientType::Device,
        recipient_id: Uuid::from_bytes([0x03; 16]),
        recipient_key_id: KeyId::from_bytes([0x04; 16]),
        recipient_key_version: 1,
        public_key: secret.public_key().unwrap(),
    };
    let author = DeviceSigningKey::from_secret_bytes([0x05; 32]);
    let set = GrantSet::issue(
        &WorkspaceCustody::CustomerManaged,
        &GrantIssuance {
            workspace_id: workspace,
            wek_version: WekVersion(1),
            author_type: AuthorType::Device,
            author_id: Uuid::from_bytes([0x06; 16]),
        },
        &wek,
        std::slice::from_ref(&recipient),
        &author,
        &mut rng,
    )
    .unwrap();

    let delivered = set.grants()[0]
        .open(
            &GrantExpectation {
                workspace_id: workspace,
                wek_version: WekVersion(1),
                recipient_type: RecipientType::Device,
                recipient_id: recipient.recipient_id,
                recipient_key_id: recipient.recipient_key_id,
                recipient_key_version: 1,
            },
            &secret,
        )
        .unwrap();

    assert_eq!(
        ContentObject::parse(&object)
            .unwrap()
            .open(&delivered)
            .unwrap(),
        b"customer bytes"
    );
}

#[test]
fn changing_the_grant_suite_does_not_change_one_stored_byte() {
    // The rejection criterion, run as an experiment. The same WEK is delivered
    // under X25519 and again under P-256; the two grant envelopes differ, and
    // the object sealed under that WEK is byte-identical and opens under both.
    use crate::content::{ContentObject, ContentObjectHeader, seal_object};
    use crate::grant::{GrantExpectation, GrantFields, RecipientSecretKey, WekGrantV1};
    use crate::identity::CloudObjectId;
    use crate::keys::{KeyId, WekVersion, WorkspaceEncryptionKey};
    use crate::regional::RecoveryKeyId;
    use crate::suites::{AuthorType, GrantSuite, RecipientType};
    use koru_device::DeviceSigningKey;
    use koru_store_core::ids::WorkspaceId;
    use rand_core::Rng as _;
    use uuid::Uuid;

    let mut rng = VectorRng::new(b"suite-change");
    let workspace = WorkspaceId::from_uuid(Uuid::from_bytes([0x11; 16]));
    let wek = WorkspaceEncryptionKey::generate(&mut rng);
    let author = DeviceSigningKey::from_secret_bytes([0x12; 32]);

    let header = ContentObjectHeader::for_blob(
        workspace,
        WekVersion(7),
        CloudObjectId::from_bytes([0x13; 32]),
    );
    let stored = seal_object(&header, &wek, b"terabytes, notionally", &mut rng).unwrap();

    let mut envelopes = Vec::new();
    for suite in [
        GrantSuite::X25519HkdfSha256ChaCha20Poly1305,
        GrantSuite::P256HkdfSha256ChaCha20Poly1305,
    ] {
        let secret = loop {
            let mut bytes = [0u8; 32];
            rng.fill_bytes(&mut bytes);
            let candidate = RecipientSecretKey::from_bytes(suite, bytes);
            if candidate.public_key().is_ok() {
                break candidate;
            }
        };
        // The recovery recipient, because it is the one whose KEM may differ.
        let fields = GrantFields {
            recipient_type: RecipientType::RegionalRecovery,
            author_type: AuthorType::Device,
            workspace_id: workspace,
            wek_version: WekVersion(7),
            recipient_id: Uuid::from_bytes([0x14; 16]),
            recipient_key_id: KeyId::from_bytes([0x15; 16]),
            recipient_key_version: 1,
            recovery_key_id: RecoveryKeyId::new(KeyId::from_bytes([0x16; 16])),
            author_id: Uuid::from_bytes([0x17; 16]),
        };
        let public = secret.public_key().unwrap();
        let grant = WekGrantV1::seal(fields, &public, &wek, &author, &mut rng).unwrap();

        let delivered = grant
            .open(
                &GrantExpectation {
                    workspace_id: workspace,
                    wek_version: WekVersion(7),
                    recipient_type: RecipientType::RegionalRecovery,
                    recipient_id: Uuid::from_bytes([0x14; 16]),
                    recipient_key_id: KeyId::from_bytes([0x15; 16]),
                    recipient_key_version: 1,
                },
                &secret,
            )
            .unwrap();

        // The stored object is untouched and opens under the delivered key.
        assert_eq!(
            ContentObject::parse(&stored)
                .unwrap()
                .open(&delivered)
                .unwrap(),
            b"terabytes, notionally"
        );
        envelopes.push(grant.encode());
    }

    assert_ne!(envelopes[0], envelopes[1], "the two suites must differ");
    // And what changed is only the grant: the stored object never entered the
    // loop, and its bytes are the ones sealed before either grant existed.
    assert_eq!(
        ContentObject::parse(&stored).unwrap().header.suite,
        crate::suites::ContentSuite::Cobblestone256
    );
}
