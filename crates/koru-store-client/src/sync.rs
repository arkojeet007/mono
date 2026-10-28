// SPDX-License-Identifier: MPL-2.0

//! The WebSocket half: joining rooms and exchanging updates.
//!
//! ## What an acknowledgement is worth
//!
//! An [`Ack`] with `durable == true` means the store has **committed the update
//! to disk**. Not received it, not queued it, not replicated it. A client may
//! stop retrying on the strength of one; it may not conclude anything about any
//! other machine.
//!
//! ## Reconnecting
//!
//! There is no client-side journal. On reconnect a client sends its version
//! vector in the `JoinRequest` and the store replies with everything it is
//! missing; anything the *client* holds that the store does not is resent as an
//! ordinary update. That is ordinary Loro synchronisation doing the work, which
//! is why no custom recovery message exists.
//!
//! ## Blocking, with a read timeout
//!
//! [`Connection::next_event`] blocks until a frame arrives or the timeout
//! passes. A caller drives it from a background thread and forwards events over
//! a channel.

use std::net::TcpStream;
use std::time::Duration;

use koru_store_core::errors::{Code, Error, Result};
use loro_protocol::encoding;
use loro_protocol::protocol::{
    CrdtType, JoinErrorCode, Permission, ProtocolMessage, RoomErrorCode, UpdateStatusCode,
};

// A caller that keeps the batches it sent in flight names them by this.
pub use loro_protocol::protocol::BatchId;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::http::Discovery;

/// Leave room for the protocol header, room id and varints beneath Loro's
/// 256 KiB encoded-message ceiling. The update itself is split; the WebSocket
/// frame limit is deliberately not raised.
const FRAGMENT_BYTES: usize = loro_protocol::protocol::MAX_MESSAGE_SIZE - 4 * 1024;
/// A corrupt or hostile peer must not make one connection reserve unbounded
/// memory while assembling an update. Ordinary document deltas are far below
/// this; file bytes travel through the blob API and never through this path.
const MAX_FRAGMENTED_UPDATE_BYTES: usize = 16 * 1024 * 1024;

/// What the store said about an update.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ack {
    pub batch: BatchId,
    /// `true` only for [`UpdateStatusCode::Ok`], which means committed to disk.
    pub durable: bool,
    pub status: UpdateStatusCode,
}

/// Something that arrived from the store.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// The room was joined. `version` is the store's position.
    Joined {
        room: String,
        permission: Permission,
        version: Vec<u8>,
    },
    /// The join was refused.
    JoinRefused {
        room: String,
        code: JoinErrorCode,
        /// Koru's stable token, when the store had one to give — which the
        /// codec only carries beside `JoinErrorCode::AppError`.
        app_code: Option<String>,
        message: String,
    },
    /// Updates to apply. From a catch-up on join, or from another client.
    Updates { room: String, updates: Vec<Vec<u8>> },
    /// The store answered an update this client sent.
    Acked { room: String, ack: Ack },
    /// Something went wrong with a room. `RejoinSuggested` means this client
    /// has fallen behind and should rejoin to catch up.
    RoomProblem {
        room: String,
        code: RoomErrorCode,
        message: String,
    },
}

/// One connection to a store, carrying any number of rooms.
pub struct Connection {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_batch: u64,
    fragments: Option<IncomingFragments>,
}

struct IncomingFragments {
    crdt: CrdtType,
    room: String,
    batch: BatchId,
    count: u64,
    total: usize,
    next: u64,
    bytes: Vec<u8>,
}

