// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **The handshake's field names, held still.** The worker's own tests read a
/// bootstrap through `read` and `validate`; this one asserts only the
/// declaration, so a renamed field fails here rather than in a launch where an
/// Agent and a worker disagree about one JSON document.
#[test]
fn a_bootstrap_round_trips_as_json() {
    let sent = WorkerBootstrap {
        bootstrap_version: BOOTSTRAP_VERSION,
        launch_id: "a-launch".into(),
        channel: "stable".into(),
        root_fingerprint: "root-digest".into(),
        installation_fingerprint: "image-digest".into(),
    };
    let wire = serde_json::to_string(&sent).unwrap();
    assert_eq!(
        wire,
        r#"{"bootstrap_version":1,"launch_id":"a-launch","channel":"stable","root_fingerprint":"root-digest","installation_fingerprint":"image-digest"}"#
    );
    let read: WorkerBootstrap = serde_json::from_str(&wire).unwrap();
    assert_eq!(read, sent);
}
