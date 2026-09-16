use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, DeviceId, DeviceRole, ItemKind, MessageId, PeerInfo,
    QueuedEnvelope, FRAME_HARD_CAP,
};

use crate::auth::verify_token;
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

struct ConnCtx {
    device_id: String,
    room_id: String,
    role: ClientRole,
    pairing_sid: Option<String>,
    device_role: DeviceRole,
}

async fn handle_socket(socket: WebSocket, state: Arc<RelayState>, token_q: Option<String>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<WsOut>(256);

    let writer = tokio::spawn(async move {
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

    while let Some(Ok(msg)) = stream.next().await {
        if last_msg.elapsed() < Duration::from_millis(20) {
            burst += 1;
            if burst > 80 {
                send_err(&tx, "rate_limited", "too many messages").await;
                break;
            }
        } else {
            burst = 0;
            last_msg = Instant::now();
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
                if b.len() > FRAME_HARD_CAP {
                    send_err(&tx, "too_large", "frame exceeds hard cap").await;
                    continue;
                }
                if let Some(c) = &ctx {
                    if c.role == ClientRole::Member
                        || c.role == ClientRole::PairingA
                        || c.role == ClientRole::PairingB
                    {
                        forward_binary(&state, c, b.to_vec(), &tx).await;
                    }
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

    if let Some(c) = ctx {
        state.unregister_conn(&c.device_id);
        notify_peers_left(&state, &c).await;
    }
    let _ = tx.send(WsOut::Close).await;
    let _ = writer.await;
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
            let conn = ConnCtx {
                device_id: device_id.as_str().to_string(),
                room_id: claims.rid.as_str().to_string(),
                role,
                pairing_sid: pairing_session_id
                    .as_ref()
                    .map(|s| s.as_str().to_string())
                    .or(claims.sid.clone()),
                device_role: claims.role,
            };
            state.register_conn(&conn.device_id, tx.clone());

            if claims.typ == "pair" {
                if let Some(sid) = &conn.pairing_sid {
                    if let Some(session) = state.pairing(sid) {
                        if let Some(joiner) = &session.joiner {
                            if device_id == session.creator.device_id {
                                // notify creator that joiner is in
                                let _ = device_name;
                                let _ = os;
                            }
                            let _ = joiner;
                        }
                    }
                }
                // If joiner just connected, tell creator.
                if role == ClientRole::PairingB {
                    if let Some(sid) = &conn.pairing_sid {
                        if let Some(session) = state.pairing(sid) {
                            if let Some(creator_tx) =
                                state.connections.get(session.creator.device_id.as_str())
                            {
                                let joined = ControlMessage::PeerJoined {
                                    device: PeerInfo {
                                        device_id: device_id.clone(),
                                        device_name: device_name.clone(),
                                        os: os.clone(),
                                        role: DeviceRole::Member,
                                        online: true,
                                    },
                                };
                                if let Ok(s) = joined.encode() {
                                    let _ = creator_tx.send(WsOut::Text(s)).await;
                                }
                            }
                        }
                    }
                }
                let hello_ok = ControlMessage::HelloOk {
                    room_id: claims.rid.clone(),
                    peer_online: false,
                    peers: Vec::new(),
                    queued_text: None,
                    device_token: None,
                };
                send_msg(tx, &hello_ok).await;
            } else {
                if let Some(room) = state.room(&conn.room_id) {
                    if let Some(dev) = room.devices.get(&conn.device_id) {
                        if dev.revoked {
                            return Err("device revoked".into());
                        }
                    } else {
                        return Err("unknown device".into());
                    }
                } else {
                    return Err("unknown room".into());
                }
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
            forward_pairing_or_peer(state, c, msg).await
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
            let env = ControlMessage::TextEnvelope {
                routing: routing.clone(),
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
            };
            for peer in &peers {
                if let Some(p) = state.connections.get(peer) {
                    any_online = true;
                    if let Ok(s) = env.encode() {
                        let _ = p.send(WsOut::Text(s)).await;
                        state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    state.queue_text(
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
            if !state.revoke(&c.room_id, device_id.as_str()) {
                return Err("device not found".into());
            }
            fanout_except(state, &c.room_id, &c.device_id, &msg).await;
            if let Some(p) = state.connections.get(device_id.as_str()) {
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
    msg: ControlMessage,
) -> Result<(), String> {
    let is_bind = matches!(msg, ControlMessage::DeviceBind { .. });
    if let Some(sid) = &c.pairing_sid {
        if let Some(session) = state.pairing(sid) {
            let peer = if c.device_id == session.creator.device_id.as_str() {
                session
                    .joiner
                    .as_ref()
                    .map(|j| j.device_id.as_str().to_string())
            } else {
                Some(session.creator.device_id.as_str().to_string())
            };
            drop(session);
            if let Some(peer) = peer {
                if let Some(p) = state.connections.get(&peer) {
                    if let Ok(s) = msg.encode() {
                        let _ = p.send(WsOut::Text(s)).await;
                    }
                }
            }
        }
        if is_bind {
            if let Some((owner_tok, member_tok, room_id, creator, joiner)) =
                state.complete_pairing(sid)
            {
                let to_owner = ControlMessage::PairingComplete {
                    room_id: room_id.clone(),
                    epoch: 1,
                    device_token: owner_tok,
                    role: DeviceRole::Owner,
                };
                let to_member = ControlMessage::PairingComplete {
                    room_id,
                    epoch: 1,
                    device_token: member_tok,
                    role: DeviceRole::Member,
                };
                if let Some(p) = state.connections.get(creator.device_id.as_str()) {
                    if let Ok(s) = to_owner.encode() {
                        let _ = p.send(WsOut::Text(s)).await;
                    }
                }
                if let Some(p) = state.connections.get(joiner.device_id.as_str()) {
                    if let Ok(s) = to_member.encode() {
                        let _ = p.send(WsOut::Text(s)).await;
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
        if let Some(p) = state.connections.get(&peer) {
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
            if let Some(p) = state.connections.get(&peer) {
                let _ = p.send(WsOut::Text(s.clone())).await;
                state.frames_forwarded.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
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
