// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::net::{IpAddr, SocketAddr};
use std::ops::ControlFlow;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::{SinkExt, Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, DeviceId, DeviceRole, ErrorBody, ItemKind, MessageId,
    PeerInfo, QueuedEnvelope, FRAME_HARD_CAP,
};

use crate::auth::verify_token;
use crate::connection_limits::{ConnectionRefused, ConnectionSlot};
use crate::state::RoomPresence;
use crate::RelayState;

/// Outbound messages queued per socket before senders wait for its writer.
const OUTBOUND_QUEUE_CAPACITY: usize = 256;
/// How long a closing socket's writer may take to flush its queue and close
/// frame. A peer that still reads needs far less; a writer still blocked after
/// this writes to a peer that stopped reading, so it is aborted, which frees
/// the queue and closes the TCP connection.
const WRITER_CLOSE_GRACE: Duration = Duration::from_secs(5);
/// The same grace for a socket that never became a room member: enough to
/// flush a close frame to a client that still reads, while an evicted or
/// expired socket gives back its slot almost at once, so closing sockets
/// cannot fill the slots kept for authenticated ones.
const NON_MEMBER_CLOSE_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub enum WsOut {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// Upgrade to a WebSocket once the client and the relay have a free
/// connection slot; the slot is held until the socket closes.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    State(state): State<Arc<RelayState>>,
) -> Response {
    let client = state.client_ip(peer.ip(), &headers);
    let slot = match state.acquire_connection(client) {
        Ok(slot) => slot,
        Err(refused) => return connection_refused(refused),
    };
    ws.max_message_size(FRAME_HARD_CAP)
        .max_frame_size(FRAME_HARD_CAP)
        .on_upgrade(move |socket| handle_socket(socket, state, q.token, client, slot))
}

fn connection_refused(refused: ConnectionRefused) -> Response {
    let (status, code, message) = match refused {
        ConnectionRefused::RelayFull => (
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_full",
            "the relay cannot accept more connections right now; try again later",
        ),
        ConnectionRefused::ClientFull => (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_connections",
            "too many connections from this address; close other connections and try again",
        ),
    };
    let body = ErrorBody {
        code: code.into(),
        message: message.into(),
    };
    (status, Json(body)).into_response()
}

struct ConnCtx {
    device_id: String,
    room_id: String,
    pairing_sid: Option<String>,
    device_role: DeviceRole,
    pairing_side: Option<usize>,
    /// The client address the socket came from (see `RelayState::client_ip`).
    client: IpAddr,
    /// Set for device-token sockets: keeps the room from being evicted.
    presence: Option<RoomPresence>,
    /// Set for pairing-token sockets: when their pairing session expires.
    pairing_deadline: Option<tokio::time::Instant>,
}

impl ConnCtx {
    /// A device-token socket of a room member. Only these may wait for queue
    /// space and hold a connection slot that cannot be evicted; pairing tokens
    /// are issued without authentication for any device ID.
    fn is_member(&self) -> bool {
        self.presence.is_some()
    }
}

fn is_member(ctx: &Option<ConnCtx>) -> bool {
    ctx.as_ref().is_some_and(ConnCtx::is_member)
}

// A protocol-v1 Ed25519 SPAKE message is 33 bytes (44 in base64). Retain only
// its first public message while the authenticated peer opens its WebSocket.
const PAIRING_SPAKE_MAX_BYTES: usize = 1024;