impl Connection {
    /// Open the socket, authenticating on the upgrade.
    ///
    /// The token is an ordinary HTTP header on the request that becomes the
    /// socket — nothing about authentication is in the protocol, which is what
    /// lets a stock Loro client connect the same way.
    pub fn open(discovery: &Discovery) -> Result<Self> {
        use tungstenite::client::IntoClientRequest;

        let mut request = discovery
            .sync_url()
            .into_client_request()
            .map_err(|e| Error::new(Code::InvalidRequest, format!("sync url: {e}")))?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", discovery.token).parse().map_err(|_| {
                Error::new(Code::Unauthenticated, "the token is not a header value")
            })?,
        );

        let (socket, _response) = tungstenite::connect(request)
            .map_err(|e| Error::new(Code::StorageFailed, format!("could not connect: {e}")))?;
        Ok(Self {
            socket,
            next_batch: 1,
            fragments: None,
        })
    }

    /// How long to wait for a frame before [`next_event`] gives up.
    ///
    /// [`next_event`]: Self::next_event
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        let stream = match self.socket.get_ref() {
            MaybeTlsStream::Plain(stream) => stream,
            _ => return Ok(()),
        };
        stream
            .set_read_timeout(timeout)
            .map_err(|e| Error::new(Code::StorageFailed, format!("read timeout: {e}")))
    }

    /// Ask to join a room, telling the store what this client already has.
    ///
    /// A client with no history sends an **empty** version vector and gets
    /// everything. That is the only form that applies to an empty replica: a
    /// delta from a non-empty version names causal ancestors it cannot obtain,
    /// and Loro parks such an update as pending — `import` returns `Ok` and
    /// nothing changes, with no error to notice.
    pub fn join(&mut self, room: &str, version: &loro::VersionVector) -> Result<()> {
        let encoded = if version.is_empty() {
            Vec::new()
        } else {
            version.encode()
        };
        self.send(ProtocolMessage::JoinRequest {
            crdt: CrdtType::Loro,
            room_id: room.to_string(),
            auth: Vec::new(),
            version: encoded,
        })
    }

    pub fn leave(&mut self, room: &str) -> Result<()> {
        self.send(ProtocolMessage::Leave {
            crdt: CrdtType::Loro,
            room_id: room.to_string(),
        })
    }

    /// Send an update. Returns the batch id its acknowledgement will carry.
    pub fn send_update(&mut self, room: &str, update: Vec<u8>) -> Result<BatchId> {
        let batch = BatchId(self.next_batch.to_le_bytes());
        self.next_batch += 1;
        self.send_update_as(room, update, batch)?;
        Ok(batch)
    }

    /// Send an update under a batch id the **caller** chose.
    ///
    /// For a sender that is not on this thread. A client that queues updates to
    /// a socket thread still has to answer "which batch is this" synchronously —
    /// the caller needs the id to record what it is owed — so the id cannot be
    /// minted where the write happens. Allocating it outside also keeps ids
    /// monotonic across a reconnect, which is what stops a late acknowledgement
    /// from a dead socket being mistaken for a new one.
    ///
    /// Mixing the two on one connection would repeat an id. Pick one.
    pub fn send_update_as(&mut self, room: &str, update: Vec<u8>, batch: BatchId) -> Result<()> {
        let message = ProtocolMessage::DocUpdate {
            crdt: CrdtType::Loro,
            room_id: room.to_string(),
            updates: vec![update.clone()],
            batch_id: batch,
        };
        if let Ok(frame) = encoding::encode(&message) {
            return self.send_frame(frame);
        }
        self.send_fragmented_update(room, update, batch)
    }

    /// A handle that can break this connection's blocking read from another
    /// thread.
    ///
    /// **A blocking `read` cannot be interrupted**, so a socket thread only
    /// notices a shutdown when its read times out. Shutting the underlying
    /// stream down makes it return at once, which is the difference between a
    /// teardown bounded by the read timeout and one that is instant — and a
    /// workspace switch drops every session together.
    ///
    /// `None` for a stream that is not plain TCP, the same match
    /// [`set_read_timeout`](Self::set_read_timeout) makes, and for the same
    /// reason: there is nothing underneath to reach.
    pub fn abort_handle(&self) -> Option<TcpStream> {
        match self.socket.get_ref() {
            MaybeTlsStream::Plain(stream) => stream.try_clone().ok(),
            _ => None,
        }
    }

    /// Wait for the next event.
    ///
    /// `Ok(None)` means the read timed out — normal, and not an error. An error
    /// means the connection is finished.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        loop {
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(tungstenite::Error::Io(e)) => {
                    return Err(Error::new(
                        Code::StorageFailed,
                        format!(
                            "the connection ended: I/O failure ({:?}, OS {:?}): {e}",
                            e.kind(),
                            e.raw_os_error()
                        ),
                    ));
                }
                Err(e) => {
                    return Err(Error::new(
                        Code::StorageFailed,
                        format!("the connection ended: {e}"),
                    ));
                }
            };

            let bytes = match message {
                Message::Binary(bytes) => bytes,
                Message::Close(_) => {
                    return Err(Error::new(
                        Code::StorageFailed,
                        "the store closed the connection",
                    ));
                }
                // Ping, Pong, Text and Frame are not protocol traffic.
                _ => continue,
            };

            let Some(decoded) = encoding::try_decode(&bytes) else {
                // A frame this build cannot read is skipped rather than fatal:
                // a newer store may send a message type added since, and
                // dropping the connection over it would be worse than ignoring
                // one frame.
                continue;
            };
            let Some(decoded) = self.accept_fragment(decoded)? else {
                continue;
            };
            if let Some(event) = translate(decoded) {
                return Ok(Some(event));
            }
        }
    }

    fn send_fragmented_update(
        &mut self,
        room: &str,
        update: Vec<u8>,
        batch: BatchId,
    ) -> Result<()> {
        if update.len() > MAX_FRAGMENTED_UPDATE_BYTES {
            return Err(Error::new(
                Code::PayloadTooLarge,
                format!(
                    "sync update is {} bytes; fragmented updates are limited to {} bytes",
                    update.len(),
                    MAX_FRAGMENTED_UPDATE_BYTES
                ),
            ));
        }
        let count = update.len().div_ceil(FRAGMENT_BYTES) as u64;
        self.send(ProtocolMessage::DocUpdateFragmentHeader {
            crdt: CrdtType::Loro,
            room_id: room.to_owned(),
            batch_id: batch,
            fragment_count: count,
            total_size_bytes: update.len() as u64,
        })?;
        for (index, fragment) in update.chunks(FRAGMENT_BYTES).enumerate() {
            self.send(ProtocolMessage::DocUpdateFragment {
                crdt: CrdtType::Loro,
                room_id: room.to_owned(),
                batch_id: batch,
                index: index as u64,
                fragment: fragment.to_vec(),
            })?;
        }
        Ok(())
    }

    fn accept_fragment(&mut self, message: ProtocolMessage) -> Result<Option<ProtocolMessage>> {
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
                    self.fragments = None;
                    return Err(Error::new(
                        Code::PayloadTooLarge,
                        format!(
                            "invalid fragmented sync update: {fragment_count} fragment(s), {total_size_bytes} bytes"
                        ),
                    ));
                }
                self.fragments = Some(IncomingFragments {
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
                let Some(mut pending) = self.fragments.take() else {
                    return Err(Error::new(
                        Code::CorruptState,
                        "sync update fragment arrived without its header",
                    ));
                };
                if pending.crdt != crdt
                    || pending.room != room_id
                    || pending.batch != batch_id
                    || pending.next != index
                    || index >= pending.count
                    || pending.bytes.len().saturating_add(fragment.len()) > pending.total
                {
                    return Err(Error::new(
                        Code::CorruptState,
                        "fragmented sync update was inconsistent or out of order",
                    ));
                }
                pending.bytes.extend_from_slice(&fragment);
                pending.next += 1;
                if pending.next == pending.count {
                    if pending.bytes.len() != pending.total {
                        return Err(Error::new(
                            Code::CorruptState,
                            "fragmented sync update did not match its declared size",
                        ));
                    }
                    Ok(Some(ProtocolMessage::DocUpdate {
                        crdt,
                        room_id,
                        updates: vec![pending.bytes],
                        batch_id,
                    }))
                } else {
                    self.fragments = Some(pending);
                    Ok(None)
                }
            }
            _other if self.fragments.is_some() => {
                self.fragments = None;
                Err(Error::new(
                    Code::CorruptState,
                    "fragmented sync update was interrupted by another message",
                ))
            }
            other => Ok(Some(other)),
        }
    }

    fn send(&mut self, message: ProtocolMessage) -> Result<()> {
        let frame = encoding::encode(&message)
            .map_err(|e| Error::new(Code::PayloadTooLarge, format!("could not encode: {e}")))?;
        self.send_frame(frame)
    }

    fn send_frame(&mut self, frame: Vec<u8>) -> Result<()> {
        self.socket
            .send(Message::Binary(frame.into()))
            .map_err(|e| Error::new(Code::StorageFailed, format!("could not send: {e}")))
    }

    /// Close politely. A dropped connection is also fine for the store, which
    /// leaves every room a socket was in when it goes, however it goes — but
    /// it is not fine for the connection: a socket dropped with the store's
    /// replies still unread is reset on Windows, and the gateway logs
    /// *forcibly closed* for it. This sends the Close frame and then reads
    /// what the store still has to say until it answers in kind, for at most
    /// `budget`, so the socket is empty when it goes.
    ///
    /// `true` when the store's Close came back, or the socket was already
    /// gone; `false` when the budget ran out first.
    pub fn close(&mut self, budget: Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        // Short reads, so the budget is honoured whatever the store is doing.
        let _ = self.set_read_timeout(Some(Duration::from_millis(20)));
        if self.socket.close(None).is_err() {
            return true;
        }
        loop {
            match self.socket.read() {
                // The store's own Close, or the socket already closed: done.
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return true;
                }
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return true,
                // A frame still in flight: read and let go.
                Ok(_) => {}
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
        }
    }
}

fn translate(message: ProtocolMessage) -> Option<Event> {
    Some(match message {
        ProtocolMessage::JoinResponseOk {
            room_id,
            permission,
            version,
            ..
        } => Event::Joined {
            room: room_id,
            permission,
            version,
        },
        ProtocolMessage::JoinError {
            room_id,
            code,
            message,
            app_code,
            ..
        } => Event::JoinRefused {
            room: room_id,
            code,
            app_code,
            message,
        },
        ProtocolMessage::DocUpdate {
            room_id, updates, ..
        } => Event::Updates {
            room: room_id,
            updates,
        },
        ProtocolMessage::Ack {
            room_id,
            ref_id,
            status,
            ..
        } => Event::Acked {
            room: room_id,
            ack: Ack {
                batch: ref_id,
                // **Only `Ok` means durable.** Every other status is a refusal
                // of some kind, and treating one as an acknowledgement is how a
                // client discards work the store never wrote.
                durable: status == UpdateStatusCode::Ok,
                status,
            },
        },
        ProtocolMessage::RoomError {
            room_id,
            code,
            message,
            ..
        } => Event::RoomProblem {
            room: room_id,
            code,
            message,
        },
        // Requests and fragments are not things a store sends to a client.
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
