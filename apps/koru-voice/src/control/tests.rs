// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The endpoint token is `koru_ipc`'s, never a literal: a second spelling is a
/// client and a worker that each bind something and never meet.
#[test]
fn the_service_token_is_the_one_koru_ipc_declares() {
    assert_eq!(SERVICE, koru_ipc::VOICE);
    assert_eq!(SERVICE, "koru-voice");
}