struct PairingArrival {
    joined: Option<(mpsc::Sender<WsOut>, PeerInfo)>,
    pending: Option<ControlMessage>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

fn register_pairing_connection(
    state: &RelayState,
    conn: &mut ConnCtx,
    tx: &mpsc::Sender<WsOut>,
    role: ClientRole,
) -> Result<PairingArrival, String> {
    let sid = conn
        .pairing_sid
        .as_deref()
        .ok_or("pairing session required")?;
    let mut session = state.pairing_mut(sid).ok_or("unknown pairing session")?;
    if session.room_id.as_str() != conn.room_id {
        return Err("pairing room mismatch".into());
    }
    if session.expires_at < chrono::Utc::now() {
        session.pending_spake = [None, None];
        return Err("pairing expired".into());
    }
    if session.completed {
        return Err("pairing completed".into());
    }
    let side = match (role, conn.device_role) {
        (ClientRole::PairingA, DeviceRole::Owner)
            if conn.device_id == session.creator.device_id.as_str() =>
        {
            0
        }
        (ClientRole::PairingB, DeviceRole::Member)
            if session
                .joiner
                .as_ref()
                .is_some_and(|peer| peer.device_id.as_str() == conn.device_id) =>
        {
            1
        }
        _ => return Err("pairing role or device mismatch".into()),
    };
    // Registration, presence and the pending-message take share the session
    // lock with forwarding, so an arriving peer cannot miss an early SPAKE.
    if session.pairing_senders[side].is_some() {
        session.pending_spake = [None, None];
    }
    state.register_conn(&conn.device_id, tx.clone());
    session.pairing_senders[side] = Some(tx.clone());
    conn.pairing_side = Some(side);
    let peer_online = session.pairing_senders[1 - side]
        .as_ref()
        .is_some_and(|sender| !sender.is_closed());
    let joined = if peer_online {
        session.joiner.clone().map(|mut peer| {
            peer.online = true;
            (session.pairing_senders[0].as_ref().unwrap().clone(), peer)
        })
    } else {
        None
    };
    let pending = if peer_online {
        session.pending_spake[1 - side]
            .take()
            .map(|message| ControlMessage::Spake2Msg {
                pairing_session_id: session.session_id.clone(),
                message,
            })
    } else {
        None
    };
    Ok(PairingArrival {
        joined,
        pending,
        expires_at: session.expires_at,
    })
}

async fn handle_socket(
    socket: WebSocket,
    state: Arc<RelayState>,
    token_q: Option<String>,
    client: IpAddr,
    slot: ConnectionSlot,
) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<WsOut>(OUTBOUND_QUEUE_CAPACITY);

    let mut writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let r = match out {
                WsOut::Text(t) => sink.send(Message::Text(t.into())).await,
                WsOut::Binary(b) => sink.send(Message::Binary(b.into())).await,
                WsOut::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            };
            if r.is_err() {
                break;
            }
        }
    });

    let ctx = read_loop(&mut stream, &state, &tx, token_q.as_deref(), client, &slot).await;
    let close_grace = if is_member(&ctx) {
        WRITER_CLOSE_GRACE
    } else {
        NON_MEMBER_CLOSE_GRACE
    };

    if let Some(c) = ctx {
        forget_socket(&state, &tx, &c).await;
    }
    // A socket whose client stopped reading may have a full queue behind a
    // writer blocked on its sink, so the close is queued without waiting.
    let close_queued = tx.try_send(WsOut::Close).is_ok();
    if !close_queued
        || tokio::time::timeout(close_grace, &mut writer)
            .await
            .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    drop(slot);
}

/// Removes a closing socket's routes and tells its room that its device left,
/// unless a replacement socket already routes for that device.
async fn forget_socket(state: &Arc<RelayState>, tx: &mpsc::Sender<WsOut>, c: &ConnCtx) {
    state
        .connections
        .remove_if(&c.device_id, |_, sender| sender.same_channel(tx));
    if let (Some(sid), Some(side)) = (&c.pairing_sid, c.pairing_side) {
        if let Some(mut session) = state.pairing_mut(sid) {
            if session.pairing_senders[side]
                .as_ref()
                .is_some_and(|sender| sender.same_channel(tx))
            {
                session.pairing_senders[side] = None;
                session.pending_spake = [None, None];
            }
        }
    }
    // A replacement may already own this device's routing entry. Its
    // peers must stay online when the superseded socket finishes closing.
    if !state.is_online(&c.device_id) {
        notify_peers_left(state, c).await;
    }
}

