// SPDX-License-Identifier: MPL-2.0

//! **The gate. Spec §17.**
//!
//! Nothing downstream of this is worth building until it passes green, because
//! everything downstream assumes the thing it proves: that an acknowledgement
//! from the Koru Store means the bytes are on the disk.
//!
//! ```text
//! 1  create workspace W1
//! 2  create document D1
//! 3  connect clients A and B
//! 4  A writes "Hello"
//! 5  B sees "Hello"
//! 6  kill A and B
//! 7  reconnect B
//! 8  "Hello" is still there
//! 9  kill the store ABRUPTLY during further edits
//! 10 restart the store
//! 11 every edit acknowledged as durable still exists
//! ```
//!
//! **Step 11 asserts on acknowledgements the client recorded, never on what it
//! sent.** Those are different sets — an update in flight when the process died
//! is sent but not acknowledged, and the store owes nothing for it. A test that
//! asserted on "what we sent" would either fail on a correct store or, worse,
//! be quietly relaxed until it asserted nothing at all.
//!
//! The client here is `koru-store-client`, which talks to the store over the
//! same authenticated HTTP and WebSocket any third party would. There is no
//! privileged path in.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use koru_store_client::{Client, Connection, Event};

/// The channel this test binary and the store it spawns both are.
///
/// They have to agree — the rendezvous is keyed on the channel, so a test
/// looking for `stable` would not find the store it just started. Asked rather
/// than written down: both are built by the same cargo invocation, so a
/// `cargo test --release` moves both to `internal-release` together and a
/// hardcoded string would move neither.
use koru_store_core::ids::{DocumentId, WorkspaceId};

// ------------------------------------------------------------------ the store

/// A real `koru-store-server` process on a fixed root.
struct Store {
    child: Option<Child>,
    root: PathBuf,
}

impl Store {
    fn start(root: &Path) -> Self {
        let mut store = Self {
            child: None,
            root: root.to_path_buf(),
        };
        store.spawn();
        store
    }

