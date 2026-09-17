use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use tracing::{info, warn};

use clipsync_crypto::{
    confirm_mac, finish_pairing, initial_epoch_key, open, seal, start_pairing_a, start_pairing_b,
    verify_confirm_mac, DeviceIdentity, PairingRole,
};
use clipsync_protocol::{
    ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse,
    DeviceBindPlain as BindPlain, DeviceId, DeviceRole, JoinPairingRequest, JoinPairingResponse,
    PairingSessionId, PROTOCOL_VERSION,
};
use clipsync_storage::{AppConfig, LocalStore, PeerRecord, RoomRecord};
use clipsync_transport::{join_ws, Incoming, RelayConnection};

use crate::{current_os, device_name, http_client, CoreError};

pub struct RoomOffer {
    pub pairing_code: String,
    pub room_id: clipsync_protocol::RoomId,
    pub expires_at: chrono::DateTime<Utc>,
}

pub struct PendingCreate {
    pub offer: RoomOffer,
    conn: RelayConnection,
    session_id: PairingSessionId,
    password: String,
}

impl PendingCreate {
    pub async fn wait(
        self,
        store: &LocalStore,
        identity: &DeviceIdentity,
    ) -> Result<(), CoreError> {
        run_pairing(
            store,
            identity,
            self.conn,
            PairingRole::A,
            &self.password,
            self.session_id,
            identity.device_id.clone(),
            DeviceId::from(""),
            self.offer.room_id.clone(),
            DeviceRole::Owner,
        )
        .await
    }
}

pub async fn begin_create_room(
    identity: &DeviceIdentity,
    cfg: &AppConfig,
    ttl_secs: u64,
) -> Result<PendingCreate, CoreError> {
    let session_id = PairingSessionId::new();
    let req = CreatePairingRequest {
        pairing_session_id: session_id.clone(),
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        protocol_version: PROTOCOL_VERSION,
        ttl_secs: Some(ttl_secs),
    };
    let url = format!("{}/v1/pairing/create", cfg.relay_url.trim_end_matches('/'));
    let resp = http_client()
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| CoreError::Message(format!("relay create {url}: {e}")))?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreError::Pairing(format!("create failed: {body}")));
    }
    let created: CreatePairingResponse = resp.json().await?;
    let offer = RoomOffer {
        pairing_code: created.pairing_code.clone(),
        room_id: created.room_id.clone(),
        expires_at: created.expires_at,
    };
    let ws = join_ws(&cfg.relay_url, &created.ws_path);
    let conn = connect_relay_ws(&ws).await?;
    conn.send_control(ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        role: ClientRole::PairingA,
        room_id: Some(created.room_id.clone()),
        token: created.pairing_token.clone(),
        pairing_session_id: Some(session_id.clone()),
    })
    .await?;
    Ok(PendingCreate {
        offer,
        conn,
        session_id,
        password: created.pairing_code,
    })
}

pub async fn create_room(
    store: &LocalStore,
    identity: &DeviceIdentity,
    cfg: &AppConfig,
    ttl_secs: u64,
) -> Result<RoomOffer, CoreError> {
    let pending = begin_create_room(identity, cfg, ttl_secs).await?;
    let offer = RoomOffer {
        pairing_code: pending.offer.pairing_code.clone(),
        room_id: pending.offer.room_id.clone(),
        expires_at: pending.offer.expires_at,
    };
    pending.wait(store, identity).await?;
    Ok(offer)
}

pub async fn join_room(
    store: &LocalStore,
    identity: &DeviceIdentity,
    cfg: &AppConfig,
    code: &str,
) -> Result<RoomRecord, CoreError> {
    let req = JoinPairingRequest {
        pairing_code: code.to_string(),
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        protocol_version: PROTOCOL_VERSION,
    };
    let url = format!("{}/v1/pairing/join", cfg.relay_url.trim_end_matches('/'));
    let resp = http_client()
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| CoreError::Message(format!("relay join {url}: {e}")))?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoreError::Pairing(format!("join failed: {body}")));
    }
    let joined: JoinPairingResponse = resp.json().await?;
    let ws = join_ws(&cfg.relay_url, &joined.ws_path);
    let conn = connect_relay_ws(&ws).await?;
    conn.send_control(ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        role: ClientRole::PairingB,
        room_id: Some(joined.room_id.clone()),
        token: joined.pairing_token.clone(),
        pairing_session_id: Some(joined.pairing_session_id.clone()),
    })
    .await?;
    run_pairing(
        store,
        identity,
        conn,
        PairingRole::B,
        code,
        joined.pairing_session_id,
        joined.peer.device_id.clone(),
        identity.device_id.clone(),
        joined.room_id,
        DeviceRole::Member,
    )
    .await?;
    store.current_room().map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