/// Reads and handles the socket's messages until it closes, misses a
/// deadline, sends too fast or, while not a room member, is evicted to admit
/// a newer socket. Returns the context, if any, for cleanup.
///
/// A socket that is not a room member (before `hello`, or holding a pairing
/// token) has a fixed deadline: the hello deadline, then its pairing
/// session's expiry. It stays evictable, and its messages are handled under
/// that deadline and eviction, so a client that never reads cannot stall the
/// loop past either. `hello` itself never waits (see `handle_control_from`),
/// so cancelling a handler never leaves a half-registered socket.
async fn read_loop<S>(
    stream: &mut S,
    state: &Arc<RelayState>,
    tx: &mpsc::Sender<WsOut>,
    token_q: Option<&str>,
    client: IpAddr,
    slot: &ConnectionSlot,
) -> Option<ConnCtx>
where
    S: Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut ctx: Option<ConnCtx> = None;
    let mut last_msg = Instant::now();
    let mut burst = 0u32;
    let deadlines = state.socket_deadlines;
    let hello_deadline = tokio::time::Instant::now() + deadlines.hello;

    loop {
        let bound = match &ctx {
            None => Some(hello_deadline),
            Some(c) if c.is_member() => None,
            Some(c) => Some(
                c.pairing_deadline
                    .unwrap_or(hello_deadline)
                    .min(tokio::time::Instant::now() + deadlines.read_idle),
            ),
        };
        let received = match bound {
            Some(deadline) => tokio::select! {
                received = tokio::time::timeout_at(deadline, stream.next()) => received,
                () = slot.evicted() => {
                    debug!("closing a non-member websocket to admit a newer one");
                    break;
                }
            },
            None => tokio::time::timeout(deadlines.read_idle, stream.next()).await,
        };
        let msg = match received {
            Ok(Some(Ok(msg))) => msg,
            Ok(_) => break,
            Err(_) => {
                debug!(authenticated = ctx.is_some(), "closing idle websocket");
                break;
            }
        };
        if let Some(presence) = ctx.as_ref().and_then(|c| c.presence.as_ref()) {
            presence.touch();
        }
        if message_rate_exceeded(&mut last_msg, &mut burst, Instant::now()) {
            // The socket closes next; never wait for room in its queue.
            try_send_msg(tx, &error_message("rate_limited", "too many messages"));
            break;
        }
        let was_member = is_member(&ctx);
        let handled = handle_message(state, tx, &mut ctx, msg, token_q, client);
        let outcome = match bound {
            Some(deadline) => tokio::select! {
                outcome = handled => outcome,
                () = tokio::time::sleep_until(deadline) => {
                    debug!("closing a non-member websocket stalled past its deadline");
                    break;
                }
                () = slot.evicted() => {
                    debug!("closing a stalled non-member websocket to admit a newer one");
                    break;
                }
            },
            None => handled.await,
        };
        if outcome.is_break() {
            break;
        }
        // A member socket can no longer be evicted; one evicted while its
        // `hello` was being handled closes like any other evicted socket.
        if !was_member && is_member(&ctx) && !slot.authenticate() {
            debug!("closing a websocket evicted while it authenticated");
            break;
        }
    }
    ctx
}

/// Handles one message; `Break` closes the socket.
async fn handle_message(
    state: &Arc<RelayState>,
    tx: &mpsc::Sender<WsOut>,
    ctx: &mut Option<ConnCtx>,
    msg: Message,
    token_q: Option<&str>,
    client: IpAddr,
) -> ControlFlow<()> {
    match msg {
        Message::Text(t) => {
            let parsed = match ControlMessage::decode(&t) {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "bad control");
                    return ControlFlow::Continue(());
                }
            };
            if let Err(e) = handle_control_from(state, tx, ctx, parsed, token_q, client).await {
                reply(tx, is_member(ctx), &error_message("error", &e)).await;
            }
        }
        Message::Binary(b) => {
            if let Some(c) = ctx.as_ref() {
                forward_binary(state, c, b.to_vec(), tx).await;
            }
        }
        Message::Ping(_) => {
            if is_member(ctx) {
                let _ = tx.send(WsOut::Binary(Vec::new())).await;
            } else {
                let _ = tx.try_send(WsOut::Binary(Vec::new()));
            }
        }
        Message::Close(_) => return ControlFlow::Break(()),
        _ => {}
    }
    ControlFlow::Continue(())
}

