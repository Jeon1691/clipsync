// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
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

use crate::{current_os, device_name, http_client, relay_redirect_error, CoreError};

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
    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
    )]
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

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
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
    let security = clipsync_transport::check_relay_url(&cfg.relay_url)?;
    let url = format!("{}/v1/pairing/create", cfg.relay_url.trim_end_matches('/'));
    let resp = http_client(security)
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            CoreError::Message(format!(
                "relay create {url}: {}",
                crate::error::format_error(&e)
            ))
        })?;
    if let Some(error) = relay_redirect_error(&resp) {
        return Err(CoreError::Pairing(format!("relay create {url}: {error}")));
    }
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

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
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

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
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
    let security = clipsync_transport::check_relay_url(&cfg.relay_url)?;
    let url = format!("{}/v1/pairing/join", cfg.relay_url.trim_end_matches('/'));
    let resp = http_client(security)
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            CoreError::Message(format!(
                "relay join {url}: {}",
                crate::error::format_error(&e)
            ))
        })?;
    if let Some(error) = relay_redirect_error(&resp) {
        return Err(CoreError::Pairing(format!("relay join {url}: {error}")));
    }
    if !resp.status().is_success() {
        let throttled = resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let body = resp.text().await.unwrap_or_default();
        if body.contains("code_invalid") || body.contains("code_expired") {
            return Err(CoreError::Pairing(
                "unknown or expired pairing code; on the other device run `clipsync room create` again".into(),
            ));
        }
        if throttled || body.contains("too_many_attempts") {
            return Err(CoreError::Pairing(
                "the relay is limiting pairing attempts; wait a few minutes, then run `clipsync room create` again on the other device and retry with the new code".into(),
            ));
        }
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

#[expect(
    clippy::too_many_arguments,
    reason = "Keep explicit borrows of the existing pairing session state; reviewed in docs/compiler-warnings.md."
)]
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
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
                    let nonce = device_bind_nonce(&k.pairing_wrap_key, role)?;
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
                    let key_store = store.clone();
                    let key_room = room_id.clone();
                    // Keep the keys' ZeroizeOnDrop ownership in the blocking worker.
                    // Filesystem durability and platform keychains must not stall
                    // the runtime that services the pairing socket and watchdog.
                    let key_material = k.clone();
                    tokio::task::spawn_blocking(move || {
                        // Capture the whole zeroizing owner, not its Copy key field.
                        let keys = key_material;
                        let epoch_key = initial_epoch_key(&keys.room_root_key)?;
                        key_store.save_room_keys(
                            &key_room,
                            &hex::encode(keys.room_root_key),
                            &hex::encode(epoch_key),
                        )?;
                        Ok::<(), CoreError>(())
                    })
                    .await
                    .map_err(|error| {
                        CoreError::Pairing(format!("room key persistence task failed: {error}"))
                    })??;
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
                let room = RoomRecord {
                    room_id: room_id.clone(),
                    epoch,
                    role,
                    created_at: Utc::now(),
                    device_token,
                    peers: vec![peer],
                };
                let room_store = store.clone();
                tokio::task::spawn_blocking(move || {
                    room_store.upsert_room(room)?;
                    let mut cfg = room_store.load_config()?;
                    cfg.sync.enabled = true;
                    cfg.sync.auto_start = true;
                    cfg.sync.direction = clipsync_protocol::SyncDirection::Both;
                    room_store.save_config(&cfg)
                })
                .await
                .map_err(|error| {
                    CoreError::Pairing(format!("room persistence task failed: {error}"))
                })??;
                info!(room = %room_id, "pairing complete");
                conn.close().await;
                return Ok(());
            }
        }
    }
}