    fn spawn(&mut self) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .arg("serve")
            // A suite starting daemons on throwaway roots must never raise a
            // modal dialog: nothing dismisses it, so the process would never
            // exit and would go on holding its databases and this binary.
            .env("KORU_STORE_NO_DIALOG", "1")
            .arg("--console")
            .arg("--root")
            .arg(&self.root);
        common::configure(&mut command, &self.root);
        let child = command.spawn().expect("spawn koru-store-server");
        self.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(30);
        while common::discovery(&self.root).map(|d| Client::new(d).status().is_ok()) != Some(true) {
            assert!(
                Instant::now() < deadline,
                "the store never became reachable"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// **The abrupt kill.** `TerminateProcess` on Windows, `SIGKILL` elsewhere:
    /// no unwinding, no flush, no graceful shutdown. Exactly the failure the
    /// durability invariant exists to survive.
    fn kill_abruptly(&mut self) {
        let mut child = self.child.take().expect("a running store");
        child.kill().expect("kill");
        child.wait().expect("reap");
    }

    fn restart(&mut self) {
        self.spawn();
    }

    fn client(&self) -> Client {
        Client::new(common::discovery(&self.root).expect("discover the store"))
    }

    fn connection(&self) -> Connection {
        let discovery = common::discovery(&self.root).expect("discover");
        let mut connection = Connection::open(&discovery).expect("connect");
        // Short, because `settle` treats a timed-out read as "nothing more to
        // drain" and returns. A long one here does not make the test more
        // patient — `wait_for` owns the patience — it just makes every quiet
        // moment cost the full timeout, which took this suite from seconds to
        // a minute and a half.
        connection
            .set_read_timeout(Some(Duration::from_millis(150)))
            .expect("timeout");
        connection
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ------------------------------------------------------------------ a client

/// A peer holding its own replica, as a real client does.
struct Peer {
    connection: Connection,
    doc: loro::LoroDoc,
    room: String,
    /// What has already been sent, so each update is a delta.
    sent: loro::VersionVector,
}

impl Peer {
    /// Join a document room, starting from whatever the store gives back.
    fn join(store: &Store, document: DocumentId) -> Self {
        let mut peer = Self {
            connection: store.connection(),
            doc: loro::LoroDoc::new(),
            room: format!("document:{}", document.to_storage()),
            sent: loro::VersionVector::new(),
        };
        // An empty version vector: this replica has no history, so it asks for
        // everything. A delta from a non-empty version would name ancestors it
        // cannot obtain, and Loro would park it as pending — applying nothing,
        // reporting nothing.
        peer.connection
            .join(&peer.room, &peer.doc.oplog_vv())
            .expect("join");
        peer.drain(Duration::from_secs(5));
        peer
    }

    /// Type into the document and send the delta. Returns the batch id.
    fn write(&mut self, text: &str) -> loro_protocol::protocol::BatchId {
        let content = self.doc.get_text("content");
        let at = content.len_utf8();
        content.insert_utf8(at, text).expect("insert");
        self.doc.commit();

        let update = self
            .doc
            .export(loro::ExportMode::updates(&self.sent))
            .expect("export");
        self.sent = self.doc.oplog_vv();
        self.connection
            .send_update(&self.room, update)
            .expect("send")
    }

    /// Drain what the socket already holds, applying updates and collecting
    /// durable acks. Returns as soon as the socket goes quiet.
    fn drain(&mut self, patience: Duration) -> Vec<loro_protocol::protocol::BatchId> {
        let deadline = Instant::now() + patience;
        let mut durable = Vec::new();
        while Instant::now() < deadline {
            match self.connection.next_event() {
                Ok(Some(Event::Updates { updates, .. })) => {
                    for update in updates {
                        let _ = self.doc.import(&update);
                    }
                }
                Ok(Some(Event::Acked { ack, .. })) => {
                    if ack.durable {
                        durable.push(ack.batch);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => break,
            }
        }
        durable
    }

    /// Wait for the store to call a write durable, up to `patience`.
    ///
    /// **A quiet socket is not an answer.** `next_event` reports a read timeout
    /// — 150 ms here — as `Ok(None)`, which is indistinguishable from an
    /// acknowledgement still in flight, and `drain` stops at the first quiet
    /// moment. So this used to *be* `drain`, and its five-second patience was
    /// worth one read timeout; it passed only while `--test-threads=2` kept the
    /// machine idle enough for the ack to arrive inside that window, and failed
    /// the day the harness ran at its own default. Keep asking until something
    /// durable arrives or the patience really is spent.
    fn settle(&mut self, patience: Duration) -> Vec<loro_protocol::protocol::BatchId> {
        let deadline = Instant::now() + patience;
        loop {
            let durable = self.drain(patience);
            if !durable.is_empty() || Instant::now() >= deadline {
                return durable;
            }
        }
    }

    /// Wait until this peer's document contains `text`, or give up.
    fn wait_for(&mut self, text: &str, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            if self.text().contains(text) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.drain(Duration::from_millis(200));
        }
    }

    fn text(&self) -> String {
        self.doc.get_text("content").to_string()
    }
}

// ------------------------------------------------------------------ set-up

fn workspace_and_document(store: &Store) -> (WorkspaceId, DocumentId) {
    let client = store.client();
    let profile = client
        .create_profile("Owner", None, None, Some("en"))
        .expect("create a profile");
    let workspace = client
        .create_workspace(profile.id, "W1")
        .expect("create W1");
    let document = client
        .create_document(workspace.id, "D1", "page")
        .expect("create D1");
    (workspace.id, document)
}

// ------------------------------------------------------------------ the slice

#[test]
fn two_clients_converge_and_every_acknowledged_edit_survives_an_abrupt_kill() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::start(dir.path());

    // 1, 2.
    let (_workspace, document) = workspace_and_document(&store);

    // 3.
    let mut a = Peer::join(&store, document);
    let mut b = Peer::join(&store, document);

    // 4.
    a.write("Hello");
    let acked = a.settle(Duration::from_secs(5));
    assert_eq!(acked.len(), 1, "the store did not acknowledge the write");

    // 5. B sees it — and only because the store broadcast it after committing.
    assert!(
        b.wait_for("Hello", Duration::from_secs(10)),
        "B never saw A's write; B has {:?}",
        b.text()
    );

    // 6.
    a.connection.close(std::time::Duration::from_secs(1));
    b.connection.close(std::time::Duration::from_secs(1));
    drop(a);
    drop(b);

    // 7, 8. A fresh replica, catching up from nothing.
    let mut b = Peer::join(&store, document);
    assert!(
        b.wait_for("Hello", Duration::from_secs(10)),
        "the reconnected client did not get the earlier work; it has {:?}",
        b.text()
    );

    // 9. More edits, recording exactly which the store called durable.
    let mut durable_text = String::from("Hello");
    for word in [" world", " again", " and again"] {
        b.write(word);
        let acked = b.settle(Duration::from_secs(5));
        assert_eq!(acked.len(), 1, "no acknowledgement for {word:?}");
        durable_text.push_str(word);
    }

    // ...and one more that is deliberately **not** waited for. It may or may
    // not survive; the store owes nothing for an update it never acknowledged,
    // and asserting either way would be asserting the wrong thing.
    b.write(" in flight");

    // The abrupt kill, with that last update quite possibly still in the air.
    store.kill_abruptly();

    // 10.
    store.restart();

    // 11. Everything acknowledged is still there.
    let mut after = Peer::join(&store, document);
    assert!(
        after.wait_for(&durable_text, Duration::from_secs(10)),
        "an acknowledged edit was lost across an abrupt kill.\n  expected to contain: {durable_text:?}\n  found: {:?}",
        after.text()
    );
}

#[test]
fn an_update_is_broadcast_only_after_it_is_durable() {
    // The other half of the invariant. If a broadcast could outrun the commit,
    // B would see — and build on — work a power cut could still erase. Killing
    // the store the instant B sees the text is the closest a test can get to
    // that race.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::start(dir.path());
    let (_workspace, document) = workspace_and_document(&store);

    let mut a = Peer::join(&store, document);
    let mut b = Peer::join(&store, document);

    a.write("Committed");
    assert!(
        b.wait_for("Committed", Duration::from_secs(10)),
        "B never saw it"
    );

    // B has seen it. If the broadcast came before the commit, this kill loses
    // it and the restart below finds nothing.
    store.kill_abruptly();
    store.restart();

    let mut after = Peer::join(&store, document);
    assert!(
        after.wait_for("Committed", Duration::from_secs(10)),
        "a broadcast outran the commit: B saw work the store had not persisted"
    );
}

#[test]
fn a_client_that_reconnects_after_the_store_restarts_catches_up() {
    // A restart gives the store a fresh process but the same durable replica.
    // A client rejoining must be told what it is missing, from its own version
    // vector rather than from a fresh snapshot every time.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::start(dir.path());
    let (_workspace, document) = workspace_and_document(&store);

    let mut a = Peer::join(&store, document);
    a.write("Before");
    assert_eq!(a.settle(Duration::from_secs(5)).len(), 1);

    store.kill_abruptly();
    store.restart();

    let mut fresh = Peer::join(&store, document);
    assert!(
        fresh.wait_for("Before", Duration::from_secs(10)),
        "a client joining after a restart got nothing; it has {:?}",
        fresh.text()
    );
}

#[test]
fn the_store_can_be_stopped_and_started_without_losing_anything() {
    // The graceful path, for contrast with the abrupt one. Both must preserve
    // acknowledged work; only one gets to run its shutdown code.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::start(dir.path());
    let (_workspace, document) = workspace_and_document(&store);

    let mut a = Peer::join(&store, document);
    a.write("Graceful");
    assert_eq!(a.settle(Duration::from_secs(5)).len(), 1);
    a.connection.close(std::time::Duration::from_secs(1));
    drop(a);

    store.client().shutdown().expect("stop standalone Store");
    if let Some(child) = &mut store.child {
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().ok().flatten().is_none() {
            assert!(Instant::now() < deadline, "the store did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    store.child = None;

    store.restart();
    let mut after = Peer::join(&store, document);
    assert!(
        after.wait_for("Graceful", Duration::from_secs(10)),
        "work was lost across a graceful restart"
    );
}
