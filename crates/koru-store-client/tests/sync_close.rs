// SPDX-License-Identifier: MPL-2.0

//! A sync connection closes with a handshake, not a drop.
//!
//! The gateway logged *forcibly closed* once per MCP read and edit: the link
//! thread ended and dropped its socket with the store's replies unread, which
//! Windows answers with a reset. `Connection::close` now sends the Close frame
//! and reads until the store answers in kind. These tests run it against a
//! loopback server that answers, one that answers late, and one that never
//! does — the last must cost the budget and no more.

use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use koru_store_client::http::Discovery;
use koru_store_client::sync::Connection;
use tungstenite::Message;

/// A loopback server for one connection, running `serve` on the accepted
/// socket; the discovery that reaches it.
fn server(
    serve: impl FnOnce(tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
) -> (Discovery, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve(tungstenite::accept(stream).unwrap());
    });
    (
        Discovery {
            port,
            token: "t".into(),
            expires_at_millis: i64::MAX,
        },
        handle,
    )
}

/// Read frames until the client's Close arrives, then answer it and finish the
/// handshake the way a well-behaved peer does.
fn answer_close(mut socket: tungstenite::WebSocket<std::net::TcpStream>) {
    loop {
        match socket.read() {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = socket.flush();
    // Keep reading so the close completes on this side too.
    while socket.read().is_ok() {}
}

#[test]
fn a_close_waits_for_the_stores_answer() {
    let (discovery, server) = server(answer_close);
    let mut connection = Connection::open(&discovery).unwrap();
    assert!(
        connection.close(Duration::from_secs(2)),
        "the store answered, so the handshake finished"
    );
    server.join().unwrap();
}

#[test]
fn a_store_still_talking_is_read_to_the_end_before_the_close_completes() {
    // The reset's cause: replies in flight when the client goes. The server
    // sends a burst and then answers the Close; the client must read the burst
    // and still finish cleanly.
    let (discovery, server) = server(|mut socket| {
        for i in 0..20u8 {
            let _ = socket.send(Message::Binary(vec![i; 512].into()));
        }
        answer_close(socket);
    });
    let mut connection = Connection::open(&discovery).unwrap();
    assert!(connection.close(Duration::from_secs(2)));
    server.join().unwrap();
}

#[test]
fn a_store_that_never_answers_costs_the_budget_and_no_more() {
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (discovery, server) = server(move |socket| {
        // Silent until the test lets go: no read, no Close.
        let _keep = socket;
        let _ = hold.recv();
    });
    let mut connection = Connection::open(&discovery).unwrap();
    let started = Instant::now();
    let finished = connection.close(Duration::from_millis(120));
    let took = started.elapsed();
    assert!(!finished, "no answer inside the budget");
    assert!(
        took >= Duration::from_millis(120) && took < Duration::from_millis(600),
        "{took:?}"
    );
    let _ = release.send(());
    server.join().unwrap();
}