fn device_bind_nonce(
    key: &[u8; clipsync_crypto::KEY_LEN],
    role: PairingRole,
) -> clipsync_crypto::Result<[u8; 12]> {
    // Reserve 0 and 1; legacy v1 peers use index 1 for both pairing roles.
    let index = match role {
        PairingRole::A => 2,
        PairingRole::B => 3,
    };
    clipsync_crypto::derive_nonce(key, index)
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn connect_relay_ws(ws: &str) -> Result<RelayConnection, CoreError> {
    match RelayConnection::connect(ws).await {
        Ok(conn) => Ok(conn),
        // A policy refusal will not change on retry; keep its usage exit code.
        Err(clipsync_transport::TransportError::RelayUrl(e)) => Err(e.into()),
        Err(e) => {
            tracing::warn!(error = %e, %ws, "websocket connect failed; retrying");
            tokio::time::sleep(Duration::from_millis(250)).await;
            RelayConnection::connect(ws).await.map_err(|e| {
                CoreError::Message(format!(
                    "websocket {ws}: {}",
                    crate::error::format_error(&e)
                ))
            })
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
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

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn b64_decode(s: &str) -> Result<Vec<u8>, CoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| CoreError::Pairing(e.to_string()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use clipsync_crypto::{derive_nonce, PairingKeys};

    fn paired_keys() -> (PairingSessionId, PairingKeys, PairingKeys) {
        let session = PairingSessionId::new();
        let a = DeviceId::from("dev-a");
        let b = DeviceId::from("dev-b");
        let state_a = start_pairing_a("482913", session.clone(), a.clone(), b.clone()).unwrap();
        let state_b = start_pairing_b("482913", session.clone(), a, b).unwrap();
        let message_a = state_a.outbound.clone();
        let message_b = state_b.outbound.clone();
        let keys_a = finish_pairing(state_a, &message_b).unwrap();
        let keys_b = finish_pairing(state_b, &message_a).unwrap();
        assert_eq!(keys_a.pairing_wrap_key, keys_b.pairing_wrap_key);
        (session, keys_a, keys_b)
    }

    fn assert_bind_exchange(
        session: &PairingSessionId,
        keys_a: &PairingKeys,
        keys_b: &PairingKeys,
        nonce_a: [u8; 12],
        nonce_b: [u8; 12],
    ) {
        assert_ne!(
            nonce_a, nonce_b,
            "new and legacy peers must not reuse a nonce"
        );
        for (sender, receiver, nonce, role) in [
            (keys_a, keys_b, nonce_a, DeviceRole::Owner),
            (keys_b, keys_a, nonce_b, DeviceRole::Member),
        ] {
            let plain = BindPlain {
                device_id: DeviceId::from(if role == DeviceRole::Owner {
                    "dev-a"
                } else {
                    "dev-b"
                }),
                static_public: hex::encode([7; 32]),
                device_name: "compatibility fixture".into(),
                os: "test".into(),
                fingerprint: "fixture fingerprint".into(),
                epoch: 1,
                epoch_key: hex::encode(initial_epoch_key(&sender.room_root_key).unwrap()),
                role,
            };
            let ciphertext = seal(
                &sender.pairing_wrap_key,
                &nonce,
                session.as_str().as_bytes(),
                &serde_json::to_vec(&plain).unwrap(),
            )
            .unwrap();
            let wire = ControlMessage::DeviceBind {
                pairing_session_id: session.clone(),
                nonce: hex::encode(nonce),
                ciphertext: hex::encode(ciphertext),
            }
            .encode()
            .unwrap();
            let ControlMessage::DeviceBind {
                nonce, ciphertext, ..
            } = ControlMessage::decode(&wire).unwrap()
            else {
                panic!("expected a v1 DeviceBind frame");
            };
            // Both legacy and new receivers decrypt with the nonce sent on the wire.
            let received_nonce: [u8; 12] = hex::decode(nonce).unwrap().try_into().unwrap();
            let received = open(
                &receiver.pairing_wrap_key,
                &received_nonce,
                session.as_str().as_bytes(),
                &hex::decode(ciphertext).unwrap(),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<BindPlain>(&received).unwrap(),
                plain
            );
        }
    }

    #[test]
    fn device_bind_nonces_separate_roles_and_legacy_indices() {
        let (_, keys_a, keys_b) = paired_keys();
        let nonce_a = device_bind_nonce(&keys_a.pairing_wrap_key, PairingRole::A).unwrap();
        let nonce_b = device_bind_nonce(&keys_b.pairing_wrap_key, PairingRole::B).unwrap();
        assert_ne!(nonce_a, nonce_b, "pairing roles must use different nonces");
        for reserved_index in [0, 1] {
            let reserved = derive_nonce(&keys_a.pairing_wrap_key, reserved_index).unwrap();
            assert_ne!(nonce_a, reserved, "role A reused a reserved nonce");
            assert_ne!(nonce_b, reserved, "role B reused a reserved nonce");
        }
    }

    #[test]
    fn device_bind_new_a_and_legacy_b_interoperate() {
        let (session, keys_a, keys_b) = paired_keys();
        let nonce_a = device_bind_nonce(&keys_a.pairing_wrap_key, PairingRole::A).unwrap();
        let nonce_b = derive_nonce(&keys_b.pairing_wrap_key, 1).unwrap();
        assert_bind_exchange(&session, &keys_a, &keys_b, nonce_a, nonce_b);
    }

    #[test]
    fn device_bind_legacy_a_and_new_b_interoperate() {
        let (session, keys_a, keys_b) = paired_keys();
        let nonce_a = derive_nonce(&keys_a.pairing_wrap_key, 1).unwrap();
        let nonce_b = device_bind_nonce(&keys_b.pairing_wrap_key, PairingRole::B).unwrap();
        assert_bind_exchange(&session, &keys_a, &keys_b, nonce_a, nonce_b);
    }

    #[tokio::test]
    async fn pairing_refuses_plaintext_relays_before_sending_the_code() {
        // The pairing code is the SPAKE2 password; it must never be POSTed to
        // a plaintext non-loopback relay. A refused policy check returns
        // immediately, well inside this bound, while a real request to the
        // non-routable documentation address would hang until its timeout.
        const POLICY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
        // Clear an ambient opt-in so the result does not depend on the shell
        // that runs the suite.
        std::env::remove_var(clipsync_transport::ALLOW_INSECURE_RELAY_ENV);
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("plaintext-relay".into(), [41; 32]);
        for relay_url in ["http://192.0.2.1:7600", "relay.example.test"] {
            let cfg = AppConfig {
                relay_url: relay_url.into(),
                ..Default::default()
            };
            let create =
                tokio::time::timeout(POLICY_DEADLINE, begin_create_room(&identity, &cfg, 60))
                    .await
                    .expect("create contacted the relay")
                    .err()
                    .unwrap();
            assert!(matches!(create, CoreError::RelayUrl(_)), "{create}");
            let join = tokio::time::timeout(
                POLICY_DEADLINE,
                join_room(&store, &identity, &cfg, "123456"),
            )
            .await
            .expect("join contacted the relay")
            .unwrap_err();
            assert!(matches!(join, CoreError::RelayUrl(_)), "{join}");
            assert_eq!(join.exit_code(), crate::ExitCode::Usage as i32);
        }
        let ws = tokio::time::timeout(
            POLICY_DEADLINE,
            connect_relay_ws("ws://192.0.2.1:7600/v1/ws"),
        )
        .await
        .expect("websocket refusal was retried")
        .err()
        .unwrap();
        assert!(matches!(ws, CoreError::RelayUrl(_)), "{ws}");
    }

    #[tokio::test]
    async fn pairing_never_follows_relay_redirects() {
        use axum::{
            http::{header::LOCATION, StatusCode},
            routing::post,
            Router,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        // A followed redirect re-POSTs the pairing code to a host the relay
        // policy never checked. Refusing it returns at once, well inside this
        // bound, while a hop to the non-routable documentation address would
        // hang until the HTTP client's own timeout.
        const POLICY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
        let captured = Arc::new(AtomicUsize::new(0));
        let capture = {
            let captured = captured.clone();
            crate::test_relay::Server::http(Router::new().route(
                "/steal",
                post(move || async move {
                    captured.fetch_add(1, Ordering::SeqCst);
                    "{}"
                }),
            ))
            .await
        };
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("redirecting-relay".into(), [43; 32]);
        for target in [
            format!("{}/steal", capture.url),
            "http://192.0.2.1:7600/steal".to_string(),
        ] {
            for status in [
                StatusCode::TEMPORARY_REDIRECT,
                StatusCode::PERMANENT_REDIRECT,
            ] {
                let location = target.clone();
                let redirect = move || {
                    let location = location.clone();
                    async move { (status, [(LOCATION, location)]) }
                };
                let relay = crate::test_relay::Server::http(
                    Router::new()
                        .route("/v1/pairing/create", post(redirect.clone()))
                        .route("/v1/pairing/join", post(redirect)),
                )
                .await;
                let cfg = AppConfig {
                    relay_url: relay.url.clone(),
                    ..Default::default()
                };
                let create =
                    tokio::time::timeout(POLICY_DEADLINE, begin_create_room(&identity, &cfg, 60))
                        .await
                        .expect("create followed the redirect")
                        .err()
                        .unwrap();
                let join = tokio::time::timeout(
                    POLICY_DEADLINE,
                    join_room(&store, &identity, &cfg, "123456"),
                )
                .await
                .expect("join followed the redirect")
                .unwrap_err();
                for err in [create, join] {
                    let message = err.to_string();
                    assert!(message.contains("redirect"), "{target} {status}: {message}");
                    assert!(message.contains(&target), "{target} {status}: {message}");
                }
            }
        }
        assert_eq!(
            captured.load(Ordering::SeqCst),
            0,
            "the redirect target received the pairing request"
        );
        assert!(store.load_state().unwrap().rooms.is_empty());
    }

    #[tokio::test]
    async fn http_pairing_failures_preserve_relay_details_and_expired_code_guidance() {
        use axum::{http::StatusCode, routing::post, Router};
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("http-failure".into(), [31; 32]);
        for (status, body) in [
            (StatusCode::CONFLICT, "code_invalid"),
            (StatusCode::CONFLICT, "code_expired"),
            (StatusCode::CONFLICT, "busy: joiner already connected"),
            (
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"code":"too_many_attempts","message":"pairing attempt limit reached"}"#,
            ),
        ] {
            let server = crate::test_relay::Server::http(
                Router::new()
                    .route(
                        "/v1/pairing/create",
                        post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "maintenance") }),
                    )
                    .route(
                        "/v1/pairing/join",
                        post(move || async move { (status, body) }),
                    ),
            )
            .await;
            let cfg = AppConfig {
                relay_url: format!("{}/", server.url),
                ..Default::default()
            };
            let create = begin_create_room(&identity, &cfg, 60).await.err().unwrap();
            assert!(create.to_string().contains("create failed: maintenance"));
            let join = join_room(&store, &identity, &cfg, "123456")
                .await
                .unwrap_err();
            if body.starts_with("code_") {
                assert!(join.to_string().contains("unknown or expired pairing code"));
            } else if status == StatusCode::TOO_MANY_REQUESTS {
                assert!(join.to_string().contains("limiting pairing attempts"));
            } else {
                assert!(join.to_string().contains(body));
            }
            assert!(store.load_state().unwrap().rooms.is_empty());
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap();
        drop(listener);
        let cfg = AppConfig {
            relay_url: format!("http://{closed}"),
            ..Default::default()
        };
        assert!(begin_create_room(&identity, &cfg, 60)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("relay create"));
        assert!(join_room(&store, &identity, &cfg, "123456")
            .await
            .unwrap_err()
            .to_string()
            .contains("relay join"));
        assert!(connect_relay_ws(&format!("ws://{closed}/v1/ws"))
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("websocket"));
    }

    #[tokio::test]
    async fn malformed_http_pairing_responses_fail_before_persisting_a_room() {
        use axum::{routing::post, Router};
        let server = crate::test_relay::Server::http(
            Router::new()
                .route("/v1/pairing/create", post(|| async { "not JSON" }))
                .route("/v1/pairing/join", post(|| async { "{}" })),
        )
        .await;
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let cfg = AppConfig {
            relay_url: server.url.clone(),
            ..Default::default()
        };
        let identity = DeviceIdentity::from_bytes("malformed-response".into(), [32; 32]);
        assert!(matches!(
            create_room(&store, &identity, &cfg, 60).await,
            Err(CoreError::Http(_))
        ));
        assert!(matches!(
            join_room(&store, &identity, &cfg, "123456").await,
            Err(CoreError::Http(_))
        ));
        assert!(store.load_state().unwrap().rooms.is_empty());
    }

    #[tokio::test]
    async fn interrupted_or_rejected_pairing_never_creates_local_room_state() {
        use crate::test_relay::{send, Server};
        for rejected in [false, true] {
            let (consumed, release) = tokio::sync::oneshot::channel();
            let mut server = Server::websocket(move |mut socket| async move {
                if rejected {
                    send(
                        &mut socket,
                        ControlMessage::Error {
                            code: "expired".into(),
                            message: "pairing session expired".into(),
                        },
                    )
                    .await;
                    let _ = release.await;
                    return;
                }
                socket
                    .send(axum::extract::ws::Message::Close(None))
                    .await
                    .unwrap();
            })
            .await;
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("interrupted".into(), [33; 32]);
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let error = run_pairing(
                &store,
                &identity,
                connection,
                PairingRole::A,
                "123456",
                PairingSessionId::new(),
                identity.device_id.clone(),
                DeviceId::new(),
                clipsync_protocol::RoomId::new(),
                DeviceRole::Owner,
            )
            .await
            .unwrap_err();
            let _ = consumed.send(());
            server.finish().await;
            assert!(error.to_string().contains(if rejected {
                "expired: pairing session expired"
            } else {
                "relay closed during pairing"
            }));
            assert!(store.load_state().unwrap().rooms.is_empty());
        }
    }

    #[tokio::test]
    async fn silent_pairing_expires_without_persisting_credentials() {
        use crate::test_relay::Server;
        let (ready, connected) = tokio::sync::oneshot::channel();
        let (consumed, release) = tokio::sync::oneshot::channel();
        let mut server = Server::websocket_with_deadline(
            axum::Router::new(),
            Duration::from_secs(180),
            move |socket| async move {
                let _ = ready.send(());
                let _ = release.await;
                drop(socket);
            },
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("pairing-timeout".into(), [35; 32]);
        let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), connected)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        let result = tokio::time::timeout(
            Duration::from_secs(150),
            run_pairing(
                &store,
                &identity,
                connection,
                PairingRole::A,
                "123456",
                PairingSessionId::new(),
                identity.device_id.clone(),
                DeviceId::new(),
                clipsync_protocol::RoomId::new(),
                DeviceRole::Owner,
            ),
        )
        .await
        .unwrap();
        tokio::time::resume();
        let _ = consumed.send(());
        server.finish().await;
        assert!(matches!(result, Err(CoreError::Timeout)));
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert_eq!(std::fs::read_dir(&store.paths.data_dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn hostile_pairing_frames_cannot_publish_peer_identity_or_room_credentials() {
        use crate::test_relay::{receive, send, Server};
        for attack in [
            "base64",
            "spake",
            "confirm",
            "nonce_hex",
            "nonce_size",
            "ciphertext",
        ] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("hostile-peer-local".into(), [34; 32]);
            let peer = DeviceId::from("hostile-peer-remote");
            let sid = PairingSessionId::new();
            let session = sid.clone();
            let local = identity.device_id.clone();
            let peer_id = peer.clone();
            let (consumed, release) = tokio::sync::oneshot::channel();
            let mut server = Server::websocket(move |mut socket| async move {
                send(
                    &mut socket,
                    ControlMessage::PeerJoined {
                        device: clipsync_protocol::PeerInfo {
                            device_id: peer_id.clone(),
                            device_name: "untrusted".into(),
                            os: "fixture".into(),
                            role: DeviceRole::Member,
                            online: true,
                        },
                    },
                )
                .await;
                let ControlMessage::Spake2Msg { message, .. } = receive(&mut socket).await else {
                    panic!("client did not initiate SPAKE2")
                };
                if attack == "base64" || attack == "spake" {
                    send(
                        &mut socket,
                        ControlMessage::Spake2Msg {
                            pairing_session_id: session,
                            message: if attack == "base64" {
                                "!".into()
                            } else {
                                b64_encode(&[0])
                            },
                        },
                    )
                    .await;
                    // Rejection returns immediately; its advisory Error frame
                    // is queued best-effort and need not flush before close.
                    let _ = release.await;
                    return;
                }
                let state = start_pairing_b("123456", session.clone(), local, peer_id).unwrap();
                let outbound = state.outbound.clone();
                let keys = finish_pairing(state, &b64_decode(&message).unwrap()).unwrap();
                send(
                    &mut socket,
                    ControlMessage::Spake2Msg {
                        pairing_session_id: session.clone(),
                        message: b64_encode(&outbound),
                    },
                )
                .await;
                let ControlMessage::KeyConfirm { mac, .. } = receive(&mut socket).await else {
                    panic!("client did not confirm key")
                };
                verify_confirm_mac(&keys, PairingRole::A, &b64_decode(&mac).unwrap()).unwrap();
                if attack == "confirm" {
                    send(
                        &mut socket,
                        ControlMessage::KeyConfirm {
                            pairing_session_id: session,
                            mac: b64_encode(&[0; 32]),
                        },
                    )
                    .await;
                    let _ = release.await;
                    return;
                }
                send(
                    &mut socket,
                    ControlMessage::DeviceBind {
                        pairing_session_id: session,
                        nonce: match attack {
                            "nonce_hex" => "!".into(),
                            "nonce_size" => "00".into(),
                            _ => hex::encode([0; 12]),
                        },
                        ciphertext: "!".into(),
                    },
                )
                .await;
                let _ = release.await;
            })
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::A,
                    "123456",
                    sid,
                    identity.device_id.clone(),
                    peer,
                    clipsync_protocol::RoomId::new(),
                    DeviceRole::Owner,
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            let _ = consumed.send(());
            server.finish().await;
            assert!(matches!(error, CoreError::Pairing(_)), "{attack}: {error}");
            let diagnostic = match attack {
                "spake" => "spake2 finish failed",
                "confirm" => "confirmation mismatch",
                "nonce_size" => "bad bind nonce",
                "base64" => "Invalid",
                _ => "Odd number of digits",
            };
            assert!(error.to_string().contains(diagnostic), "{attack}: {error}");
            assert!(store.load_state().unwrap().rooms.is_empty(), "{attack}");
        }
    }
}
