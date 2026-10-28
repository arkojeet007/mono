// SPDX-License-Identifier: MPL-2.0

use koru_store_core::ids::WorkspaceId;
use rand_core::Rng as _;
use uuid::Uuid;

use super::*;
use crate::testkit::VectorRng;

fn hash(byte: u8) -> BlobHash {
    BlobHash::from_digest("b3", &format!("{byte:02x}").repeat(32)).unwrap()
}

fn workspace(byte: u8) -> WorkspaceId {
    WorkspaceId::from_uuid(Uuid::from_bytes([byte; 16]))
}

#[test]
fn the_same_blob_in_the_same_workspace_gets_the_same_id() {
    // Deduplication within one workspace is intentional and visible (D90).
    let root = WorkspaceBlobIdentityKey::from_bytes([0x01; 32]);
    let a = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(9), &hash(0xaa));
    let b = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(9), &hash(0xaa));
    assert_eq!(a, b);
}

#[test]
fn the_same_blob_in_two_workspaces_gets_unrelated_ids() {
    // The claim, precisely: cross-workspace equality correlation is prevented
    // **assuming each workspace's identity key is independently random**. Two
    // independent roots here; the assumption is the whole of the property.
    let first = WorkspaceBlobIdentityKey::from_bytes([0x01; 32]);
    let second = WorkspaceBlobIdentityKey::from_bytes([0x02; 32]);
    let a = cloud_object_id(&first, BlobIdentityKeyVersion(1), workspace(1), &hash(0xaa));
    let b = cloud_object_id(
        &second,
        BlobIdentityKeyVersion(1),
        workspace(2),
        &hash(0xaa),
    );
    assert_ne!(a, b);
}

#[test]
fn two_workspaces_sharing_a_root_would_correlate_which_is_why_they_must_not() {
    // The failure mode named out loud, so the assumption is testable rather
    // than aspirational: with one root and one workspace id, the ids match.
    let root = WorkspaceBlobIdentityKey::from_bytes([0x03; 32]);
    let a = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), &hash(0xaa));
    let b = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), &hash(0xaa));
    assert_eq!(a, b);
    // Binding the workspace id means even a shared root does not correlate
    // across workspaces — defence in depth behind the key, never instead of it.
    let c = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(2), &hash(0xaa));
    assert_ne!(a, c);
}

#[test]
fn the_identity_key_version_is_its_own_axis() {
    let root = WorkspaceBlobIdentityKey::from_bytes([0x04; 32]);
    let v1 = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), &hash(0xaa));
    let v2 = cloud_object_id(&root, BlobIdentityKeyVersion(2), workspace(1), &hash(0xaa));
    assert_ne!(v1, v2);
}

#[test]
fn different_blobs_get_different_ids_and_the_encoding_is_unambiguous() {
    let root = WorkspaceBlobIdentityKey::from_bytes([0x05; 32]);
    let id = |h: &BlobHash| cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), h);
    assert_ne!(id(&hash(0xaa)), id(&hash(0xbb)));
    // The algorithm prefix is part of the input, length-prefixed, so a legacy
    // spelling and a `b3:` spelling of the same digest cannot collide.
    let legacy = BlobHash::from_digest("fnv1a", &"aa".repeat(32)).unwrap();
    assert_ne!(id(&hash(0xaa)), id(&legacy));
}

#[test]
fn the_root_is_never_used_raw() {
    // D90: the identity root never doubles as an encryption key, and the one
    // use it has goes through a derived subkey. If the raw root were the hash
    // key, this would be equal.
    let root = WorkspaceBlobIdentityKey::from_bytes([0x06; 32]);
    let derived = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), &hash(0xaa));
    let mut input = crate::codec::Writer::default();
    input.bytes(b"koru:cloud-object-id:v1\0");
    input.uuid(workspace(1).as_uuid());
    input.u32(1);
    input.var(hash(0xaa).as_str().as_bytes()).unwrap();
    let raw: [u8; 32] = blake3::keyed_hash(root.as_bytes(), input.as_slice()).into();
    assert_ne!(derived.as_bytes(), &raw);
}

#[test]
fn ids_look_uniform_over_many_blobs() {
    // Not a security proof — BLAKE3's is elsewhere — but a check that nothing
    // in the input construction is constant across inputs, which is the kind of
    // bug that would make correlation trivial and a round trip still pass.
    let mut rng = VectorRng::new(b"identity");
    let mut root_bytes = [0u8; 32];
    rng.fill_bytes(&mut root_bytes);
    let root = WorkspaceBlobIdentityKey::from_bytes(root_bytes);

    let mut seen = std::collections::BTreeSet::new();
    for i in 0..512u32 {
        let h = BlobHash::from_digest("b3", &format!("{i:064x}")).unwrap();
        let id = cloud_object_id(&root, BlobIdentityKeyVersion(1), workspace(1), &h);
        assert!(seen.insert(*id.as_bytes()), "collision at {i}");
    }
}

#[test]
fn the_storage_form_is_lowercase_hex() {
    let id = CloudObjectId::from_bytes([0x0f; 32]);
    assert_eq!(id.to_storage(), "0f".repeat(32));
    assert_eq!(id.to_storage().len(), 64);
}
