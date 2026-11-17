// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::future::Future;
use std::time::Duration;

use base64::Engine as _;
use chrono::Utc;
use tracing::{info, warn};

use clipsync_crypto::{
    confirm_mac, finish_pairing, initial_epoch_key, open, seal, short_authentication_string,
    start_pairing_a, start_pairing_b, verify_confirm_mac, DeviceIdentity, PairingRole, SAS_DIGITS,
};
use clipsync_protocol::{
    ClientRole, ControlMessage, CreatePairingRequest, CreatePairingResponse,
    DeviceBindPlain as BindPlain, DeviceId, DeviceRole, JoinPairingRequest, JoinPairingResponse,
    PairingSessionId, PROTOCOL_VERSION,
};
use clipsync_storage::{AppConfig, LocalStore, PeerRecord, RoomRecord};
use clipsync_transport::{join_ws, Incoming, RelayConnection};

use crate::untrusted_text::{
    is_plain_identifier, sanitize_untrusted, MAX_PEER_NAME_COLUMNS, MAX_RELAY_MESSAGE_COLUMNS,
};
use crate::{current_os, device_name, http_client, CoreError};

/// Advisory error code sent to the relay when the local user rejects the SAS.
/// The relay aborts the pairing session on any client `Error` frame.
const SAS_REJECTED_CODE: &str = "sas_rejected";
/// How long a rejecting client keeps reading after queueing its `Error` and
/// `Close` frames. The transport stops forwarding, and drops queued frames,
/// once nobody reads incoming frames, so a late peer frame could otherwise
/// cancel the abort notice. Two queued frames flush in milliseconds on a live
/// socket; the bound keeps a stuck relay from holding the caller.
const SAS_REJECT_FLUSH: Duration = Duration::from_secs(2);
/// `mac` of the `key_confirm` a device sends right before its `sas_rejected`
/// `Error`. The relay forwards `key_confirm` to the peer but not `Error`, so
/// this existing frame is the only way to tell a peer that already confirmed
/// to stop waiting. An empty MAC never verifies: a pre-SAS peer fails key
/// confirmation at once, and an updated peer treats any second `key_confirm`
/// as this notice.
const SAS_ABORT_NOTICE_MAC: &str = "";

/// Relay-supplied text made safe to print: it ends up in error messages on
/// a terminal, possibly right after a verification code.
pub(crate) fn relay_text(raw: &str) -> String {
    sanitize_untrusted(raw, MAX_RELAY_MESSAGE_COLUMNS)
}

/// Digits in a pairing code: the relay draws codes uniformly from 000000 to
/// 999999 (`alloc_code` in the relay's state module).
const PAIRING_CODE_DIGITS: usize = 6;

/// Error for a relay-supplied value that is printed or stored but does not
/// have the expected form, such as a room ID carrying terminal escapes.
fn malformed(what: &str) -> CoreError {
    CoreError::Pairing(format!(
        "the relay sent a malformed {what}; nothing was stored"
    ))
}

/// Check the values of a create or join response that the CLI prints and
/// the store keeps before anything is shown or stored.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn check_relay_ids(
    room_id: &clipsync_protocol::RoomId,
    peer: Option<&DeviceId>,
) -> Result<(), CoreError> {
    if !is_plain_identifier(room_id.as_str()) {
        return Err(malformed("room ID"));
    }
    if peer.is_some_and(|peer| !is_plain_identifier(peer.as_str())) {
        return Err(malformed("device ID"));
    }
    Ok(())
}

/// A pairing code is exactly [`PAIRING_CODE_DIGITS`] ASCII digits.
fn is_pairing_code(code: &str) -> bool {
    code.len() == PAIRING_CODE_DIGITS && code.bytes().all(|byte| byte.is_ascii_digit())
}

/// What the local user must compare with the other device before a pairing
/// is committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SasCheck {
    /// Short authentication string. Both devices show the same value only if
    /// they ran SPAKE2 with each other.
    pub sas: String,
    pub peer_device_id: DeviceId,
    /// Name the relay reported for the peer before SPAKE2 started (a later,
    /// different announcement aborts the pairing). It is not authenticated
    /// and is shown for context only; the SAS is what proves the peer's
    /// identity. Control, bidi, blank and invisible characters arrive escaped
    /// and the width is bounded (see [`crate::sanitize_untrusted`]), but show
    /// it apart from the SAS anyway.
    pub peer_device_name: String,
}

impl SasCheck {
    /// Compare with the value a user read from the other device, ignoring the
    /// spaces or dashes used to group digits on screen.
    pub fn matches(&self, shown_on_other_device: &str) -> bool {
        parse_sas(shown_on_other_device).is_some_and(|entered| entered == self.sas)
    }

    /// Digits grouped for reading aloud, such as `482 913`.
    pub fn display(&self) -> String {
        display_sas(&self.sas)
    }
}

/// Keep only the digits of user-entered SAS text.
pub fn normalize_sas(raw: &str) -> String {
    raw.chars().filter(char::is_ascii_digit).collect()
}

/// Characters a user may type between digit groups, as [`display_sas`]
/// shows them or as they are commonly read aloud.
const SAS_GROUP_SEPARATORS: [char; 2] = [' ', '-'];

