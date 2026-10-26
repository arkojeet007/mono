// SPDX-License-Identifier: MPL-2.0

use super::*;

fn bootstrap(root: &std::path::Path) -> WorkerBootstrap {
    WorkerBootstrap {
        bootstrap_version: BOOTSTRAP_VERSION,
        launch_id: "a-launch".into(),
        channel: koru_channel::segment().into(),
        root_fingerprint: fingerprint(root.to_string_lossy().as_bytes()),
        installation_fingerprint: this_image(),
    }
}

fn this_image() -> String {
    let executable = std::env::current_exe().unwrap();
    fingerprint(std::fs::read(executable).unwrap())
}

#[test]
fn a_bootstrap_round_trips_through_a_pipe() {
    let root = std::path::Path::new("C:/tmp/a-root");
    let sent = bootstrap(root);
    let wire = serde_json::to_vec(&sent).unwrap();
    let read = read(std::io::Cursor::new(wire)).unwrap();
    assert_eq!(read, sent);
    validate(&read, root, koru_channel::segment()).unwrap();
}

/// Trailing whitespace is what a parent that wrote a newline leaves behind, and
/// refusing to start over it would be a supervision failure nobody could see.
#[test]
fn trailing_whitespace_is_tolerated() {
    let root = std::path::Path::new("C:/tmp/a-root");
    let mut wire = serde_json::to_vec(&bootstrap(root)).unwrap();
    wire.extend_from_slice(b"\r\n");
    assert!(read(std::io::Cursor::new(wire)).is_ok());
}

#[test]
fn nonsense_on_stdin_is_named_rather_than_ignored() {
    let refused = read(std::io::Cursor::new(b"not json".to_vec())).unwrap_err();
    assert!(refused.contains("not readable"), "{refused}");
}

/// **The three things the fingerprints answer.** Three channels install side by
/// side, an update leaves an old binary in `.previous/`, and an agent that
/// spawned the wrong one would supervise a worker writing into another
/// channel's model cache.
#[test]
fn a_bootstrap_for_another_channel_root_or_image_is_refused() {
    let root = std::path::Path::new("C:/tmp/a-root");

    let mut wrong_version = bootstrap(root);
    wrong_version.bootstrap_version = BOOTSTRAP_VERSION + 1;
    assert!(
        validate(&wrong_version, root, koru_channel::segment())
            .unwrap_err()
            .contains("version")
    );

    let mut wrong_channel = bootstrap(root);
    wrong_channel.channel = "nightly-but-not-this-one".into();
    assert!(
        validate(&wrong_channel, root, koru_channel::segment())
            .unwrap_err()
            .contains("channel")
    );

    let other_root = std::path::Path::new("C:/tmp/another-root");
    let refused = validate(&bootstrap(root), other_root, koru_channel::segment()).unwrap_err();
    assert!(refused.contains("different root"), "{refused}");
    // Neither path is printed: a root is a filesystem path, which
    // docs/contributing/operations/LOGGING.md names beside credentials.
    assert!(!refused.contains("a-root"), "{refused}");

    let mut wrong_image = bootstrap(root);
    wrong_image.installation_fingerprint = fingerprint(b"another build");
    assert!(
        validate(&wrong_image, root, koru_channel::segment())
            .unwrap_err()
            .contains("different worker image")
    );
}

/// The same hash function, spelled the same way, as the gateway's bootstrap —
/// so a fingerprint computed by either side is comparable by the other.
#[test]
fn the_fingerprint_is_blake3_hex() {
    let hex = fingerprint(b"abc");
    assert_eq!(hex.len(), 64);
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    // A literal, not `blake3::hash` again: comparing the function with itself
    // proves nothing about the spelling. The BLAKE3 reference vector for
    // "abc", and the same string `koru-agent` asserts for
    // `koru_network_bridge::fingerprint`.
    assert_eq!(
        hex,
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
}

/// A parent could be anything, and `read_to_string` on an unbounded pipe is a
/// way to make this process allocate whatever the writer feels like.
#[test]
fn a_bootstrap_is_read_with_a_bound() {
    let huge = vec![b'x'; 128 * 1024];
    // It fails to parse rather than reading all of it, which is the point: the
    // read stopped at the cap.
    assert!(read(std::io::Cursor::new(huge)).is_err());
}
