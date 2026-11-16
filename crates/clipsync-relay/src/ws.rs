// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::future::join_all;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::error::SendTimeoutError;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, warn};

use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, DeviceId, DeviceRole, ItemKind, MessageId, PeerInfo,
    QueuedEnvelope, FRAME_HARD_CAP, HEARTBEAT_SECS,
};

use crate::auth::verify_token;
use crate::state::{LiveConnection, MemberRegistration};
use crate::RelayState;

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

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    State(state): State<Arc<RelayState>>,
) -> impl IntoResponse {
    ws.max_message_size(FRAME_HARD_CAP)
        .max_frame_size(FRAME_HARD_CAP)
        .on_upgrade(move |socket| handle_socket(socket, state, q.token))
}

/// Client heartbeats a socket may miss before the relay treats it as dead.
/// Clients send a control `Ping` and a WebSocket ping every `HEARTBEAT_SECS`
/// and reconnect on their own after about two silent heartbeats, so three
/// tolerates a late beat and never closes a socket its client still trusts.
const READ_IDLE_HEARTBEATS: u64 = 3;

/// How long a socket may send nothing before the relay closes it and drops its
/// route. A half-open socket (laptop sleep, network change) never delivers a
/// FIN; without this it would keep its member online and swallow the text
/// that should have been queued for it.
pub(crate) const READ_IDLE_TIMEOUT: Duration =
    Duration::from_secs(HEARTBEAT_SECS * READ_IDLE_HEARTBEATS);

/// How long a closing socket's writer may take to flush its queue and close
/// frame. A peer that still reads needs far less; a writer still blocked after
/// this writes to a peer that stopped reading, so it is aborted, which frees
/// the queue and closes the TCP connection.
const WRITER_CLOSE_GRACE: Duration = Duration::from_secs(5);

struct ConnCtx {
    device_id: String,
    room_id: String,
    pairing_sid: Option<String>,
    device_role: DeviceRole,
    pairing_side: Option<usize>,
    // Set only for device-token sockets registered for a room-member route.
    member_conn: Option<MemberRegistration>,
    _live: LiveConnection,
}

// A protocol-v1 Ed25519 SPAKE message is 33 bytes (44 in base64). Retain only
// its first public message while the authenticated peer opens its WebSocket.
const PAIRING_SPAKE_MAX_BYTES: usize = 1024;

struct PairingArrival {
    joined: Option<(mpsc::Sender<WsOut>, PeerInfo)>,
    pending: Option<ControlMessage>,
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
    // Pairing tokens are issued without authentication for any device ID, so
    // a pairing socket is routed only within its session and never becomes a
    // room-member route.
    session.pairing_senders[side] = Some(tx.clone());
    state.connections_total.fetch_add(1, Ordering::Relaxed);
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
    Ok(PairingArrival { joined, pending })
}

async fn handle_socket(socket: WebSocket, state: Arc<RelayState>, token_q: Option<String>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<WsOut>(256);

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

    let mut ctx: Option<ConnCtx> = None;
    let mut last_msg = Instant::now();
    let mut burst = 0u32;

    loop {
        let evicted = ctx
            .as_ref()
            .and_then(|c| c.member_conn.as_ref())
            .map(|m| m.evicted.clone());
        let msg = tokio::select! {
            msg = tokio::time::timeout(state.read_idle_timeout, stream.next()) => match msg {
                Ok(Some(Ok(msg))) => msg,
                Ok(_) => break,
                Err(_) => {
                    debug!("closing a socket that missed its heartbeats");
                    break;
                }
            },
            // An evicted or revoked member socket lost its route. Stop reading
            // so it closes even if the client never answers the close frame.
            () = until_evicted(evicted.as_deref()) => break,
        };
        if message_rate_exceeded(&mut last_msg, &mut burst, Instant::now()) {
            send_err(&tx, "rate_limited", "too many messages").await;
            break;
        }
        match msg {
            Message::Text(t) => {
                let parsed = match ControlMessage::decode(&t) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "bad control");
                        continue;
                    }
                };
                if let Err(e) =
                    handle_control(&state, &tx, &mut ctx, parsed, token_q.as_deref()).await
                {
                    send_err(&tx, "error", &e).await;
                }
            }
            Message::Binary(b) => {
                // Like frames before hello, binary from a pairing socket or a
                // member socket that is no longer routed is dropped.
                if let Ok(c) = member_ctx(&state, &ctx) {
                    forward_binary(&state, c, b.to_vec(), &tx).await;
                }
            }
            Message::Ping(p) => {
                let _ = tx.send(WsOut::Binary(Vec::new())).await;
                let _ = p;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    if let Some(c) = &ctx {
        if let Some(member) = &c.member_conn {
            state.unregister_conn(&c.room_id, &c.device_id, member.id);
            // The member may still have other sockets (its daemon, a newer
            // reconnect). Its peers must keep seeing it online until the last
            // one closes.
            if !state.is_online(&c.room_id, &c.device_id) {
                notify_peers_left(&state, c).await;
            }
        }
        if let (Some(sid), Some(side)) = (&c.pairing_sid, c.pairing_side) {
            if let Some(mut session) = state.pairing_mut(sid) {
                if session.pairing_senders[side]
                    .as_ref()
                    .is_some_and(|sender| sender.same_channel(&tx))
                {
                    session.pairing_senders[side] = None;
                    session.pending_spake = [None, None];
                }
            }
        }
    }
    // An evicted socket may have a full queue behind a writer blocked on a
    // peer that stopped reading, so the close is queued without waiting.
    let close_queued = tx.try_send(WsOut::Close).is_ok();
    if !close_queued
        || tokio::time::timeout(WRITER_CLOSE_GRACE, &mut writer)
            .await
            .is_err()
    {
        writer.abort();
        let _ = writer.await;
    }
    // The live-connection count drops only now: after any leave notice was
    // queued and after the writer, its queue and its sink are gone.
    drop(ctx);
}

