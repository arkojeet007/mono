// SPDX-License-Identifier: MPL-2.0

//! `GET /v1/sync` — the WebSocket carrying the Loro Protocol.
//!
//! One connection, many rooms. The protocol multiplexes, so a client editing
//! four documents holds one socket rather than four.
//!
//! ## Authentication is on the upgrade
//!
//! The bearer token travels as an ordinary `Authorization` header on the HTTP
//! request that becomes the socket. **Nothing about authentication appears in
//! the protocol**, which is what lets a client built against the published Loro
//! Protocol — knowing nothing about Koru — connect and sync.
//!
//! ## Two tasks, one channel
//!
//! ```text
//! socket ──► reader ──► store (locked briefly) ──► reply ──┐
//!                                    └─► broadcast ──► rooms
//!                                                            │
//! socket ◄── writer ◄──────────── mpsc ◄──────────────────────┘
//! ```
//!
//! The writer is separate because a frame can arrive from two directions: a
//! reply to this connection's own message, and a broadcast caused by somebody
//! else's. Funnelling both through one channel means the socket has a single
//! writer, which is what `WebSocket` requires and what keeps frames from
//! interleaving.
//!
//! Everything this connection joined is left when it closes, however it closes.
//! A room holding senders for a dead socket would grow forever.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use koru_store_core::model::Timestamp;
use loro_protocol::encoding;
use loro_protocol::protocol::{BatchId, CrdtType, ProtocolMessage, RoomErrorCode};
use tokio::sync::mpsc;

use crate::auth::Caller;
use crate::protocol;
use crate::rooms::Room;
use crate::state::AppState;

const FRAGMENT_BYTES: usize = loro_protocol::protocol::MAX_MESSAGE_SIZE - 4 * 1024;
const MAX_FRAGMENTED_UPDATE_BYTES: usize = 16 * 1024 * 1024;

struct IncomingFragments {
    crdt: CrdtType,
    room: String,
    batch: BatchId,
    count: u64,
    total: usize,
    next: u64,
    bytes: Vec<u8>,
}

pub async fn upgrade(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    ws: WebSocketUpgrade,
) -> Response {
    // The auth middleware has already run — reaching here means the token was
    // good — so the socket inherits an authenticated identity and the protocol
    // never has to carry one.
    ws.on_upgrade(move |socket| serve(socket, state, caller))
}