/// Parse user-entered SAS text: exactly [`SAS_DIGITS`] digits, optionally
/// grouped with spaces or dashes. Anything else, such as "yes" or a code
/// with stray letters, is not a code and returns `None`.
pub fn parse_sas(raw: &str) -> Option<String> {
    let digits: String = raw
        .trim()
        .chars()
        .filter(|c| !SAS_GROUP_SEPARATORS.contains(c))
        .collect();
    let complete = digits.len() == SAS_DIGITS as usize;
    (complete && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(digits)
}

/// Group SAS digits in threes for display. Grouping is presentation only;
/// [`SasCheck::matches`] ignores it.
pub fn display_sas(sas: &str) -> String {
    // Three-digit groups keep a six-digit value readable without looking like
    // the ungrouped pairing code.
    const SAS_DISPLAY_GROUP: usize = 3;
    let digits: Vec<char> = sas.chars().collect();
    digits
        .chunks(SAS_DISPLAY_GROUP)
        .map(|group| group.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The local user's answer to a [`SasCheck`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SasDecision {
    /// The other device shows the same SAS: send our binding and store the room.
    Confirmed,
    /// The values differ, or the user cancelled: abort and store nothing.
    Rejected,
}

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
    /// Wait for a joiner, then ask `confirm` to compare the SAS before
    /// anything is sent to the peer or stored locally.
    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
    )]
    pub async fn wait<C, F>(
        self,
        store: &LocalStore,
        identity: &DeviceIdentity,
        confirm: C,
    ) -> Result<(), CoreError>
    where
        C: FnOnce(SasCheck) -> F + Send,
        F: Future<Output = SasDecision> + Send,
    {
        run_pairing(
            store,
            identity,
            self.conn,
            PairingRole::A,
            &self.password,
            self.session_id,
            identity.device_id.clone(),
            DeviceId::from(""),
            None,
            self.offer.room_id.clone(),
            DeviceRole::Owner,
            confirm,
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
    let url = format!("{}/v1/pairing/create", cfg.relay_url.trim_end_matches('/'));
    let resp = http_client()
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
    if !resp.status().is_success() {
        let body = relay_text(&resp.text().await.unwrap_or_default());
        return Err(CoreError::Pairing(format!("create failed: {body}")));
    }
    let created: CreatePairingResponse = resp.json().await?;
    // The code and room ID are printed and the room ID is stored.
    if !is_pairing_code(&created.pairing_code) {
        return Err(malformed("pairing code"));
    }
    check_relay_ids(&created.room_id, None)?;
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
pub async fn create_room<C, F>(
    store: &LocalStore,
    identity: &DeviceIdentity,
    cfg: &AppConfig,
    ttl_secs: u64,
    confirm: C,
) -> Result<RoomOffer, CoreError>
where
    C: FnOnce(SasCheck) -> F + Send,
    F: Future<Output = SasDecision> + Send,
{
    let pending = begin_create_room(identity, cfg, ttl_secs).await?;
    let offer = RoomOffer {
        pairing_code: pending.offer.pairing_code.clone(),
        room_id: pending.offer.room_id.clone(),
        expires_at: pending.offer.expires_at,
    };
    pending.wait(store, identity, confirm).await?;
    Ok(offer)
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn join_room<C, F>(
    store: &LocalStore,
    identity: &DeviceIdentity,
    cfg: &AppConfig,
    code: &str,
    confirm: C,
) -> Result<RoomRecord, CoreError>
where
    C: FnOnce(SasCheck) -> F + Send,
    F: Future<Output = SasDecision> + Send,
{
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
        .map_err(|e| {
            CoreError::Message(format!(
                "relay join {url}: {}",
                crate::error::format_error(&e)
            ))
        })?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        if body.contains("code_invalid") || body.contains("code_expired") {
            return Err(CoreError::Pairing(
                "unknown or expired pairing code; on the other device run `clipsync room create` again".into(),
            ));
        }
        let body = relay_text(&body);
        return Err(CoreError::Pairing(format!("join failed: {body}")));
    }
    let joined: JoinPairingResponse = resp.json().await?;
    check_relay_ids(&joined.room_id, Some(&joined.peer.device_id))?;
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
        Some(joined.peer.device_name.clone()),
        joined.room_id,
        DeviceRole::Member,
        confirm,
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
async fn run_pairing<C, F>(
    store: &LocalStore,
    identity: &DeviceIdentity,
    mut conn: RelayConnection,
    role: PairingRole,
    password: &str,
    session_id: PairingSessionId,
    device_a: DeviceId,
    device_b: DeviceId,
    mut peer_name: Option<String>,
    room_id: clipsync_protocol::RoomId,
    our_role: DeviceRole,
    confirm: C,
) -> Result<(), CoreError>
where
    C: FnOnce(SasCheck) -> F + Send,
    F: Future<Output = SasDecision> + Send,
{
    // The overall deadline also bounds how long the user may take to compare.
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
    let mut confirm = Some(confirm);
    let mut decision: Option<std::pin::Pin<Box<F>>> = None;
    let mut sas_confirmed = false;

    loop {
        tokio::select! {
            _ = &mut timeout => return Err(CoreError::Timeout),
            verdict = async {
                match decision.as_mut() {
                    Some(pending) => pending.await,
                    None => std::future::pending().await,
                }
            }, if decision.is_some() => {
                decision = None;
                if verdict == SasDecision::Rejected {
                    // Nothing has been sent to the peer or stored yet. Tell a
                    // peer that may already have confirmed, then abort.
                    let _ = conn
                        .send_control(ControlMessage::KeyConfirm {
                            pairing_session_id: session_id.clone(),
                            mac: SAS_ABORT_NOTICE_MAC.into(),
                        })
                        .await;
                    let _ = conn
                        .send_control(ControlMessage::Error {
                            code: SAS_REJECTED_CODE.into(),
                            message: "verification code rejected".into(),
                        })
                        .await;
                    conn.close().await;
                    // Keep the receiver alive until the transport has sent
                    // both frames and closed (`recv` then yields `None`).
                    let _ = tokio::time::timeout(SAS_REJECT_FLUSH, async {
                        while conn.incoming.recv().await.is_some() {}
                    })
                    .await;
                    return Err(CoreError::Pairing(
                        "verification code rejected; nothing was stored".into(),
                    ));
                }
                sas_confirmed = true;
            }
            incoming = conn.incoming.recv() => {
                let Some(incoming) = incoming else {
                    return Err(CoreError::Pairing("relay closed during pairing".into()));
                };
                match incoming {
                    Incoming::Control(ControlMessage::HelloOk { .. }) => {}
                    Incoming::Control(ControlMessage::PeerJoined { device }) => {
                        match &peer_id {
                            // The creator learns the joiner here, and SPAKE2
                            // starts right after: pin the identity and name
                            // now, while the relay cannot know any code.
                            None => {
                                check_relay_ids(&room_id, Some(&device.device_id))?;
                                peer_id = Some(device.device_id);
                                peer_name = Some(device.device_name);
                            }
                            // A repeated announcement changes nothing.
                            Some(pinned)
                                if *pinned == device.device_id
                                    && peer_name.as_deref() == Some(device.device_name.as_str()) => {}
                            // A relay running both SPAKE2 sessions knows both
                            // codes by now and could pick a name that forges
                            // a code line. The honest relay never does this.
                            Some(_) => {
                                let _ = conn
                                    .send_control(ControlMessage::Error {
                                        code: "pairing_failed".into(),
                                        message: "peer identity changed during pairing".into(),
                                    })
                                    .await;
                                conn.close().await;
                                return Err(CoreError::Pairing(
                                    "the relay changed the other device's identity during pairing; nothing was stored".into(),
                                ));
                            }
                        }
                    }
                    Incoming::Control(ControlMessage::Spake2Msg { message, .. }) => {
                        inbound_spake = Some(b64_decode(&message)?);
                    }
                    Incoming::Control(ControlMessage::KeyConfirm { mac, .. }) => {
                        if peer_confirm.is_some() {
                            // The peer's user rejected the SAS (see
                            // `SAS_ABORT_NOTICE_MAC`); nothing is stored here
                            // before our commit, which needs its binding.
                            return Err(CoreError::Pairing(
                                "the other device rejected the verification code; nothing was stored".into(),
                            ));
                        }
                        peer_confirm = Some(b64_decode(&mac)?);
                    }
                    Incoming::Control(ControlMessage::DeviceBind { nonce, ciphertext, .. }) => {
                        peer_bind = Some((nonce, ciphertext));
                    }
                    Incoming::Control(msg @ ControlMessage::PairingComplete { .. }) => {
                        complete = Some(msg);
                    }
                    Incoming::Control(ControlMessage::Error { code, message }) => {
                        let (code, message) = (relay_text(&code), relay_text(&message));
                        if our_bind_sent {
                            // The relay drops the session when the other device
                            // rejects, so our binding is refused afterwards.
                            return Err(CoreError::Pairing(format!(
                                "the other device rejected the verification code or the pairing expired; nothing was stored ({code}: {message})"
                            )));
                        }
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
                if let Some(ask) = confirm.take() {
                    let check = SasCheck {
                        sas: short_authentication_string(k),
                        peer_device_id: peer_id
                            .clone()
                            .ok_or_else(|| CoreError::Pairing("missing peer identity".into()))?,
                        // Shown next to the SAS: make relay escapes inert.
                        peer_device_name: sanitize_untrusted(
                            peer_name.as_deref().unwrap_or_default(),
                            MAX_PEER_NAME_COLUMNS,
                        ),
                    };
                    decision = Some(Box::pin(ask(check)));
                }
                if sas_confirmed && !our_bind_sent {
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
                }
            }
        }

        // Commit only after the local user confirmed the SAS and our binding
        // went out; until then a peer binding just waits in `peer_bind`.
        if !our_bind_sent {
            continue;
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
            let key_room = room_id.clone();
            if let Some(ControlMessage::PairingComplete {
                room_id,
                epoch,
                device_token,
                role,
            }) = complete.clone()
            {
                // The keys are stored under the room this pairing announced;
                // a different room in the completion would be printed and
                // stored without them.
                if room_id != key_room {
                    return Err(malformed("room ID in the pairing completion"));
                }
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
                // Keep the keys' ZeroizeOnDrop ownership in the blocking worker.
                // Filesystem durability and platform keychains must not stall
                // the runtime that services the pairing socket and watchdog.
                let key_material = k.clone();
                tokio::task::spawn_blocking(move || {
                    // Capture the whole zeroizing owner, not its Copy key field.
                    let keys = key_material;
                    let epoch_key = initial_epoch_key(&keys.room_root_key)?;
                    room_store.save_room_keys(
                        &key_room,
                        &hex::encode(keys.room_root_key),
                        &hex::encode(epoch_key),
                    )?;
                    room_store.upsert_room(room)?;
                    let mut cfg = room_store.load_config()?;
                    cfg.sync.enabled = true;
                    cfg.sync.auto_start = true;
                    cfg.sync.direction = clipsync_protocol::SyncDirection::Both;
                    room_store.save_config(&cfg)?;
                    Ok::<(), CoreError>(())
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

/// Test-only pair of confirmers that compare SAS values the way two users
/// would: each side shows its value to the other and confirms only on a match.
#[cfg(test)]
pub(crate) type SasPeerConfirm =
    Box<dyn FnOnce(SasCheck) -> std::pin::Pin<Box<dyn Future<Output = SasDecision> + Send>> + Send>;

#[cfg(test)]
pub(crate) fn sas_comparison() -> (SasPeerConfirm, SasPeerConfirm) {
    fn side(
        mine: tokio::sync::oneshot::Sender<String>,
        theirs: tokio::sync::oneshot::Receiver<String>,
    ) -> SasPeerConfirm {
        Box::new(move |check: SasCheck| {
            Box::pin(async move {
                let _ = mine.send(check.sas.clone());
                match theirs.await {
                    Ok(other) if check.matches(&other) => SasDecision::Confirmed,
                    _ => SasDecision::Rejected,
                }
            })
        })
    }
    let (creator_tx, creator_rx) = tokio::sync::oneshot::channel();
    let (joiner_tx, joiner_rx) = tokio::sync::oneshot::channel();
    (side(creator_tx, joiner_rx), side(joiner_tx, creator_rx))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use clipsync_crypto::{derive_nonce, PairingKeys};

    /// Real-clock budget for a scripted relay that answers immediately.
    const SCRIPT_DEADLINE: Duration = Duration::from_secs(8);

    /// A random pairing code per test process, so no test depends on a fixed
    /// secret value.
    static TEST_CODE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        // Six decimal digits, the relay's pairing-code format.
        const PAIRING_CODE_SPACE: u32 = 1_000_000;
        format!("{:06}", rand::random_range(0..PAIRING_CODE_SPACE))
    });

    async fn accept(_: SasCheck) -> SasDecision {
        SasDecision::Confirmed
    }

    /// Name a hostile relay reports for the joiner: it clears the line,
    /// returns the cursor, prints a fake code and reverses the text after it.
    const HOSTILE_PEER_NAME: &str =
        "Pixel\u{1b}[2K\r\u{9b}1Averification code: 000 000\u{202e}\u{2066}\u{200f}";
    /// Error text a hostile relay sends to overwrite the screen.
    const HOSTILE_RELAY_MESSAGE: &str = "unknown pairing session\u{1b}[1A\r\u{8}\u{202d}paired";

    /// True when `text` can move the cursor, overwrite or reorder what a
    /// terminal or text view shows.
    fn can_rewrite_display(text: &str) -> bool {
        text.chars().any(|c| {
            c.is_control() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    }

    fn secret_files(store: &LocalStore) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&store.paths.data_dir)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "secret"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What the scripted joiner does once both key confirmations passed.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Joiner {
        /// Wait for our binding, then bind and complete.
        BindsAfterUs,
        /// Bind at once like a pre-SAS peer, with an early completion.
        BindsFirst,
        /// Wait for our binding, then reject its own SAS prompt.
        RejectsAfterUs,
        /// Wait for our binding, then refuse it the way the relay does once
        /// the session was aborted (a lost or pre-notice rejection).
        RelayRefusesUs,
        /// After the client's key confirmation, when a relay running both
        /// SPAKE2 sessions knows both codes, re-announce the joiner under a
        /// name that forges a code line (and, with `new_id`, another device
        /// ID), then confirm and bind like [`Joiner::BindsAfterUs`].
        RenamesBeforeConfirm { new_id: bool },
    }

    /// Send without failing the script when the client already hung up.
    async fn try_send(socket: &mut axum::extract::ws::WebSocket, message: ControlMessage) {
        let _ = socket
            .send(axum::extract::ws::Message::Text(
                message.encode().unwrap().into(),
            ))
            .await;
    }

    /// The reviewer's forged name: ideographic spaces (two columns each)
    /// pad an 80-column line until the forged code starts the next line.
    fn forged_code_name(sas: &str) -> String {
        const PADDING: usize = 33;
        format!(
            "{}verification code: {}",
            "\u{3000}".repeat(PADDING),
            display_sas(sas)
        )
    }

    /// Scripted relay plus honest joiner (role B) sharing [`TEST_CODE`]. It reports
    /// the SAS it derived, then follows `script`. Returns the frames the
    /// client sent after key confirmation, up to its binding or `Error`.
    async fn honest_joiner(
        local: DeviceId,
        sid: PairingSessionId,
        room: clipsync_protocol::RoomId,
        script: Joiner,
        script_deadline: Duration,
    ) -> (
        crate::test_relay::Server,
        tokio::sync::oneshot::Receiver<String>,
        tokio::sync::oneshot::Receiver<Vec<ControlMessage>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use crate::test_relay::{receive, send, Server};
        let (sas_tx, sas_rx) = tokio::sync::oneshot::channel();
        let (frame_tx, frame_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release) = tokio::sync::oneshot::channel::<()>();
        let peer = DeviceId::from("sas-peer-remote");
        let server = Server::websocket_with_deadline(
            axum::Router::new(),
            script_deadline,
            move |mut socket| async move {
                send(
                    &mut socket,
                    ControlMessage::PeerJoined {
                        device: clipsync_protocol::PeerInfo {
                            device_id: peer.clone(),
                            device_name: HOSTILE_PEER_NAME.into(),
                            os: "android".into(),
                            role: DeviceRole::Member,
                            online: true,
                        },
                    },
                )
                .await;
                let ControlMessage::Spake2Msg { message, .. } = receive(&mut socket).await else {
                    panic!("client did not initiate SPAKE2")
                };
                let state = start_pairing_b(&TEST_CODE, sid.clone(), local, peer.clone()).unwrap();
                let outbound = state.outbound.clone();
                let keys = finish_pairing(state, &b64_decode(&message).unwrap()).unwrap();
                let _ = sas_tx.send(short_authentication_string(&keys));
                send(
                    &mut socket,
                    ControlMessage::Spake2Msg {
                        pairing_session_id: sid.clone(),
                        message: b64_encode(&outbound),
                    },
                )
                .await;
                let ControlMessage::KeyConfirm { mac, .. } = receive(&mut socket).await else {
                    panic!("client did not confirm key")
                };
                verify_confirm_mac(&keys, PairingRole::A, &b64_decode(&mac).unwrap()).unwrap();
                if let Joiner::RenamesBeforeConfirm { new_id } = script {
                    try_send(
                        &mut socket,
                        ControlMessage::PeerJoined {
                            device: clipsync_protocol::PeerInfo {
                                device_id: if new_id {
                                    DeviceId::from("sas-peer-renamed")
                                } else {
                                    peer.clone()
                                },
                                device_name: forged_code_name(&short_authentication_string(&keys)),
                                os: "android".into(),
                                role: DeviceRole::Member,
                                online: true,
                            },
                        },
                    )
                    .await;
                }
                try_send(
                    &mut socket,
                    ControlMessage::KeyConfirm {
                        pairing_session_id: sid.clone(),
                        mac: b64_encode(&confirm_mac(&keys, PairingRole::B)),
                    },
                )
                .await;
                let bind = {
                    let plain = BindPlain {
                        device_id: peer.clone(),
                        static_public: hex::encode([7; 32]),
                        device_name: "Pixel of the joiner".into(),
                        os: "android".into(),
                        fingerprint: "fixture fingerprint".into(),
                        epoch: 1,
                        epoch_key: hex::encode(initial_epoch_key(&keys.room_root_key).unwrap()),
                        role: DeviceRole::Member,
                    };
                    let nonce = device_bind_nonce(&keys.pairing_wrap_key, PairingRole::B).unwrap();
                    ControlMessage::DeviceBind {
                        pairing_session_id: sid.clone(),
                        nonce: hex::encode(nonce),
                        ciphertext: hex::encode(
                            seal(
                                &keys.pairing_wrap_key,
                                &nonce,
                                sid.as_str().as_bytes(),
                                &serde_json::to_vec(&plain).unwrap(),
                            )
                            .unwrap(),
                        ),
                    }
                };
                let complete = ControlMessage::PairingComplete {
                    room_id: room,
                    epoch: 1,
                    device_token: "synthetic-device-token".into(),
                    role: DeviceRole::Owner,
                };
                if script == Joiner::BindsFirst {
                    // A pre-SAS peer binds immediately; a hostile relay may also
                    // announce completion early. Neither may commit the pairing.
                    send(&mut socket, bind.clone()).await;
                    send(&mut socket, complete.clone()).await;
                }
                // No receive deadline here: the timeout test pauses the clock
                // while the client waits for an answer that never comes.
                let mut after_confirm = Vec::new();
                while let Some(Ok(frame)) = socket.recv().await {
                    let axum::extract::ws::Message::Text(text) = frame else {
                        continue;
                    };
                    match ControlMessage::decode(&text).unwrap() {
                        ControlMessage::Ping { ts } => {
                            send(&mut socket, ControlMessage::Pong { ts }).await;
                        }
                        message => {
                            let last = matches!(
                                message,
                                ControlMessage::DeviceBind { .. } | ControlMessage::Error { .. }
                            );
                            after_confirm.push(message);
                            if last {
                                break;
                            }
                        }
                    }
                }
                let bound = matches!(
                    after_confirm.last(),
                    Some(ControlMessage::DeviceBind { .. })
                );
                let _ = frame_tx.send(after_confirm);
                if bound {
                    match script {
                        Joiner::BindsAfterUs | Joiner::RenamesBeforeConfirm { .. } => {
                            send(&mut socket, bind).await;
                            send(&mut socket, complete).await;
                        }
                        Joiner::RejectsAfterUs => {
                            // The relay forwards this notice but not the
                            // joiner's following `Error`.
                            send(
                                &mut socket,
                                ControlMessage::KeyConfirm {
                                    pairing_session_id: sid.clone(),
                                    mac: SAS_ABORT_NOTICE_MAC.into(),
                                },
                            )
                            .await;
                        }
                        Joiner::RelayRefusesUs => {
                            send(
                                &mut socket,
                                ControlMessage::Error {
                                    code: "pairing_failed".into(),
                                    message: HOSTILE_RELAY_MESSAGE.into(),
                                },
                            )
                            .await;
                        }
                        Joiner::BindsFirst => {}
                    }
                }
                let _ = release.await;
            },
        )
        .await;
        (server, sas_rx, frame_rx, release_tx)
    }

    #[tokio::test]
    async fn pairing_commits_only_after_the_user_confirms_a_matching_sas() {
        for script in [Joiner::BindsAfterUs, Joiner::BindsFirst] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("sas-confirm-local".into(), [36; 32]);
            let sid = PairingSessionId::new();
            let room = clipsync_protocol::RoomId::new();
            let (mut server, peer_sas, mut frame, release) = honest_joiner(
                identity.device_id.clone(),
                sid.clone(),
                room.clone(),
                script,
                SCRIPT_DEADLINE,
            )
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let (shown_tx, shown) = tokio::sync::oneshot::channel();
            let (answer, answered) = tokio::sync::oneshot::channel();
            let pairing = run_pairing(
                &store,
                &identity,
                connection,
                PairingRole::A,
                &TEST_CODE,
                sid,
                identity.device_id.clone(),
                DeviceId::new(),
                None,
                room.clone(),
                DeviceRole::Owner,
                move |check: SasCheck| async move {
                    shown_tx.send(check).unwrap();
                    answered.await.unwrap()
                },
            );
            let driver = async {
                let check = tokio::time::timeout(Duration::from_secs(5), shown)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(check.sas, peer_sas.await.unwrap(), "honest peers agree");
                assert!(check.matches(&check.display()));
                assert_eq!(check.peer_device_id, DeviceId::from("sas-peer-remote"));
                // The relay-reported name is shown next to the code, so it
                // arrives inert: escapes visible, nothing that moves the cursor.
                assert!(
                    !can_rewrite_display(&check.peer_device_name),
                    "{:?}",
                    check.peer_device_name
                );
                assert!(check.peer_device_name.starts_with("Pixel"));
                assert!(check.peer_device_name.contains("\\u{1b}"));
                // While the user is still comparing, nothing is bound or stored.
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), &mut frame)
                        .await
                        .is_err(),
                    "DeviceBind sent before SAS confirmation"
                );
                assert!(store.load_state().unwrap().rooms.is_empty());
                assert!(secret_files(&store).is_empty());
                answer.send(SasDecision::Confirmed).unwrap();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(pairing, driver)
            })
            .await
            .unwrap();
            result.unwrap();
            assert!(matches!(
                frame.await.unwrap().as_slice(),
                [ControlMessage::DeviceBind { .. }]
            ));
            let _ = release.send(());
            server.finish().await;
            let stored = store.current_room().unwrap();
            assert_eq!(stored.room_id, room);
            assert_eq!(stored.peers[0].device_id, DeviceId::from("sas-peer-remote"));
            assert!(store.load_epoch_key(&room).is_ok());
        }
    }

    #[tokio::test]
    async fn rejected_sas_sends_no_binding_and_stores_no_room_keys_or_credentials() {
        for script in [Joiner::BindsAfterUs, Joiner::BindsFirst] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("sas-reject-local".into(), [37; 32]);
            let sid = PairingSessionId::new();
            let room = clipsync_protocol::RoomId::new();
            let (mut server, _peer_sas, frame, release) = honest_joiner(
                identity.device_id.clone(),
                sid.clone(),
                room.clone(),
                script,
                SCRIPT_DEADLINE,
            )
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let error = tokio::time::timeout(
                Duration::from_secs(10),
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::A,
                    &TEST_CODE,
                    sid,
                    identity.device_id.clone(),
                    DeviceId::new(),
                    None,
                    room.clone(),
                    DeviceRole::Owner,
                    |_| async { SasDecision::Rejected },
                ),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(
                error.to_string().contains("verification code rejected"),
                "{error}"
            );
            // The relay forwards the abort notice to a peer that may already
            // have confirmed, then drops the session on the `Error`.
            let frames = frame.await.unwrap();
            let [ControlMessage::KeyConfirm { mac, .. }, ControlMessage::Error { code, .. }] =
                frames.as_slice()
            else {
                panic!("rejection must notify the peer and abort instead of binding: {frames:?}")
            };
            assert_eq!(mac, SAS_ABORT_NOTICE_MAC);
            assert_eq!(code, SAS_REJECTED_CODE);
            let _ = release.send(());
            server.finish().await;
            assert!(store.load_state().unwrap().rooms.is_empty());
            assert!(secret_files(&store).is_empty());
        }
    }

    #[tokio::test]
    async fn peer_rejection_after_our_confirmation_ends_pairing_promptly() {
        for (script, expected) in [
            (
                Joiner::RejectsAfterUs,
                "the other device rejected the verification code; nothing was stored",
            ),
            (
                Joiner::RelayRefusesUs,
                "the other device rejected the verification code or the pairing expired",
            ),
        ] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("sas-peer-rejects-local".into(), [39; 32]);
            let sid = PairingSessionId::new();
            let room = clipsync_protocol::RoomId::new();
            let (mut server, _peer_sas, frame, release) = honest_joiner(
                identity.device_id.clone(),
                sid.clone(),
                room.clone(),
                script,
                SCRIPT_DEADLINE,
            )
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            // Far below the pairing deadline the client would otherwise wait out.
            let error = tokio::time::timeout(
                SCRIPT_DEADLINE,
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::A,
                    &TEST_CODE,
                    sid,
                    identity.device_id.clone(),
                    DeviceId::new(),
                    None,
                    room,
                    DeviceRole::Owner,
                    accept,
                ),
            )
            .await
            .expect("a confirmed device must not wait out the deadline after a peer rejection")
            .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
            // Relay error text ends up on the terminal; it must stay inert.
            assert!(!can_rewrite_display(&error.to_string()), "{error:?}");
            assert!(matches!(
                frame.await.unwrap().as_slice(),
                [ControlMessage::DeviceBind { .. }]
            ));
            let _ = release.send(());
            server.finish().await;
            assert!(store.load_state().unwrap().rooms.is_empty());
            assert!(secret_files(&store).is_empty());
        }
    }

    #[tokio::test]
    async fn unanswered_sas_prompt_times_out_without_storing_anything() {
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("sas-timeout-local".into(), [38; 32]);
        let sid = PairingSessionId::new();
        let room = clipsync_protocol::RoomId::new();
        let (mut server, _peer_sas, _frame, release) = honest_joiner(
            identity.device_id.clone(),
            sid.clone(),
            room.clone(),
            Joiner::BindsFirst,
            // Outlive the client's paused-clock pairing deadline.
            Duration::from_secs(clipsync_protocol::PAIRING_TTL_SECS * 2),
        )
        .await;
        let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
            .await
            .unwrap();
        let (shown_tx, shown) = tokio::sync::oneshot::channel();
        let pairing = tokio::spawn({
            let store = store.clone();
            async move {
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::A,
                    &TEST_CODE,
                    sid,
                    identity.device_id.clone(),
                    DeviceId::new(),
                    None,
                    room,
                    DeviceRole::Owner,
                    move |_| async move {
                        shown_tx.send(()).unwrap();
                        std::future::pending::<SasDecision>().await
                    },
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), shown)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        let result = tokio::time::timeout(Duration::from_secs(150), pairing)
            .await
            .unwrap()
            .unwrap();
        tokio::time::resume();
        let _ = release.send(());
        server.finish().await;
        assert!(matches!(result, Err(CoreError::Timeout)), "{result:?}");
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert!(secret_files(&store).is_empty());
    }

    #[tokio::test]
    async fn a_relay_cannot_rename_the_peer_after_spake2_started() {
        for new_id in [false, true] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("sas-rename-local".into(), [37; 32]);
            let sid = PairingSessionId::new();
            let room = clipsync_protocol::RoomId::new();
            let (mut server, _peer_sas, frame, release) = honest_joiner(
                identity.device_id.clone(),
                sid.clone(),
                room.clone(),
                Joiner::RenamesBeforeConfirm { new_id },
                SCRIPT_DEADLINE,
            )
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let (shown_tx, mut shown) = tokio::sync::oneshot::channel();
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::A,
                    &TEST_CODE,
                    sid,
                    identity.device_id.clone(),
                    DeviceId::new(),
                    None,
                    room,
                    DeviceRole::Owner,
                    move |check: SasCheck| async move {
                        let _ = shown_tx.send(check);
                        SasDecision::Confirmed
                    },
                ),
            )
            .await
            .unwrap();
            let after_confirm = frame.await.unwrap();
            let _ = release.send(());
            server.finish().await;
            // The relay knew both codes when it renamed the peer, so the
            // user must never compare against a name chosen at that point.
            assert!(
                shown.try_recv().is_err(),
                "new_id={new_id}: SAS shown after a late rename"
            );
            let error = result.expect_err("pairing survived a late rename");
            assert!(
                error.to_string().contains("changed the other device"),
                "new_id={new_id}: {error}"
            );
            assert!(
                !after_confirm
                    .iter()
                    .any(|frame| matches!(frame, ControlMessage::DeviceBind { .. })),
                "new_id={new_id}: bound after a late rename"
            );
            assert!(store.load_state().unwrap().rooms.is_empty());
            assert!(secret_files(&store).is_empty());
        }
    }

    /// Scripted relay plus honest creator (role A) for a client joining as
    /// role B. With `rename`, the relay announces the creator under a forged
    /// name after both SPAKE2 messages crossed. Reports the creator's SAS.
    async fn honest_creator(
        local: DeviceId,
        creator: DeviceId,
        sid: PairingSessionId,
        room: clipsync_protocol::RoomId,
        rename: bool,
    ) -> (
        crate::test_relay::Server,
        tokio::sync::oneshot::Receiver<String>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        use crate::test_relay::{receive, send, Server};
        let (sas_tx, sas_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release) = tokio::sync::oneshot::channel::<()>();
        let server = Server::websocket(move |mut socket| async move {
            send(
                &mut socket,
                ControlMessage::HelloOk {
                    room_id: room,
                    peer_online: false,
                    peers: Vec::new(),
                    queued_text: None,
                    device_token: None,
                },
            )
            .await;
            let ControlMessage::Spake2Msg { message, .. } = receive(&mut socket).await else {
                panic!("joiner did not send SPAKE2")
            };
            let state = start_pairing_a(&TEST_CODE, sid.clone(), creator.clone(), local).unwrap();
            let outbound = state.outbound.clone();
            let keys = finish_pairing(state, &b64_decode(&message).unwrap()).unwrap();
            let sas = short_authentication_string(&keys);
            send(
                &mut socket,
                ControlMessage::Spake2Msg {
                    pairing_session_id: sid.clone(),
                    message: b64_encode(&outbound),
                },
            )
            .await;
            if rename {
                try_send(
                    &mut socket,
                    ControlMessage::PeerJoined {
                        device: clipsync_protocol::PeerInfo {
                            device_id: creator.clone(),
                            device_name: forged_code_name(&sas),
                            os: "macos".into(),
                            role: DeviceRole::Owner,
                            online: true,
                        },
                    },
                )
                .await;
            }
            let _ = sas_tx.send(sas);
            if let ControlMessage::KeyConfirm { mac, .. } = receive(&mut socket).await {
                verify_confirm_mac(&keys, PairingRole::B, &b64_decode(&mac).unwrap()).unwrap();
            }
            try_send(
                &mut socket,
                ControlMessage::KeyConfirm {
                    pairing_session_id: sid,
                    mac: b64_encode(&confirm_mac(&keys, PairingRole::A)),
                },
            )
            .await;
            while let Some(Ok(_)) = socket.recv().await {}
            let _ = release.await;
        })
        .await;
        (server, sas_rx, release_tx)
    }

    #[tokio::test]
    async fn joiner_shows_the_creator_name_inert_and_refuses_a_late_rename() {
        for rename in [false, true] {
            let directory = tempfile::TempDir::new().unwrap();
            let store =
                LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
            let identity = DeviceIdentity::from_bytes("sas-joiner-local".into(), [38; 32]);
            let creator = DeviceId::from("sas-creator-remote");
            let sid = PairingSessionId::new();
            let room = clipsync_protocol::RoomId::new();
            let (mut server, creator_sas, release) = honest_creator(
                identity.device_id.clone(),
                creator.clone(),
                sid.clone(),
                room.clone(),
                rename,
            )
            .await;
            let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
                .await
                .unwrap();
            let (shown_tx, mut shown) = tokio::sync::oneshot::channel();
            // The name from the join response, as `join_room` passes it.
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                run_pairing(
                    &store,
                    &identity,
                    connection,
                    PairingRole::B,
                    &TEST_CODE,
                    sid,
                    creator.clone(),
                    identity.device_id.clone(),
                    Some(HOSTILE_PEER_NAME.into()),
                    room,
                    DeviceRole::Member,
                    move |check: SasCheck| async move {
                        let _ = shown_tx.send(check);
                        SasDecision::Rejected
                    },
                ),
            )
            .await
            .unwrap();
            let creator_sas = creator_sas.await.unwrap();
            let _ = release.send(());
            server.finish().await;
            let error = result.expect_err("a rejected or renamed pairing must fail");
            if rename {
                assert!(shown.try_recv().is_err(), "SAS shown after a late rename");
                assert!(
                    error.to_string().contains("changed the other device"),
                    "{error}"
                );
            } else {
                let check = shown.try_recv().expect("joiner never showed the SAS");
                assert_eq!(check.sas, creator_sas, "honest peers agree");
                assert_eq!(check.peer_device_id, creator);
                assert!(
                    !can_rewrite_display(&check.peer_device_name),
                    "{:?}",
                    check.peer_device_name
                );
                assert!(check.peer_device_name.contains("\\u{1b}"));
            }
            assert!(store.load_state().unwrap().rooms.is_empty());
            assert!(secret_files(&store).is_empty());
        }
    }

    #[test]
    fn sas_check_matching_ignores_grouping_and_rejects_empty_input() {
        let check = SasCheck {
            sas: "482913".into(),
            peer_device_id: DeviceId::from("peer"),
            peer_device_name: "peer".into(),
        };
        assert_eq!(check.display(), "482 913");
        for same in ["482913", "482 913", "482-913", " 48 29 13 "] {
            assert!(check.matches(same), "{same}");
        }
        for different in [
            "",
            "yes",
            "482914",
            "4829130",
            "48291",
            "code 482913",
            "482913x",
            "48_29_13",
        ] {
            assert!(!check.matches(different), "{different}");
            assert_ne!(parse_sas(different).as_deref(), Some("482913"));
        }
        assert_eq!(parse_sas(" 482-913\n").as_deref(), Some("482913"));
        assert_eq!(parse_sas("yes"), None);
        assert_eq!(normalize_sas("4a8-2 9"), "4829");
    }

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
    async fn http_pairing_failures_preserve_relay_details_and_expired_code_guidance() {
        use axum::{http::StatusCode, routing::post, Router};
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("http-failure".into(), [31; 32]);
        for body in [
            "code_invalid",
            "code_expired",
            "busy: joiner already connected",
        ] {
            let server = crate::test_relay::Server::http(
                Router::new()
                    .route(
                        "/v1/pairing/create",
                        post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "maintenance") }),
                    )
                    .route(
                        "/v1/pairing/join",
                        post(move || async move { (StatusCode::CONFLICT, body) }),
                    ),
            )
            .await;
            let cfg = AppConfig {
                relay_url: format!("{}/", server.url),
                ..Default::default()
            };
            let create = begin_create_room(&identity, &cfg, 60).await.err().unwrap();
            assert!(create.to_string().contains("create failed: maintenance"));
            let join = join_room(&store, &identity, &cfg, &TEST_CODE, accept)
                .await
                .unwrap_err();
            if body.starts_with("code_") {
                assert!(join.to_string().contains("unknown or expired pairing code"));
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
        assert!(join_room(&store, &identity, &cfg, &TEST_CODE, accept)
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
            create_room(&store, &identity, &cfg, 60, accept).await,
            Err(CoreError::Http(_))
        ));
        assert!(matches!(
            join_room(&store, &identity, &cfg, &TEST_CODE, accept).await,
            Err(CoreError::Http(_))
        ));
        assert!(store.load_state().unwrap().rooms.is_empty());
    }

    #[tokio::test]
    async fn relay_codes_and_ids_that_could_reach_the_terminal_are_refused() {
        use axum::{routing::post, Json, Router};
        // An OSC 52 clipboard write and an 8-bit CSI, as the CLI would print
        // them right after "pairing code:" or "room:".
        const OSC: &str = "\u{1b}]52;c;aGk=\u{7}";
        const CSI: &str = "\u{9b}2J";
        let good_room = clipsync_protocol::RoomId::new();
        let creates = [
            (format!("12345{OSC}"), good_room.clone(), "pairing code"),
            ("12345".to_string(), good_room.clone(), "pairing code"),
            ("1234567".to_string(), good_room.clone(), "pairing code"),
            (
                "12345\u{661}".to_string(),
                good_room.clone(),
                "pairing code",
            ),
            (
                TEST_CODE.clone(),
                clipsync_protocol::RoomId::from(format!("room{CSI}")),
                "room ID",
            ),
        ];
        let peer = |device_id: &str| clipsync_protocol::PeerInfo {
            device_id: DeviceId::from(device_id),
            device_name: "Laptop".into(),
            os: "macos".into(),
            role: DeviceRole::Owner,
            online: true,
        };
        let joins = [
            (
                clipsync_protocol::RoomId::from(format!("room{OSC}")),
                peer("creator"),
                "room ID",
            ),
            (
                good_room.clone(),
                peer(&format!("creator{CSI}")),
                "device ID",
            ),
            (good_room.clone(), peer(""), "device ID"),
        ];
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("malformed-ids".into(), [39; 32]);
        for (pairing_code, room_id, what) in creates {
            let created = CreatePairingResponse {
                room_id,
                pairing_code,
                pairing_token: "synthetic-token".into(),
                expires_at: Utc::now(),
                ws_path: "/v1/ws".into(),
            };
            let server = crate::test_relay::Server::http(Router::new().route(
                "/v1/pairing/create",
                post(move || async move { Json(created) }),
            ))
            .await;
            let cfg = AppConfig {
                relay_url: server.url.clone(),
                ..Default::default()
            };
            let error = begin_create_room(&identity, &cfg, 60).await.err().unwrap();
            assert!(
                error.to_string().contains(&format!("malformed {what}")),
                "{error}"
            );
        }
        for (room_id, peer, what) in joins {
            let joined = JoinPairingResponse {
                pairing_session_id: PairingSessionId::new(),
                room_id,
                pairing_token: "synthetic-token".into(),
                peer,
                expires_at: Utc::now(),
                ws_path: "/v1/ws".into(),
            };
            let server = crate::test_relay::Server::http(Router::new().route(
                "/v1/pairing/join",
                post(move || async move { Json(joined) }),
            ))
            .await;
            let cfg = AppConfig {
                relay_url: server.url.clone(),
                ..Default::default()
            };
            let error = join_room(&store, &identity, &cfg, &TEST_CODE, accept)
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains(&format!("malformed {what}")),
                "{error}"
            );
        }
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert!(secret_files(&store).is_empty());
    }

    #[tokio::test]
    async fn a_completion_for_another_room_stores_nothing() {
        let directory = tempfile::TempDir::new().unwrap();
        let store =
            LocalStore::open_from(crate::AppPaths::from_root(directory.path().into())).unwrap();
        let identity = DeviceIdentity::from_bytes("sas-other-room".into(), [40; 32]);
        let sid = PairingSessionId::new();
        // The relay completes into a room other than the one it announced.
        let (mut server, _peer_sas, frame, release) = honest_joiner(
            identity.device_id.clone(),
            sid.clone(),
            clipsync_protocol::RoomId::from("01JC8Z3V7Q4N2M5K6P8R9S0T1W"),
            Joiner::BindsAfterUs,
            SCRIPT_DEADLINE,
        )
        .await;
        let connection = RelayConnection::connect(&join_ws(&server.url, "/v1/ws"))
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            run_pairing(
                &store,
                &identity,
                connection,
                PairingRole::A,
                &TEST_CODE,
                sid,
                identity.device_id.clone(),
                DeviceId::new(),
                None,
                clipsync_protocol::RoomId::new(),
                DeviceRole::Owner,
                accept,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        let _ = frame.await;
        let _ = release.send(());
        server.finish().await;
        assert!(error.to_string().contains("malformed room ID"), "{error}");
        assert!(store.load_state().unwrap().rooms.is_empty());
        assert!(secret_files(&store).is_empty());
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
                &TEST_CODE,
                PairingSessionId::new(),
                identity.device_id.clone(),
                DeviceId::new(),
                None,
                clipsync_protocol::RoomId::new(),
                DeviceRole::Owner,
                accept,
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
                &TEST_CODE,
                PairingSessionId::new(),
                identity.device_id.clone(),
                DeviceId::new(),
                None,
                clipsync_protocol::RoomId::new(),
                DeviceRole::Owner,
                accept,
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
                let state = start_pairing_b(&TEST_CODE, session.clone(), local, peer_id).unwrap();
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
                // Confirm honestly so the client reaches its SAS prompt and
                // then parses the hostile binding.
                send(
                    &mut socket,
                    ControlMessage::KeyConfirm {
                        pairing_session_id: session.clone(),
                        mac: b64_encode(&confirm_mac(&keys, PairingRole::B)),
                    },
                )
                .await;
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
                    &TEST_CODE,
                    sid,
                    identity.device_id.clone(),
                    peer,
                    None,
                    clipsync_protocol::RoomId::new(),
                    DeviceRole::Owner,
                    accept,
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