fn message_rate_exceeded(last: &mut Instant, burst: &mut u32, now: Instant) -> bool {
    if now.duration_since(*last) < Duration::from_millis(20) {
        *burst += 1;
        *burst > 80
    } else {
        *burst = 0;
        *last = now;
        false
    }
}

/// Handles one control message from a socket at client address `client`.
async fn handle_control_from(
    state: &Arc<RelayState>,
    tx: &mpsc::Sender<WsOut>,
    ctx: &mut Option<ConnCtx>,
    msg: ControlMessage,
    token_q: Option<&str>,
    client: IpAddr,
) -> Result<(), String> {
    match msg {
        ControlMessage::Hello {
            protocol_version,
            device_id,
            device_name,
            os,
            role,
            room_id,
            token,
            pairing_session_id,
        } => {
            if ctx.is_some() {
                return Err("hello already received".into());
            }
            if protocol_version != clipsync_protocol::PROTOCOL_VERSION {
                return Err("unsupported protocol version".into());
            }
            let token = if token.is_empty() {
                token_q.unwrap_or("").to_string()
            } else {
                token
            };
            let claims =
                verify_token(&state.config.token_secret, &token).map_err(|e| e.to_string())?;
            if claims.did != device_id {
                return Err("token device mismatch".into());
            }
            if let Some(rid) = &room_id {
                if *rid != claims.rid {
                    return Err("token room mismatch".into());
                }
            }
            if claims.typ == "pair"
                && pairing_session_id
                    .as_ref()
                    .is_some_and(|sid| Some(sid.as_str()) != claims.sid.as_deref())
            {
                return Err("token pairing session mismatch".into());
            }
            let mut conn = ConnCtx {
                device_id: device_id.as_str().to_string(),
                room_id: claims.rid.as_str().to_string(),
                pairing_sid: if claims.typ == "pair" {
                    claims.sid.clone()
                } else {
                    None
                },
                device_role: claims.role,
                pairing_side: None,
                client,
                presence: None,
                pairing_deadline: None,
            };
            if claims.typ != "pair" {
                conn.presence = Some(state.attach_member(&conn.room_id, &conn.device_id)?);
            }
            if claims.typ == "pair" {
                let arrival = register_pairing_connection(state, &mut conn, tx, role)?;
                let remaining = (arrival.expires_at - chrono::Utc::now())
                    .to_std()
                    .unwrap_or_default();
                conn.pairing_deadline = Some(tokio::time::Instant::now() + remaining);
                let hello_ok = ControlMessage::HelloOk {
                    room_id: claims.rid.clone(),
                    peer_online: false,
                    peers: Vec::new(),
                    queued_text: None,
                    device_token: None,
                };
                // Nothing in `hello` waits for queue space: it runs before the
                // socket's context exists, so it must never stall (see
                // `read_loop`). A client that reads has an empty queue here.
                try_send_msg(tx, &hello_ok);
                if let Some((creator, device)) = arrival.joined {
                    try_send_msg(&creator, &ControlMessage::PeerJoined { device });
                }
                if let Some(pending) = arrival.pending {
                    try_send_msg(tx, &pending);
                }
            } else {
                state.register_conn(&conn.device_id, tx.clone());
                let peers: Vec<PeerInfo> = state
                    .peer_ids(&conn.room_id, &conn.device_id)
                    .into_iter()
                    .filter_map(|id| {
                        let room = state.room(&conn.room_id)?;
                        let mut info = room.devices.get(&id)?.info.clone();
                        info.online = state.is_online(&id);
                        Some(info)
                    })
                    .collect();
                let peer_online = peers.iter().any(|p| p.online);
                let queued = state.take_queue(&conn.device_id);
                let hello_ok = ControlMessage::HelloOk {
                    room_id: claims.rid.clone(),
                    peer_online,
                    peers: peers.clone(),
                    queued_text: queued,
                    device_token: None,
                };
                try_send_msg(tx, &hello_ok);
                let joined = ControlMessage::PeerJoined {
                    device: PeerInfo {
                        device_id: device_id.clone(),
                        device_name,
                        os,
                        role: claims.role,
                        online: true,
                    },
                };
                fanout_except_now(state, &conn.room_id, &conn.device_id, &joined);
                state.webhook_spawn(serde_json::json!({
                    "type": "device.joined",
                    "room_id": conn.room_id,
                    "device_id": conn.device_id,
                    "item_kind": null,
                    "status": "ok",
                    "ts": chrono::Utc::now().to_rfc3339(),
                }));
            }
            *ctx = Some(conn);
            Ok(())
        }
        ControlMessage::Ping { ts } => {
            reply(tx, is_member(ctx), &ControlMessage::Pong { ts }).await;
            Ok(())
        }
        ControlMessage::Spake2Msg { .. }
        | ControlMessage::KeyConfirm { .. }
        | ControlMessage::DeviceBind { .. }
        | ControlMessage::NoiseHandshake { .. } => {
            let c = ctx.as_ref().ok_or("hello required")?;
            forward_pairing_or_peer(state, c, tx, msg).await
        }
        ControlMessage::TextEnvelope {
            routing,
            nonce,
            ciphertext,
        } => {
            let c = ctx.as_ref().ok_or("hello required")?;
            if routing.item_kind != ItemKind::Text {
                return Err("text envelope item_kind mismatch".into());
            }
            let peers = state.peer_ids(&c.room_id, &c.device_id);
            if peers.is_empty() {
                send_msg(
                    tx,
                    &ControlMessage::Ack {
                        message_id: routing.message_id.clone(),
                        transfer_id: Some(routing.transfer_id.clone()),
                        status: AckStatus::PeerOffline,
                        detail: None,
                    },
                )
                .await;
                return Ok(());
            }
            let mut any_online = false;
            let mut any_queued = false;
            let env = ControlMessage::TextEnvelope {
                routing: routing.clone(),
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
            };
            for peer in &peers {
                // Clone the sender: a DashMap guard held across `send().await`
                // would block every writer to its shard while the peer is slow.
                let online = state.connections.get(peer).map(|p| p.value().clone());
                if let Some(p) = online {
                    any_online = true;
                    if let Ok(s) = env.encode() {
                        let _ = p.send(WsOut::Text(s)).await;
                        state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    any_queued |= state.queue_text(
                        peer,
                        QueuedEnvelope {
                            routing: routing.clone(),
                            nonce: nonce.clone(),
                            ciphertext: ciphertext.clone(),
                        },
                        routing.expires_at,
                        c.client,
                    );
                }
            }
            let (status, detail) = if any_online {
                (AckStatus::Delivered, None)
            } else if any_queued {
                (AckStatus::Accepted, None)
            } else {
                (
                    AckStatus::PeerOffline,
                    Some("the relay's offline queue is full for this sender".into()),
                )
            };
            send_msg(
                tx,
                &ControlMessage::Ack {
                    message_id: routing.message_id,
                    transfer_id: Some(routing.transfer_id),
                    status,
                    detail,
                },
            )
            .await;
            state.webhook_spawn(serde_json::json!({
                "type": "clip.received",
                "room_id": c.room_id,
                "device_id": c.device_id,
                "item_kind": "text",
                "encrypted_size": ciphertext.len(),
                "status": "ok",
                "ts": chrono::Utc::now().to_rfc3339(),
            }));
            Ok(())
        }
        ControlMessage::Manifest { ref routing, .. } => {
            let c = ctx.as_ref().ok_or("hello required")?;
            let peers = state.peer_ids(&c.room_id, &c.device_id);
            let online: Vec<_> = peers
                .iter()
                .filter(|p| state.is_online(p))
                .cloned()
                .collect();
            if online.is_empty() {
                send_msg(
                    tx,
                    &ControlMessage::Ack {
                        message_id: routing.message_id.clone(),
                        transfer_id: Some(routing.transfer_id.clone()),
                        status: AckStatus::PeerOffline,
                        detail: Some("binary transfers require an online peer".into()),
                    },
                )
                .await;
                return Ok(());
            }
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
            send_msg(
                tx,
                &ControlMessage::Ack {
                    message_id: routing.message_id.clone(),
                    transfer_id: Some(routing.transfer_id.clone()),
                    status: AckStatus::Streaming,
                    detail: None,
                },
            )
            .await;
            Ok(())
        }
        ControlMessage::Ack { .. } | ControlMessage::EpochWrapMsg { .. } => {
            let c = ctx.as_ref().ok_or("hello required")?;
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
            Ok(())
        }
        ControlMessage::Revoke { ref device_id } => {
            let c = ctx.as_ref().ok_or("hello required")?;
            if c.device_role != DeviceRole::Owner {
                return Err("manage permission required".into());
            }
            // Revoke removes the routing entry. Keep the sender long enough
            // to close the authenticated socket after invalidating its device.
            let revoked_connection = state
                .connections
                .get(device_id.as_str())
                .map(|connection| connection.value().clone());
            if !state.revoke(&c.room_id, device_id.as_str()) {
                return Err("device not found".into());
            }
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
            if let Some(p) = revoked_connection {
                let _ = p.send(WsOut::Close).await;
            }
            state.webhook_spawn(serde_json::json!({
                "type": "room.revoked",
                "room_id": c.room_id,
                "device_id": device_id.as_str(),
                "status": "ok",
                "ts": chrono::Utc::now().to_rfc3339(),
            }));
            Ok(())
        }
        ControlMessage::Error { code, message } => {
            if let Some(c) = ctx {
                if let Some(sid) = &c.pairing_sid {
                    state.abort_pairing(sid);
                }
            }
            debug!(code, message, "client error");
            Ok(())
        }
        other => {
            let _ = other;
            Ok(())
        }
    }
}

async fn forward_pairing_or_peer(
    state: &Arc<RelayState>,
    c: &ConnCtx,
    tx: &mpsc::Sender<WsOut>,
    msg: ControlMessage,
) -> Result<(), String> {
    if let Some(sid) = &c.pairing_sid {
        let side = c
            .pairing_side
            .ok_or("authenticated pairing hello required")?;
        let message_sid = match &msg {
            ControlMessage::Spake2Msg {
                pairing_session_id, ..
            }
            | ControlMessage::KeyConfirm {
                pairing_session_id, ..
            }
            | ControlMessage::DeviceBind {
                pairing_session_id, ..
            } => pairing_session_id,
            _ => return Err("unexpected pairing frame".into()),
        };
        if message_sid.as_str() != sid {
            return Err("pairing frame session mismatch".into());
        }
        let senders = {
            let mut session = state.pairing_mut(sid).ok_or("unknown pairing session")?;
            if session.room_id.as_str() != c.room_id {
                return Err("pairing room mismatch".into());
            }
            if session.expires_at < chrono::Utc::now() {
                session.pending_spake = [None, None];
                return Err("pairing expired".into());
            }
            if session.completed {
                return Err("pairing completed".into());
            }
            if !session.pairing_senders[side]
                .as_ref()
                .is_some_and(|sender| sender.same_channel(tx))
            {
                return Err("pairing connection replaced".into());
            }
            if let ControlMessage::Spake2Msg { message, .. } = &msg {
                if message.len() > PAIRING_SPAKE_MAX_BYTES {
                    return Err("pairing SPAKE message too large".into());
                }
                if !session.pairing_senders[1 - side]
                    .as_ref()
                    .is_some_and(|sender| !sender.is_closed())
                {
                    match &session.pending_spake[side] {
                        Some(first) if first != message => {
                            return Err("pairing SPAKE already pending".into())
                        }
                        Some(_) => {}
                        None => session.pending_spake[side] = Some(message.clone()),
                    }
                    return Ok(());
                }
            }
            session.pairing_senders.clone()
        };
        if let Some(peer) = &senders[1 - side] {
            send_msg(peer, &msg).await;
        }
        if matches!(msg, ControlMessage::DeviceBind { .. }) {
            if let Some((owner_tok, member_tok, room_id, _, _)) =
                state.complete_pairing_bound(sid, Some(&c.room_id))
            {
                for (sender, device_token, role) in [
                    (&senders[0], owner_tok, DeviceRole::Owner),
                    (&senders[1], member_tok, DeviceRole::Member),
                ] {
                    if let Some(sender) = sender {
                        send_msg(
                            sender,
                            &ControlMessage::PairingComplete {
                                room_id: room_id.clone(),
                                epoch: 1,
                                device_token,
                                role,
                            },
                        )
                        .await;
                    }
                }
            }
        }
        return Ok(());
    }
    fanout_except(state, &c.room_id, &c.device_id, &msg).await;
    Ok(())
}

async fn forward_binary(
    state: &Arc<RelayState>,
    c: &ConnCtx,
    bytes: Vec<u8>,
    tx: &mpsc::Sender<WsOut>,
) {
    let peers = state.peer_ids(&c.room_id, &c.device_id);
    let mut sent = false;
    for peer in peers {
        let online = state.connections.get(&peer).map(|p| p.value().clone());
        if let Some(p) = online {
            let _ = p.send(WsOut::Binary(bytes.clone())).await;
            state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
            sent = true;
        }
    }
    if !sent {
        let ack = ControlMessage::Ack {
            message_id: MessageId::from("chunk"),
            transfer_id: None,
            status: AckStatus::PeerOffline,
            detail: None,
        };
        send_msg(tx, &ack).await;
    }
}

async fn fanout_except(state: &Arc<RelayState>, room_id: &str, except: &str, msg: &ControlMessage) {
    if let Ok(s) = msg.encode() {
        for peer in state.peer_ids(room_id, except) {
            let online = state.connections.get(&peer).map(|p| p.value().clone());
            if let Some(p) = online {
                let _ = p.send(WsOut::Text(s.clone())).await;
                state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// [`fanout_except`] without waiting: a peer whose queue is full misses `msg`.
fn fanout_except_now(state: &RelayState, room_id: &str, except: &str, msg: &ControlMessage) {
    if let Ok(s) = msg.encode() {
        for peer in state.peer_ids(room_id, except) {
            let online = state.connections.get(&peer).map(|p| p.value().clone());
            if online.is_some_and(|p| p.try_send(WsOut::Text(s.clone())).is_ok()) {
                state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Only a member's closing socket waits for its peers' queue space. A
/// non-member's slot stays evictable and bounded by its deadlines, so a peer
/// that stopped reading must not hold it open: that peer misses the notice.
async fn notify_peers_left(state: &Arc<RelayState>, c: &ConnCtx) {
    let msg = ControlMessage::PeerLeft {
        device_id: DeviceId::from(c.device_id.as_str()),
    };
    if c.is_member() {
        fanout_except(state, &c.room_id, &c.device_id, &msg).await;
    } else {
        fanout_except_now(state, &c.room_id, &c.device_id, &msg);
    }
}

async fn send_msg(tx: &mpsc::Sender<WsOut>, msg: &ControlMessage) {
    if let Ok(s) = msg.encode() {
        let _ = tx.send(WsOut::Text(s)).await;
    }
}

fn error_message(code: &str, message: &str) -> ControlMessage {
    ControlMessage::Error {
        code: code.into(),
        message: message.into(),
    }
}

/// Queues `msg` without waiting; it is dropped when the queue is full.
fn try_send_msg(tx: &mpsc::Sender<WsOut>, msg: &ControlMessage) {
    if let Ok(s) = msg.encode() {
        let _ = tx.try_send(WsOut::Text(s));
    }
}

/// Replies to the socket's own client. Only a room member's socket waits for
/// queue space (see [`read_loop`]).
async fn reply(tx: &mpsc::Sender<WsOut>, member: bool, msg: &ControlMessage) {
    if member {
        send_msg(tx, msg).await;
    } else {
        try_send_msg(tx, msg);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "ws_tests.rs"]
mod tests;