async fn serve(socket: WebSocket, state: Arc<AppState>, caller: Caller) {
    let (mut sink, mut stream) = socket.split();
    let (outgoing, mut to_send) = mpsc::unbounded_channel::<Vec<u8>>();
    let (close, mut close_requested) = mpsc::unbounded_channel::<()>();
    let who = state.rooms.next_connection();
    state.rooms.register(who, close);
    tracing::debug!(connection = who.0, "sync connection opened");

    // The single writer.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                frame = to_send.recv() => {
                    let Some(frame) = frame else { break };
                    if sink.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                },
                _ = close_requested.recv() => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    let mut joined: Vec<Room> = Vec::new();
    let mut fragments: Option<IncomingFragments> = None;

    // What a connection did before it ended, so a reset can be read against
    // it: a client that sent `Leave` and then vanished is a different story
    // from one that vanished mid-edit, and a Windows reset ("forcibly closed")
    // is what a socket dropped with unread data looks like.
    let opened = std::time::Instant::now();
    let mut seen = Seen::default();
    let mut ended = "the loop ran out";

    loop {
        let message = match stream.next().await {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                ended = "read failed";
                // The whole chain, and the OS error kind when one is in it:
                // `%error` alone said only *forcibly closed*.
                let mut kind = None;
                let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
                while let Some(cause) = source {
                    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                        kind = Some(io.kind());
                        break;
                    }
                    source = cause.source();
                }
                tracing::warn!(
                    connection = who.0,
                    %error,
                    io_kind = ?kind,
                    age_ms = opened.elapsed().as_millis() as u64,
                    frames = seen.frames,
                    last_frame = seen.last,
                    since_last_ms = seen.at.elapsed().as_millis() as u64,
                    sent_leave = seen.leave,
                    sent_close = seen.close,
                    rooms = joined.len(),
                    "sync socket read failed"
                );
                break;
            }
            None => {
                ended = "client closed the socket";
                tracing::debug!(connection = who.0, "sync client closed the socket");
                break;
            }
        };
        let bytes = match message {
            Message::Binary(bytes) => bytes,
            // The protocol is binary. A text frame is a client that has not
            // read it; closing is clearer than guessing.
            Message::Text(_) => {
                tracing::warn!(connection = who.0, "sync client sent a text frame");
                break;
            }
            Message::Close(_) => {
                seen.close = true;
                ended = "client requested close";
                tracing::debug!(connection = who.0, "sync client requested close");
                break;
            }
            // Ping and Pong are answered by axum.
            Message::Ping(_) | Message::Pong(_) => continue,
        };

        let Some(decoded) = encoding::try_decode(&bytes) else {
            // Undecodable input is not a reason to keep a connection that is
            // clearly out of step with the wire format.
            tracing::warn!(
                connection = who.0,
                bytes = bytes.len(),
                "sync client sent an undecodable frame"
            );
            break;
        };

        let decoded = match accept_fragment(decoded, &mut fragments) {
            Ok(Some(message)) => message,
            Ok(None) => {
                seen.note("fragment");
                continue;
            }
            Err(problem) => {
                if send(&outgoing, &problem).is_err() {
                    break;
                }
                continue;
            }
        };
        seen.note(frame_kind(&decoded));

        let outcome = {
            // The lock is held for exactly one message — which includes its
            // commit, and therefore its fsync. That is the store's
            // serialisation point.
            let Ok(mut store) = state.store.lock() else {
                tracing::error!(connection = who.0, "Store lock was poisoned during sync");
                break;
            };
            protocol::handle(&mut store, &caller, decoded, Timestamp::now())
        };

        // The Store commit is the wake boundary, not successful WebSocket
        // delivery. Text, tree and mark operations are all opaque Loro bytes
        // here and deliberately take this same path. Wake before replying so a
        // client disappearing after its durable write cannot suppress peer
        // reconciliation.
        if outcome.durable_change {
            state.notify_replication();
        }

        if let Some(room) = outcome.joined.clone() {
            if !state.rooms.join(room.clone(), who, outgoing.clone()) {
                tracing::error!(
                    connection = who.0,
                    room = %room.to_id(),
                    "sync room registration failed"
                );
                break;
            }
            tracing::debug!(connection = who.0, room = %room.to_id(), "sync room joined");
            joined.push(room);
        }
        if let Some(room) = outcome.left {
            state.rooms.leave(&room, who);
            joined.retain(|r| *r != room);
        }

        // The reply first, so a joiner sees its response before the catch-up
        // that follows it.
        if let Some(reply) = outcome.reply
            && send(&outgoing, &reply).is_err()
        {
            break;
        }

        if let Some(message) = outcome.broadcast {
            let ProtocolMessage::DocUpdate { room_id, .. } = &message else {
                continue;
            };
            let room_id = room_id.clone();

            if outcome.joined.is_some() {
                // A catch-up belongs to the joiner alone — it is the state they
                // were missing, not news for the room.
                if send(&outgoing, &message).is_err() {
                    break;
                }
                continue;
            }

            let Some(room) = Room::parse(&room_id) else {
                continue;
            };
            match encode_frames(&message) {
                // After the commit, and to everyone but the sender.
                Ok(frames) => {
                    state.rooms.broadcast_many(&room, who, &frames);
                }
                Err(_) => {
                    // **The update is durable but exceeds the bounded
                    // fragmented-update path.** The room's other members will
                    // not receive it and would silently fall behind without a
                    // notice.
                    //
                    // `RejoinSuggested` is the protocol's own answer: it tells
                    // them to rejoin, which fetches a snapshot and closes the
                    // gap. Losing the delta costs a round trip; staying quiet
                    // would cost correctness.
                    //
                    // Ordinary updates above 256 KiB have already been split by
                    // `encode_frames`; this remains the fallback above 16 MiB.
                    let notice = ProtocolMessage::RoomError {
                        crdt: crdt_of(&message),
                        room_id,
                        code: RoomErrorCode::RejoinSuggested,
                        message: "an update was too large to relay; rejoin to catch up".into(),
                    };
                    if let Ok(frame) = encoding::encode(&notice) {
                        state.rooms.broadcast(&room, who, frame);
                    }
                }
            }
        }
    }

    // However this ended.
    state.rooms.leave_all(who);
    drop(outgoing);
    let _ = writer.await;
    tracing::debug!(
        connection = who.0,
        rooms = joined.len(),
        ended,
        age_ms = opened.elapsed().as_millis() as u64,
        frames = seen.frames,
        last_frame = seen.last,
        sent_leave = seen.leave,
        sent_close = seen.close,
        "sync connection closed"
    );
}

