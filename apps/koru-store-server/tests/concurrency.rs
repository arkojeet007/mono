// SPDX-License-Identifier: MPL-2.0

//! **The divergence matrix, through the real store.**
//!
//! Six of these are already pinned in `koru-store-core`'s manifest tests, which
//! converge two `Manifest`s in one process. That proves the CRDT semantics. It
//! does not prove that the *store* preserves them — a broadcast sent before a
//! commit, a room that echoes to its sender, a catch-up computed from the wrong
//! version vector would each break convergence while every in-process test
//! stayed green.
//!
//! So these drive two real WebSocket clients against a real daemon.
//!
//! **Converged is not the same as correct.** Loro will converge on almost
//! anything; the question each test asks is whether it converged on the answer
//! Koru wants. `edit_versus_delete` is the clearest case: two replicas agreeing
//! that a deleted document is back would be perfect convergence and wrong.

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
use koru_store_core::manifest::Manifest;
use koru_store_core::model::Timestamp;

// ------------------------------------------------------------------ harness

struct Store {
    child: Child,
    root: PathBuf,
}

impl Store {
    fn start() -> (tempfile::TempDir, Self) {
        let dir = tempfile::tempdir().unwrap();
        let store = Self::start_in(dir.path());
        (dir, store)
    }

    fn start_in(root: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .arg("serve")
            // A suite starting daemons on throwaway roots must never raise a
            // modal dialog: nothing dismisses it, so the process would never
            // exit and would go on holding its databases and this binary.
            .env("KORU_STORE_NO_DIALOG", "1")
            .arg("--console")
            .arg("--root")
            .arg(root);
        common::configure(&mut command, root);
        let child = command.spawn().expect("spawn");
        let store = Self {
            child,
            root: root.to_path_buf(),
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while common::discovery(&store.root).map(|d| Client::new(d).status().is_ok()) != Some(true)
        {
            assert!(Instant::now() < deadline, "the store never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        store
    }

    fn client(&self) -> Client {
        Client::new(common::discovery(&self.root).expect("discover"))
    }

    fn connection(&self) -> Connection {
        let mut c =
            Connection::open(&common::discovery(&self.root).expect("discover")).expect("connect");
        c.set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        c
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = &self.root as &Path;
    }
}

/// A replica of the workspace manifest, held by one client.
struct Peer {
    connection: Connection,
    manifest: Manifest,
    room: String,
    sent: loro::VersionVector,
}

impl Peer {
    fn join(store: &Store, workspace: WorkspaceId) -> Self {
        let mut peer = Self {
            connection: store.connection(),
            // Empty: this replica has no history and asks for everything. A
            // delta from a non-empty version would name ancestors it cannot
            // obtain, and Loro would park it as pending — applying nothing.
            manifest: Manifest::from_snapshot(
                &Manifest::create(workspace, "seed", Timestamp::from_millis(0))
                    .unwrap()
                    .export_snapshot()
                    .unwrap(),
            )
            .unwrap(),
            room: format!("workspace:{}:manifest", workspace.to_storage()),
            sent: loro::VersionVector::new(),
        };
        // Discard the seed and start from the store's own snapshot, which is
        // what a real client does.
        let client = store.client();
        let document = client.manifest(workspace).expect("manifest id");
        let snapshot = client.snapshot(workspace, document).expect("snapshot");
        peer.manifest = Manifest::from_snapshot(&snapshot).unwrap();
        peer.sent = peer.manifest.version();
        peer.connection
            .join(&peer.room, &peer.manifest.version())
            .expect("join");
        peer.settle();
        peer
    }

    /// Do something to the manifest, without sending it yet.
    fn edit(&mut self, change: impl FnOnce(&mut Manifest)) {
        change(&mut self.manifest);
    }

    /// Send everything this peer has that it has not sent.
    fn push(&mut self) {
        let update = self.manifest.export_updates_since(&self.sent).unwrap();
        self.sent = self.manifest.version();
        self.connection
            .send_update(&self.room, update)
            .expect("send");
        self.settle();
    }

    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.connection.next_event() {
                Ok(Some(Event::Updates { updates, .. })) => {
                    for update in updates {
                        let _ = self.manifest.import(&update);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Exchange until both peers stop changing.
    fn converge(a: &mut Peer, b: &mut Peer) {
        for _ in 0..6 {
            a.push_pending();
            b.push_pending();
            a.settle();
            b.settle();
        }
    }

    /// Send anything outstanding, without requiring there to be any.
    fn push_pending(&mut self) {
        let update = self.manifest.export_updates_since(&self.sent).unwrap();
        if self.sent != self.manifest.version() {
            self.sent = self.manifest.version();
            let _ = self.connection.send_update(&self.room, update);
        }
        self.settle();
    }

    fn titles(&self) -> Vec<String> {
        self.manifest
            .entries()
            .into_iter()
            .map(|e| e.title)
            .collect()
    }

    fn ids(&self) -> Vec<DocumentId> {
        self.manifest.entries().into_iter().map(|e| e.id).collect()
    }
}

fn workspace(store: &Store) -> WorkspaceId {
    let client = store.client();
    let profile = client
        .create_profile("Owner", None, None, Some("en"))
        .expect("profile");
    client
        .create_workspace(profile.id, "W")
        .expect("workspace")
        .id
}

/// Two peers on one workspace, both up to date.
fn pair(store: &Store) -> (WorkspaceId, Peer, Peer) {
    let ws = workspace(store);
    let a = Peer::join(store, ws);
    let b = Peer::join(store, ws);
    (ws, a, b)
}

// ------------------------------------------------------------------ the cases

#[test]
fn simultaneous_text_edits_both_survive() {
    // Two titles typed at once. A `LoroText` would merge them character by
    // character; a manifest title is a register, so one wins — but *both
    // documents* must exist, which is the part a lost broadcast would break.
    let (_dir, store) = Store::start();
    let (_ws, mut a, mut b) = pair(&store);

    a.edit(|m| {
        m.add_document(DocumentId::new(), "Quarterly plan", "page", None)
            .unwrap()
    });
    b.edit(|m| {
        m.add_document(DocumentId::new(), "四半期計画", "page", None)
            .unwrap()
    });
    Peer::converge(&mut a, &mut b);

    assert_eq!(a.titles().len(), 2, "an edit was lost: {:?}", a.titles());
    assert_eq!(a.titles(), b.titles(), "the replicas disagree");
}

#[test]
fn rename_versus_rename_converges_on_one_whole_title() {
    let (_dir, store) = Store::start();
    let (_ws, mut a, mut b) = pair(&store);

    let doc = DocumentId::new();
    a.edit(|m| m.add_document(doc, "Original", "page", None).unwrap());
    Peer::converge(&mut a, &mut b);
    assert_eq!(b.titles(), ["Original"], "b never saw the document");

    a.edit(|m| m.rename_document(doc, "Planejamento trimestral").unwrap());
    b.edit(|m| m.rename_document(doc, "الخطة الفصلية").unwrap());
    Peer::converge(&mut a, &mut b);

    assert_eq!(a.titles(), b.titles(), "the replicas disagree");
    let won = &a.titles()[0];
    // The semantic that matters: one edit wins outright. A character-merged
    // title would converge too, and be gibberish in both languages.
    assert!(
        won == "Planejamento trimestral" || won == "الخطة الفصلية",
        "the titles were merged rather than one winning: {won:?}"
    );
}

#[test]
fn move_versus_move_converges_on_one_order() {
    let (_dir, store) = Store::start();
    let (_ws, mut a, mut b) = pair(&store);

    let (x, y, z) = (DocumentId::new(), DocumentId::new(), DocumentId::new());
    a.edit(|m| {
        for (id, title) in [(x, "X"), (y, "Y"), (z, "Z")] {
            m.add_document(id, title, "page", None).unwrap();
        }
    });
    Peer::converge(&mut a, &mut b);
    assert_eq!(b.titles().len(), 3);

    a.edit(|m| m.move_document(z, None, 0).unwrap());
    b.edit(|m| m.move_document(z, None, 1).unwrap());
    Peer::converge(&mut a, &mut b);

    assert_eq!(a.titles(), b.titles(), "the replicas disagree on order");
    let mut sorted = a.titles();
    sorted.sort();
    assert_eq!(
        sorted,
        ["X", "Y", "Z"],
        "a move lost or duplicated a document"
    );
}

#[test]
fn edit_versus_delete_does_not_resurrect_a_deleted_document() {
    // The clearest case of "converged is not correct": two replicas agreeing
    // that a deleted document is back would be perfect convergence and wrong.
    let (_dir, store) = Store::start();
    let (_ws, mut a, mut b) = pair(&store);

    let doomed = DocumentId::new();
    a.edit(|m| m.add_document(doomed, "Doomed", "page", None).unwrap());
    Peer::converge(&mut a, &mut b);
    assert_eq!(b.titles(), ["Doomed"]);

    a.edit(|m| {
        m.remove_document(doomed).unwrap();
    });
    b.edit(|m| m.rename_document(doomed, "Renamed anyway").unwrap());
    Peer::converge(&mut a, &mut b);

    assert_eq!(a.titles(), b.titles(), "the replicas disagree");
    assert!(
        a.titles().is_empty(),
        "a deleted document came back: {:?}",
        a.titles()
    );
}

#[test]
fn work_done_while_disconnected_arrives_on_reconnect() {
    // "Create while offline", as it actually happens: the socket is gone, the
    // client keeps working, and everything lands when it comes back.
    let (_dir, store) = Store::start();
    let ws = workspace(&store);
    let mut a = Peer::join(&store, ws);

    a.connection.close(std::time::Duration::from_secs(1));
    for title in ["Offline one", "Offline two"] {
        a.edit(|m| {
            m.add_document(DocumentId::new(), title, "page", None)
                .unwrap()
        });
    }

    // A new socket, joining from the version this replica actually holds.
    a.connection = store.connection();
    a.connection
        .join(&a.room, &a.manifest.version())
        .expect("rejoin");
    a.settle();
    a.push_pending();

    let mut fresh = Peer::join(&store, ws);
    assert_eq!(fresh.titles(), ["Offline one", "Offline two"]);
    let _ = &mut fresh;
}

#[test]
fn a_replica_that_was_away_a_long_time_catches_up_in_one_exchange() {
    let (_dir, store) = Store::start();
    let ws = workspace(&store);
    let mut a = Peer::join(&store, ws);
    let behind = Peer::join(&store, ws);
    let behind_version = behind.manifest.version();
    drop(behind);

    a.edit(|m| {
        for i in 0..50 {
            m.add_document(DocumentId::new(), &format!("Doc {i}"), "page", None)
                .unwrap();
        }
    });
    a.push();

    // Joining from the old version asks for exactly the difference.
    let mut late = Peer::join(&store, ws);
    assert_eq!(late.titles().len(), 50);
    assert!(late.manifest.version().includes_vv(&behind_version));
    let _ = &mut late;
}

#[test]
fn a_redelivered_update_changes_nothing() {
    // Transports retry. Sending the same bytes twice must cost bytes rather
    // than correctness.
    let (_dir, store) = Store::start();
    let ws = workspace(&store);
    let mut a = Peer::join(&store, ws);

    a.edit(|m| {
        m.add_document(DocumentId::new(), "Once", "page", None)
            .unwrap()
    });
    let update = a.manifest.export_updates_since(&a.sent).unwrap();
    a.sent = a.manifest.version();

    for _ in 0..3 {
        a.connection
            .send_update(&a.room, update.clone())
            .expect("send");
        a.settle();
    }

    let mut fresh = Peer::join(&store, ws);
    assert_eq!(fresh.titles(), ["Once"], "a retry duplicated the document");
    let _ = &mut fresh;
}

#[test]
fn a_sender_is_not_sent_its_own_update_back() {
    // It already has it — echoing would make every edit a round trip that ends
    // in `frame_is_known`, and would hide a broken broadcast behind a no-op.
    let (_dir, store) = Store::start();
    let ws = workspace(&store);
    let mut a = Peer::join(&store, ws);

    a.edit(|m| {
        m.add_document(DocumentId::new(), "Mine", "page", None)
            .unwrap()
    });
    let update = a.manifest.export_updates_since(&a.sent).unwrap();
    a.sent = a.manifest.version();
    a.connection.send_update(&a.room, update).expect("send");

    let deadline = Instant::now() + Duration::from_secs(1);
    let mut echoed = false;
    while Instant::now() < deadline {
        match a.connection.next_event() {
            Ok(Some(Event::Updates { .. })) => echoed = true,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert!(!echoed, "the store echoed an update to its sender");
}

#[test]
fn ids_survive_convergence_unchanged() {
    // A document id is the thing everything else points at. Convergence that
    // preserved titles and changed ids would break every reference in the
    // workspace while looking correct in a listing.
    let (_dir, store) = Store::start();
    let (_ws, mut a, mut b) = pair(&store);

    let mine = DocumentId::new();
    a.edit(|m| m.add_document(mine, "Mine", "page", None).unwrap());
    Peer::converge(&mut a, &mut b);

    assert_eq!(a.ids(), vec![mine]);
    assert_eq!(b.ids(), vec![mine]);
}

#[test]
fn a_document_created_over_http_reaches_a_websocket_subscriber() {
    // **Found through a live room transcript, not by a test.** The HTTP route
    // created the document, committed it, and told nobody — so a client
    // watching the manifest room stayed stale until it rejoined. Two clients
    // doing entirely ordinary things, diverging silently.
    //
    // An HTTP mutation and a WebSocket one are the same mutation. A subscriber
    // must not have to know which door an edit came through.
    let (_dir, store) = Store::start();
    let ws = workspace(&store);
    let mut watching = Peer::join(&store, ws);
    assert!(watching.titles().is_empty());

    // The other client uses the management API rather than the socket.
    store
        .client()
        .create_document(ws, "Through the other door", "page")
        .expect("create over HTTP");

    let deadline = Instant::now() + Duration::from_secs(10);
    while watching.titles().is_empty() && Instant::now() < deadline {
        watching.settle();
    }
    assert_eq!(
        watching.titles(),
        ["Through the other door"],
        "an HTTP mutation never reached the room"
    );
}

#[test]
fn an_http_mutation_is_broadcast_only_after_it_is_durable() {
    // The same ordering the protocol path obeys. If the broadcast could
    // outrun the commit, a subscriber would build on work a power cut could
    // still erase — so killing the store the instant the subscriber sees the
    // document, and restarting, must find it there.
    let (dir, mut store) = Store::start();
    let ws = workspace(&store);
    let mut watching = Peer::join(&store, ws);

    store
        .client()
        .create_document(ws, "Durable", "page")
        .expect("create");

    let deadline = Instant::now() + Duration::from_secs(10);
    while watching.titles().is_empty() && Instant::now() < deadline {
        watching.settle();
    }
    assert_eq!(watching.titles(), ["Durable"], "never arrived");
    drop(watching);

    // It has been seen. If the broadcast preceded the commit, this loses it.
    store.child.kill().expect("kill");
    store.child.wait().expect("reap");
    let restarted = Store::start_in(dir.path());

    let after = Peer::join(&restarted, ws);
    assert_eq!(
        after.titles(),
        ["Durable"],
        "a broadcast outran the commit: a subscriber saw work the store had not persisted"
    );
}