async fn run_pairing(
    store: &LocalStore,
    identity: &DeviceIdentity,
    mut conn: RelayConnection,
    role: PairingRole,
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
    room_id: clipsync_protocol::RoomId,
    our_role: DeviceRole,
) -> Result<(), CoreError> {
    let timeout = tokio::time::sleep(Duration::from_secs(clipsync_protocol::PAIRING_TTL_SECS + 5));
    tokio::pin!(timeout);

    let mut peer_id: Option<DeviceId> = match role {
        PairingRole::A => None,
        PairingRole::B => Some(device_a.clone()),
    };
    let mut spake_started = false;
    let mut state = None;
    let mut keys = None;
    let mut inbound_spake: Option<Vec<u8>> = None;
    let mut peer_confirm: Option<Vec<u8>> = None;
    let mut peer_bind: Option<(String, String)> = None;
    let mut complete: Option<ControlMessage> = None;
    let mut our_bind_sent = false;
    let mut confirm_sent = false;

    loop {
        tokio::select! {
            _ = &mut timeout => return Err(CoreError::Timeout),
            incoming = conn.incoming.recv() => {
                let Some(incoming) = incoming else {
                    return Err(CoreError::Pairing("relay closed during pairing".into()));
                };
                match incoming {
                    Incoming::Control(ControlMessage::HelloOk { .. }) => {}
                    Incoming::Control(ControlMessage::PeerJoined { device }) => {
                        peer_id = Some(device.device_id.clone());
                    }
                    Incoming::Control(ControlMessage::Spake2Msg { message, .. }) => {
                        inbound_spake = Some(b64_decode(&message)?);
                    }
                    Incoming::Control(ControlMessage::KeyConfirm { mac, .. }) => {
                        peer_confirm = Some(b64_decode(&mac)?);
                    }
                    Incoming::Control(ControlMessage::DeviceBind { nonce, ciphertext, .. }) => {
                        peer_bind = Some((nonce, ciphertext));
                    }
                    Incoming::Control(msg @ ControlMessage::PairingComplete { .. }) => {
                        complete = Some(msg);
                    }
                    Incoming::Control(ControlMessage::Error { code, message }) => {
                        return Err(CoreError::Pairing(format!("{code}: {message}")));
                    }
                    Incoming::Pong | Incoming::Binary(_) => {}
                    Incoming::Control(_) => {}
                }
            }
        }

        if role == PairingRole::A && peer_id.is_none() {
            continue;
        }

        if !spake_started {
            let (id_a, id_b) = match role {
                PairingRole::A => (
                    identity.device_id.clone(),
                    peer_id
                        .clone()
                        .ok_or_else(|| CoreError::Pairing("missing joiner identity".into()))?,
                ),
                PairingRole::B => (device_a.clone(), device_b.clone()),
            };
            let st = match role {
                PairingRole::A => start_pairing_a(password, session_id.clone(), id_a, id_b)?,
                PairingRole::B => start_pairing_b(password, session_id.clone(), id_a, id_b)?,
            };
            conn.send_control(ControlMessage::Spake2Msg {
                pairing_session_id: session_id.clone(),
                message: b64_encode(&st.outbound),
            })
            .await?;
            state = Some(st);
            spake_started = true;
        }

        if keys.is_none() && inbound_spake.is_some() {
            if let (Some(st), Some(inbound)) = (state.take(), inbound_spake.clone()) {
                match finish_pairing(st, &inbound) {
                    Ok(k) => keys = Some(k),
                    Err(e) => {
                        let _ = conn
                            .send_control(ControlMessage::Error {
                                code: "pairing_failed".into(),
                                message: e.to_string(),
                            })
                            .await;
                        return Err(CoreError::Pairing(e.to_string()));
                    }
                }
            }
        }

        if let Some(k) = &keys {
            if !confirm_sent {
                conn.send_control(ControlMessage::KeyConfirm {
                    pairing_session_id: session_id.clone(),
                    mac: b64_encode(&confirm_mac(k, role)),
                })
                .await?;
                confirm_sent = true;
            }
            if let Some(mac) = &peer_confirm {
                let peer_role = match role {
                    PairingRole::A => PairingRole::B,
                    PairingRole::B => PairingRole::A,
                };
                if let Err(e) = verify_confirm_mac(k, peer_role, mac) {
                    let _ = conn
                        .send_control(ControlMessage::Error {
                            code: "confirm_failed".into(),
                            message: "key confirmation failed".into(),
                        })
                        .await;
                    return Err(CoreError::Pairing(e.to_string()));
                }
                if !our_bind_sent {
                    let epoch_key = initial_epoch_key(&k.room_root_key)?;
                    let plain = BindPlain {
                        device_id: identity.device_id.clone(),
                        static_public: hex::encode(identity.public_bytes()),
                        device_name: device_name(),
                        os: current_os().into(),
                        fingerprint: identity.fingerprint(),
                        epoch: 1,
                        epoch_key: hex::encode(epoch_key),
                        role: our_role,
                    };
                    let pt = serde_json::to_vec(&plain)?;
                    let nonce = clipsync_crypto::derive_nonce(&k.pairing_wrap_key, 1)?;
                    let ct = seal(
                        &k.pairing_wrap_key,
                        &nonce,
                        session_id.as_str().as_bytes(),
                        &pt,
                    )?;
                    conn.send_control(ControlMessage::DeviceBind {
                        pairing_session_id: session_id.clone(),
                        nonce: hex::encode(nonce),
                        ciphertext: hex::encode(ct),
                    })
                    .await?;
                    our_bind_sent = true;
                    store.save_room_keys(
                        &room_id,
                        &hex::encode(k.room_root_key),
                        &hex::encode(epoch_key),
                    )?;
                }
            }
        }

        if let (Some(k), Some((nonce_hex, ct_hex))) = (&keys, peer_bind.clone()) {
            let nonce_bytes =
                hex::decode(&nonce_hex).map_err(|e| CoreError::Pairing(e.to_string()))?;
            if nonce_bytes.len() != 12 {
                return Err(CoreError::Pairing("bad bind nonce".into()));
            }
            let mut nonce = [0u8; 12];
            nonce.copy_from_slice(&nonce_bytes);
            let ct = hex::decode(ct_hex).map_err(|e| CoreError::Pairing(e.to_string()))?;
            let pt = open(
                &k.pairing_wrap_key,
                &nonce,
                session_id.as_str().as_bytes(),
                &ct,
            )?;
            let bind: BindPlain = serde_json::from_slice(&pt)?;
            if let Some(ControlMessage::PairingComplete {
                room_id,
                epoch,
                device_token,
                role,
            }) = complete.clone()
            {
                let peer = PeerRecord {
                    device_id: bind.device_id.clone(),
                    device_name: bind.device_name.clone(),
                    os: bind.os.clone(),
                    role: bind.role,
                    public_key_hex: bind.static_public.clone(),
                    fingerprint: bind.fingerprint.clone(),
                };
                store.upsert_room(RoomRecord {
                    room_id: room_id.clone(),
                    epoch,
                    role,
                    created_at: Utc::now(),
                    device_token,
                    peers: vec![peer],
                })?;
                let mut cfg = store.load_config()?;
                cfg.sync.enabled = true;
                cfg.sync.auto_start = true;
                cfg.sync.direction = clipsync_protocol::SyncDirection::Both;
                store.save_config(&cfg)?;
                info!(room = %room_id, "pairing complete");
                conn.close().await;
                return Ok(());
            }
        }
    }
}

async fn connect_relay_ws(ws: &str) -> Result<RelayConnection, CoreError> {
    match RelayConnection::connect(ws).await {
        Ok(conn) => Ok(conn),
        Err(e) => {
            tracing::warn!(error = %e, %ws, "websocket connect failed; retrying");
            tokio::time::sleep(Duration::from_millis(250)).await;
            RelayConnection::connect(ws)
                .await
                .map_err(|e| CoreError::Message(format!("websocket {ws}: {e}")))
        }
    }
}

pub fn leave_room(store: &LocalStore) -> Result<(), CoreError> {
    match store.leave_current()? {
        Some(room) => {
            info!(room = %room.room_id, "left room");
            Ok(())
        }
        None => {
            warn!("no current room");
            Ok(())
        }
    }
}

fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, CoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| CoreError::Pairing(e.to_string()))
}