/// What a connection has sent: how many frames, the kind of the last, when,
/// and whether it said goodbye.
struct Seen {
    frames: u64,
    last: &'static str,
    at: std::time::Instant,
    leave: bool,
    close: bool,
}

impl Default for Seen {
    fn default() -> Self {
        Self {
            frames: 0,
            last: "none",
            at: std::time::Instant::now(),
            leave: false,
            close: false,
        }
    }
}

impl Seen {
    fn note(&mut self, kind: &'static str) {
        self.frames += 1;
        self.last = kind;
        self.at = std::time::Instant::now();
        self.leave |= kind == "leave";
    }
}

/// The kind of a decoded frame, by name and without formatting its payload.
fn frame_kind(message: &ProtocolMessage) -> &'static str {
    match message {
        ProtocolMessage::JoinRequest { .. } => "join",
        ProtocolMessage::Leave { .. } => "leave",
        ProtocolMessage::DocUpdate { .. } => "update",
        _ => "other",
    }
}

fn crdt_of(message: &ProtocolMessage) -> loro_protocol::protocol::CrdtType {
    match message {
        ProtocolMessage::DocUpdate { crdt, .. } => *crdt,
        _ => loro_protocol::protocol::CrdtType::Loro,
    }
}

fn accept_fragment(
    message: ProtocolMessage,
    pending: &mut Option<IncomingFragments>,
) -> Result<Option<ProtocolMessage>, ProtocolMessage> {
    match message {
        ProtocolMessage::DocUpdateFragmentHeader {
            crdt,
            room_id,
            batch_id,
            fragment_count,
            total_size_bytes,
        } => {
            let total = usize::try_from(total_size_bytes).unwrap_or(usize::MAX);
            if fragment_count == 0 || total > MAX_FRAGMENTED_UPDATE_BYTES {
                *pending = None;
                return Err(fragment_error(
                    crdt,
                    room_id,
                    "fragmented update exceeds the store limit",
                ));
            }
            *pending = Some(IncomingFragments {
                crdt,
                room: room_id,
                batch: batch_id,
                count: fragment_count,
                total,
                next: 0,
                bytes: Vec::new(),
            });
            Ok(None)
        }
        ProtocolMessage::DocUpdateFragment {
            crdt,
            room_id,
            batch_id,
            index,
            fragment,
        } => {
            let Some(mut assembling) = pending.take() else {
                return Err(fragment_error(
                    crdt,
                    room_id,
                    "update fragment arrived without its header",
                ));
            };
            if assembling.crdt != crdt
                || assembling.room != room_id
                || assembling.batch != batch_id
                || assembling.next != index
                || index >= assembling.count
                || assembling.bytes.len().saturating_add(fragment.len()) > assembling.total
            {
                return Err(fragment_error(
                    crdt,
                    room_id,
                    "fragmented update was inconsistent or out of order",
                ));
            }
            assembling.bytes.extend_from_slice(&fragment);
            assembling.next += 1;
            if assembling.next == assembling.count {
                if assembling.bytes.len() != assembling.total {
                    return Err(fragment_error(
                        crdt,
                        room_id,
                        "fragmented update did not match its declared size",
                    ));
                }
                Ok(Some(ProtocolMessage::DocUpdate {
                    crdt,
                    room_id,
                    updates: vec![assembling.bytes],
                    batch_id,
                }))
            } else {
                *pending = Some(assembling);
                Ok(None)
            }
        }
        other if pending.is_some() => {
            let (crdt, room_id) = message_identity(&other);
            *pending = None;
            Err(fragment_error(
                crdt,
                room_id,
                "fragmented update was interrupted by another message",
            ))
        }
        other => Ok(Some(other)),
    }
}