const MEMBER_CONNECTION_REQUIRED: &str = "room member connection required";

async fn until_evicted(evicted: Option<&Notify>) {
    match evicted {
        Some(evicted) => evicted.notified().await,
        None => std::future::pending().await,
    }
}

/// Room traffic needs a device-token socket that is still routed for its
/// member. Pairing tokens are issued without authentication and are confined
/// to their pairing session; an evicted or revoked socket lost its route, so
/// it may not send either.
///
/// The check and the forward that follows are not one atomic step: a frame
/// that passed the check just before a concurrent revocation is still
/// forwarded. Each socket handles one frame at a time, so this is at most one
/// frame per socket of the revoked member.
fn member_ctx<'a>(state: &RelayState, ctx: &'a Option<ConnCtx>) -> Result<&'a ConnCtx, String> {
    let c = ctx.as_ref().ok_or("hello required")?;
    if !owns_member_route(state, c) {
        return Err(MEMBER_CONNECTION_REQUIRED.into());
    }
    Ok(c)
}

fn owns_member_route(state: &RelayState, c: &ConnCtx) -> bool {
    c.member_conn
        .as_ref()
        .is_some_and(|member| state.owns_route(&c.room_id, &c.device_id, member.id))
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

async fn handle_control(
    state: &Arc<RelayState>,
    tx: &mpsc::Sender<WsOut>,
    ctx: &mut Option<ConnCtx>,
    msg: ControlMessage,
    token_q: Option<&str>,
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
                member_conn: None,
                _live: state.live_connection(),
            };
            if claims.typ == "pair" {
                let arrival = register_pairing_connection(state, &mut conn, tx, role)?;
                let hello_ok = ControlMessage::HelloOk {
                    room_id: claims.rid.clone(),
                    peer_online: false,
                    peers: Vec::new(),
                    queued_text: None,
                    device_token: None,
                };
                send_msg(tx, &hello_ok).await;
                if let Some((creator, device)) = arrival.joined {
                    send_msg(&creator, &ControlMessage::PeerJoined { device }).await;
                }
                if let Some(pending) = arrival.pending {
                    send_msg(tx, &pending).await;
                }
            } else {
                conn.member_conn =
                    Some(state.register_conn(&conn.room_id, &conn.device_id, tx.clone())?);
                let peers: Vec<PeerInfo> = state
                    .peer_ids(&conn.room_id, &conn.device_id)
                    .into_iter()
                    .filter_map(|id| {
                        let mut info = state.room(&conn.room_id)?.devices.get(&id)?.info.clone();
                        info.online = state.is_online(&conn.room_id, &id);
                        Some(info)
                    })
                    .collect();
                let peer_online = peers.iter().any(|p| p.online);
                let queued = state.take_queue(&conn.room_id, &conn.device_id);
                let hello_ok = ControlMessage::HelloOk {
                    room_id: claims.rid.clone(),
                    peer_online,
                    peers: peers.clone(),
                    queued_text: queued,
                    device_token: None,
                };
                send_msg(tx, &hello_ok).await;
                let joined = ControlMessage::PeerJoined {
                    device: PeerInfo {
                        device_id: device_id.clone(),
                        device_name,
                        os,
                        role: claims.role,
                        online: true,
                    },
                };
                fanout_except(state, &conn.room_id, &conn.device_id, &joined).await;
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
            send_msg(tx, &ControlMessage::Pong { ts }).await;
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
            let c = member_ctx(state, ctx)?;
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
            let env = ControlMessage::TextEnvelope {
                routing: routing.clone(),
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
            };
            let encoded = env.encode().ok();
            for peer in &peers {
                let online = match &encoded {
                    Some(s) => deliver(state, &c.room_id, peer, WsOut::Text(s.clone())).await,
                    None => state.is_online(&c.room_id, peer),
                };
                if online {
                    any_online = true;
                } else {
                    state.queue_text(
                        &c.room_id,
                        peer,
                        QueuedEnvelope {
                            routing: routing.clone(),
                            nonce: nonce.clone(),
                            ciphertext: ciphertext.clone(),
                        },
                        routing.expires_at,
                    );
                }
            }
            send_msg(
                tx,
                &ControlMessage::Ack {
                    message_id: routing.message_id,
                    transfer_id: Some(routing.transfer_id),
                    status: if any_online {
                        AckStatus::Delivered
                    } else {
                        AckStatus::Accepted
                    },
                    detail: None,
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
            let c = member_ctx(state, ctx)?;
            let peers = state.peer_ids(&c.room_id, &c.device_id);
            let online: Vec<_> = peers
                .iter()
                .filter(|p| state.is_online(&c.room_id, p))
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
            let c = member_ctx(state, ctx)?;
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
            Ok(())
        }
        ControlMessage::Revoke { ref device_id } => {
            let c = member_ctx(state, ctx)?;
            if c.device_role != DeviceRole::Owner {
                return Err("manage permission required".into());
            }
            // Revocation removes every route of the member and evicts all of
            // its sockets, which then close themselves.
            if !state.revoke(&c.room_id, device_id.as_str()) {
                return Err("device not found".into());
            }
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
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
    if !owns_member_route(state, c) {
        return Err(MEMBER_CONNECTION_REQUIRED.into());
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
        sent |= deliver(state, &c.room_id, &peer, WsOut::Binary(bytes.clone())).await;
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
            deliver(state, room_id, &peer, WsOut::Text(s.clone())).await;
        }
    }
}

/// How long an older socket of a member may hold up delivery while its
/// outbound queue is full before the relay evicts it. A healthy socket frees
/// a slot once its writer hands one frame (at most `FRAME_HARD_CAP`, 384 KiB)
/// to the kernel; 30 s covers that on a link of about 105 kbit/s or faster
/// (384 KiB × 8 / 30 s). Only a stalled socket reaches it, typically a
/// half-open one whose client already reconnected, which would otherwise block
/// delivery to the member's live sockets until the read-idle timeout closed it.
const STALLED_MEMBER_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// Sends `out` to every routed socket of `peer` in `room_id` and returns
/// whether the member had any. The newest socket keeps plain backpressure, as
/// a member's only socket always had; an older socket that stays full for
/// `STALLED_MEMBER_SOCKET_TIMEOUT` is evicted instead of stalling the sender.
/// All of the member's sockets are sent to at once, so stalled older sockets
/// delay one delivery by at most one `STALLED_MEMBER_SOCKET_TIMEOUT` in total.
async fn deliver(state: &RelayState, room_id: &str, peer: &str, out: WsOut) -> bool {
    deliver_within(state, room_id, peer, out, STALLED_MEMBER_SOCKET_TIMEOUT).await
}

async fn deliver_within(
    state: &RelayState,
    room_id: &str,
    peer: &str,
    out: WsOut,
    stall_timeout: Duration,
) -> bool {
    let sockets = state.member_senders(room_id, peer);
    let Some(((_, newest), older)) = sockets.split_last() else {
        return false;
    };
    let older_sends = join_all(older.iter().map(|(id, sender)| {
        let out = out.clone();
        async move {
            match sender.send_timeout(out, stall_timeout).await {
                Ok(()) => {
                    state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
                }
                Err(SendTimeoutError::Timeout(_)) => {
                    warn!("evicting a member socket whose outbound queue stalled");
                    state.evict_conn(room_id, peer, *id);
                }
                // The socket is already closing; its cleanup unregisters it.
                Err(SendTimeoutError::Closed(_)) => {}
            }
        }
    }));
    let (_, newest_result) = tokio::join!(older_sends, newest.send(out));
    if newest_result.is_ok() {
        state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
    }
    true
}

async fn notify_peers_left(state: &Arc<RelayState>, c: &ConnCtx) {
    let msg = ControlMessage::PeerLeft {
        device_id: DeviceId::from(c.device_id.as_str()),
    };
    fanout_except(state, &c.room_id, &c.device_id, &msg).await;
}

async fn send_msg(tx: &mpsc::Sender<WsOut>, msg: &ControlMessage) {
    if let Ok(s) = msg.encode() {
        let _ = tx.send(WsOut::Text(s)).await;
    }
}

async fn send_err(tx: &mpsc::Sender<WsOut>, code: &str, message: &str) {
    send_msg(
        tx,
        &ControlMessage::Error {
            code: code.into(),
            message: message.into(),
        },
    )
    .await;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "ws_tests.rs"]
mod tests;