fn message_identity(message: &ProtocolMessage) -> (CrdtType, String) {
    match message {
        ProtocolMessage::JoinRequest { crdt, room_id, .. }
        | ProtocolMessage::JoinResponseOk { crdt, room_id, .. }
        | ProtocolMessage::JoinError { crdt, room_id, .. }
        | ProtocolMessage::DocUpdate { crdt, room_id, .. }
        | ProtocolMessage::DocUpdateFragmentHeader { crdt, room_id, .. }
        | ProtocolMessage::DocUpdateFragment { crdt, room_id, .. }
        | ProtocolMessage::RoomError { crdt, room_id, .. }
        | ProtocolMessage::Ack { crdt, room_id, .. }
        | ProtocolMessage::Leave { crdt, room_id } => (*crdt, room_id.clone()),
    }
}

fn fragment_error(crdt: CrdtType, room_id: String, message: &str) -> ProtocolMessage {
    ProtocolMessage::RoomError {
        crdt,
        room_id,
        code: RoomErrorCode::Unknown,
        message: message.to_owned(),
    }
}

fn encode_frames(message: &ProtocolMessage) -> Result<Vec<Vec<u8>>, ()> {
    if let Ok(frame) = encoding::encode(message) {
        return Ok(vec![frame]);
    }
    let ProtocolMessage::DocUpdate {
        crdt,
        room_id,
        updates,
        batch_id,
    } = message
    else {
        return Err(());
    };
    let [update] = updates.as_slice() else {
        return Err(());
    };
    if update.len() > MAX_FRAGMENTED_UPDATE_BYTES {
        return Err(());
    }
    let count = update.len().div_ceil(FRAGMENT_BYTES) as u64;
    let mut frames = Vec::with_capacity(count as usize + 1);
    frames.push(
        encoding::encode(&ProtocolMessage::DocUpdateFragmentHeader {
            crdt: *crdt,
            room_id: room_id.clone(),
            batch_id: *batch_id,
            fragment_count: count,
            total_size_bytes: update.len() as u64,
        })
        .map_err(|_| ())?,
    );
    for (index, fragment) in update.chunks(FRAGMENT_BYTES).enumerate() {
        frames.push(
            encoding::encode(&ProtocolMessage::DocUpdateFragment {
                crdt: *crdt,
                room_id: room_id.clone(),
                batch_id: *batch_id,
                index: index as u64,
                fragment: fragment.to_vec(),
            })
            .map_err(|_| ())?,
        );
    }
    Ok(frames)
}

/// Queue a message for the socket's single writer.
///
/// A message that will not encode is dropped rather than closing the
/// connection: the caller is mid-flow, and the alternative — tearing down a
/// working socket because one reply was oversized — is worse than the reply
/// going missing. Anything load-bearing enough to matter is a `DocUpdate`,
/// which is handled above.
fn send(to: &mpsc::UnboundedSender<Vec<u8>>, message: &ProtocolMessage) -> Result<(), ()> {
    let Ok(frames) = encode_frames(message) else {
        return Ok(());
    };
    for frame in frames {
        to.send(frame).map_err(|_| ())?;
    }
    Ok(())
}
