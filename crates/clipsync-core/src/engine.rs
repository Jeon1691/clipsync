// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use clipsync_clipboard::{wait_for_change, ClipboardItem, FileRef, ImageMime, SystemClipboard};
use clipsync_crypto::{derive_content_key, derive_nonce, open, seal, DeviceIdentity};
use clipsync_daemon::{
    read_request, write_pid, write_response, IpcRequest, IpcResponse, IpcServer, PullPayload,
    StatusPayload,
};
use clipsync_protocol::{
    decode_chunk_frame, envelope_aad, AckStatus, ClientRole, ControlMessage, DeviceId,
    EnvelopeFrame, FileManifest, HandshakeCapabilities, ImageManifest, ItemKind, ManifestPlain,
    MessageId, PeerInfo, Recipient, RoutingHeader, TransferId, PROTOCOL_VERSION,
};
use clipsync_storage::{LocalStore, RoomRecord};
use clipsync_transfer::{
    open_chunk, prune_idle_transfers, split_and_encrypt, IncomingTransfer, StagingArea,
    TransferError,
};
use clipsync_transport::{backoff_delay, http_to_ws, join_ws, Incoming, RelayConnection};

use crate::envelope::{
    check_header, check_version, commit_sequence, sending_version, sequence_floor, EnvelopePolicy,
    EnvelopeRejection,
};
use crate::handshake::{HandshakeOutcome, HandshakeTracker};
use crate::loop_guard::LoopGuard;
use crate::notify::Notifier;
use crate::{current_os, device_name, CoreError, PullResult};

pub struct SyncHandle {
    pub paused: Arc<AtomicBool>,
    pub connected: Arc<AtomicBool>,
    pub peer_online: Arc<AtomicBool>,
    pub last_item: Arc<Mutex<Option<ClipboardItem>>>,
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn run_daemon(clipboard: Option<SystemClipboard>) -> Result<(), CoreError> {
    let store = LocalStore::open()?;
    let _ = crate::App {
        store: store.clone(),
    }
    .identity()?;
    write_pid(&store.paths).ok();
    let clipboard = match clipboard {
        Some(c) => c,
        None => SystemClipboard::open()?,
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = SyncHandle {
        paused: Arc::new(AtomicBool::new(store.load_state()?.paused)),
        connected: Arc::new(AtomicBool::new(false)),
        peer_online: Arc::new(AtomicBool::new(false)),
        last_item: Arc::new(Mutex::new(None)),
    };
    let ipc = IpcServer::bind(&store.paths).await;
    let engine = tokio::spawn(sync_loop(
        store.clone(),
        clipboard,
        handle.clone_inner(),
        shutdown_rx,
    ));
    if let Ok(server) = ipc {
        loop {
            tokio::select! {
                accept = server.accept() => {
                    if let Ok(mut stream) = accept {
                        let h = handle.clone_inner();
                        let store2 = LocalStore::open_from(store.paths.clone())?;
                        tokio::spawn(async move {
                            if let Ok(Some(req)) = read_request(&mut stream).await {
                                let resp = handle_ipc(&store2, &h, req).await;
                                let _ = write_response(&mut stream, &resp).await;
                            }
                        });
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    let _ = shutdown_tx.send(true);
                    break;
                }
            }
        }
    } else {
        tokio::signal::ctrl_c().await.ok();
        let _ = shutdown_tx.send(true);
    }
    let _ = engine.await;
    clipsync_daemon::remove_pid(&store.paths);
    Ok(())
}

/// A manifest accepted by [`Inbound::manifest`], kept until its chunks arrive.
type PendingManifest = (RoutingHeader, ManifestPlain);

fn drop_idle_transfers(
    incoming: &mut std::collections::HashMap<String, IncomingTransfer>,
    manifests: &mut std::collections::HashMap<String, PendingManifest>,
) {
    let ttl = std::time::Duration::from_secs(clipsync_protocol::TRANSFER_IDLE_SECS);
    let _ = prune_idle_transfers(incoming, std::time::Instant::now(), ttl);
    manifests.retain(|id, _| incoming.contains_key(id));
}

impl SyncHandle {
    fn clone_inner(&self) -> Self {
        Self {
            paused: self.paused.clone(),
            connected: self.connected.clone(),
            peer_online: self.peer_online.clone(),
            last_item: self.last_item.clone(),
        }
    }
}

async fn handle_ipc(store: &LocalStore, handle: &SyncHandle, req: IpcRequest) -> IpcResponse {
    match req {
        IpcRequest::Status => {
            let room = store.current_room().ok();
            let identity = store.secrets.load_identity().ok();
            let cfg = store.load_config().ok();
            IpcResponse {
                ok: true,
                error: None,
                status: Some(StatusPayload {
                    daemon: true,
                    paused: handle.paused.load(Ordering::Relaxed),
                    connected: handle.connected.load(Ordering::Relaxed),
                    peer_online: handle.peer_online.load(Ordering::Relaxed),
                    room_id: room.as_ref().map(|r| r.room_id.clone()),
                    device_id: identity.as_ref().map(|i| i.device_id.clone()),
                    device_name: Some(device_name()),
                    relay_url: cfg.map(|c| c.relay_url),
                    last_item_kind: handle.last_item.lock().as_ref().map(|i| i.kind()),
                    last_sync_at: None,
                }),
                pull: None,
            }
        }
        IpcRequest::Pause => {
            handle.paused.store(true, Ordering::Relaxed);
            if let Ok(mut st) = store.load_state() {
                st.paused = true;
                let _ = store.save_state(&st);
            }
            IpcResponse {
                ok: true,
                error: None,
                status: None,
                pull: None,
            }
        }
        IpcRequest::Resume => {
            handle.paused.store(false, Ordering::Relaxed);
            if let Ok(mut st) = store.load_state() {
                st.paused = false;
                let _ = store.save_state(&st);
            }
            IpcResponse {
                ok: true,
                error: None,
                status: None,
                pull: None,
            }
        }
        IpcRequest::Pull => {
            let item = handle.last_item.lock().clone();
            IpcResponse {
                ok: true,
                error: None,
                status: None,
                pull: item.map(|i| PullPayload {
                    kind: format!("{:?}", i.kind()).to_lowercase(),
                    text: match &i {
                        ClipboardItem::Text { text } => Some(text.clone()),
                        _ => None,
                    },
                    summary: i.summary(),
                }),
            }
        }
        IpcRequest::PushText { text } => {
            *handle.last_item.lock() = Some(ClipboardItem::Text { text });
            IpcResponse {
                ok: true,
                error: Some("queued locally; live push uses clipboard watch".into()),
                status: None,
                pull: None,
            }
        }
        IpcRequest::Shutdown => IpcResponse {
            ok: true,
            error: None,
            status: None,
            pull: None,
        },
    }
}

async fn sync_loop(
    store: LocalStore,
    clipboard: SystemClipboard,
    handle: SyncHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut attempt = 0u32;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let started = std::time::Instant::now();
        match run_session(&store, &clipboard, &handle, &mut shutdown).await {
            Ok(()) => attempt = 0,
            Err(e) => {
                warn!(error = %e, "sync session ended");
                handle.connected.store(false, Ordering::Relaxed);
                attempt = clipsync_transport::next_attempt(attempt, started.elapsed());
                let delay = backoff_delay(attempt);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.changed() => break,
                }
            }
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn run_session(
    store: &LocalStore,
    clipboard: &SystemClipboard,
    handle: &SyncHandle,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), CoreError> {
    let cfg = store.load_config()?;
    let identity = store.secrets.load_identity()?;
    let room = store.current_room()?;
    let epoch_key = store.load_epoch_key(&room.room_id)?;
    let ws = join_ws(&cfg.relay_url, "/v1/ws");
    let mut conn = RelayConnection::connect(&ws).await?;
    conn.send_control(ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        role: ClientRole::Member,
        room_id: Some(room.room_id.clone()),
        token: room.device_token.clone(),
        pairing_session_id: None,
    })
    .await?;
    let hello = wait_hello_ok_msg(&mut conn).await?;

    let mut guard = LoopGuard::default();
    let notifier = Notifier::new(cfg.notify.clone());
    let staging = StagingArea::new(store.paths.inbox_dir.clone())?;
    let mut last_sig = clipboard.signature().await.unwrap_or_else(|_| "0".into());
    let mut incoming: HashMap<String, IncomingTransfer> = HashMap::new();
    let mut manifests: HashMap<String, PendingManifest> = HashMap::new();
    let policy = EnvelopePolicy::from_config(&cfg.sync);
    let inbound = Inbound {
        store,
        room: &room,
        local: &identity.device_id,
        epoch_key: &epoch_key,
        policy,
    };
    let mut handshakes = HandshakeTracker::new(
        HandshakeCapabilities::accepting(policy.accepted_versions())
            .encode()
            .map_err(TransferError::from)?,
    );
    handle.connected.store(true, Ordering::Relaxed);
    apply_hello_ok(
        &hello,
        &identity,
        &room,
        &inbound,
        clipboard,
        &mut guard,
        handle,
        &notifier,
        &mut last_sig,
        &mut handshakes,
        &conn,
    )
    .await;

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    conn.close().await;
                    return Ok(());
                }
            }
            change = wait_for_change(clipboard, &last_sig) => {
                match change {
                    Ok((sig, Some(item))) => {
                        last_sig = sig;
                        if handle.paused.load(Ordering::Relaxed) {
                            continue;
                        }
                        if !cfg.sync.enabled {
                            continue;
                        }
                        if matches!(cfg.sync.direction, clipsync_protocol::SyncDirection::ReceiveOnly) {
                            continue;
                        }
                        if guard.is_echo(&item) {
                            debug!("suppress echo");
                            continue;
                        }
                        if let Err(e) = item.validate() {
                            warn!(error = %e, "skip invalid clipboard item");
                            continue;
                        }
                        let stamp = match EnvelopeStamp::next(store, &room, &identity.device_id, policy) {
                            Ok(stamp) => stamp,
                            Err(e) => {
                                warn!(error = %e, "cannot reserve envelope sequence");
                                return Err(e);
                            }
                        };
                        if let Err(e) = send_item(&conn, &identity, &room, &epoch_key, stamp, &item).await {
                            warn!(error = %e, "send failed; reconnecting");
                            return Err(e);
                        }
                    }
                    Ok((sig, None)) => last_sig = sig,
                    Err(e) => warn!(error = %e, "clipboard watch"),
                }
            }
            incoming_msg = conn.incoming.recv() => {
                let Some(msg) = incoming_msg else {
                    return Err(CoreError::Message("relay disconnected".into()));
                };
                match msg {
                    Incoming::Control(hello @ ControlMessage::HelloOk { .. }) => {
                        apply_hello_ok(
                            &hello,
                            &identity,
                            &room,
                            &inbound,
                            clipboard,
                            &mut guard,
                            handle,
                            &notifier,
                            &mut last_sig,
                            &mut handshakes,
                            &conn,
                        )
                        .await;
                    }
                    Incoming::Control(ControlMessage::PeerJoined { device }) => {
                        handle.peer_online.store(true, Ordering::Relaxed);
                        // The peer (re)connected: its handshake state is new.
                        if let Some(message) = handshakes.initiate(&identity, &room, &device.device_id, true) {
                            send_handshake(&conn, message).await;
                        }
                    }
                    Incoming::Control(ControlMessage::PeerLeft { .. }) => {
                        handle.peer_online.store(false, Ordering::Relaxed);
                    }
                    Incoming::Control(ControlMessage::NoiseHandshake { message }) => {
                        let bytes = hex::decode(&message).unwrap_or_default();
                        let outcome = handshakes.receive(&identity, &room, &bytes);
                        finish_handshake(store, &room, &conn, outcome).await;
                    }
                    Incoming::Control(ControlMessage::TextEnvelope { routing, nonce, ciphertext }) => {
                        if guard.seen_message(routing.message_id.as_str()) {
                            continue;
                        }
                        match inbound.text(&routing, &nonce, &ciphertext) {
                            Ok(item) => {
                                apply_remote(clipboard, &mut guard, handle, &notifier, routing.message_id.as_str(), item, &mut last_sig).await;
                                let _ = conn.send_control(ControlMessage::Ack {
                                    message_id: routing.message_id,
                                    transfer_id: Some(routing.transfer_id),
                                    status: AckStatus::Delivered,
                                    detail: None,
                                }).await;
                            }
                            Err(e) => warn!(error = %e, "text envelope rejected"),
                        }
                    }
                    Incoming::Control(ControlMessage::Manifest { routing, nonce, ciphertext, .. }) => {
                        drop_idle_transfers(&mut incoming, &mut manifests);
                        match inbound.manifest(&routing, &nonce, &ciphertext) {
                            Ok(manifest) => {
                                let id = routing.transfer_id.clone();
                                let t = staging.begin(&id, manifest.chunk_count);
                                incoming.insert(id.0.clone(), t);
                                manifests.insert(id.0, (routing, manifest));
                            }
                            Err(e) => warn!(error = %e, "manifest rejected"),
                        }
                    }
                    Incoming::Control(ControlMessage::Ack { status, .. }) => {
                        debug!(?status, "ack");
                    }
                    Incoming::Control(ControlMessage::Error { code, message }) => {
                        warn!(code, message, "relay error");
                    }
                    Incoming::Binary(buf) => {
                        drop_idle_transfers(&mut incoming, &mut manifests);
                        match open_pending_chunk(&epoch_key, &manifests, &buf) {
                            Ok((tid, idx, count, pt)) => {
                                if let Some(t) = incoming.get_mut(tid.as_str()) {
                                    if t.push(idx, count, pt).is_err() {
                                        t.fail();
                                        continue;
                                    }
                                    if t.is_complete() {
                                        if let Some((_, man)) = manifests.remove(tid.as_str()) {
                                            if let Some(xfer) = incoming.remove(tid.as_str()) {
                                                match xfer.assemble(&man.content_hash) {
                                                    Ok(payload) => {
                                                        if let Ok(item) = item_from_payload(&man, payload, &staging, &tid).await {
                                                            apply_remote(clipboard, &mut guard, handle, &notifier, tid.as_str(), item, &mut last_sig).await;
                                                        }
                                                    }
                                                    Err(e) => warn!(error = %e, "assemble"),
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => debug!(error = %e, "chunk ignored"),
                        }
                    }
                    Incoming::Pong => {}
                    Incoming::Control(_) => {}
                }
            }
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn send_item(
    conn: &RelayConnection,
    identity: &DeviceIdentity,
    room: &RoomRecord,
    epoch_key: &[u8; 32],
    stamp: EnvelopeStamp,
    item: &ClipboardItem,
) -> Result<(), CoreError> {
    let transfer_id = TransferId::new();
    let message_id = MessageId::new();
    let now = Utc::now();
    let ttl = ChronoDuration::seconds(clipsync_protocol::DEFAULT_TTL_SECS as i64);
    let routing = RoutingHeader {
        version: stamp.version,
        message_id,
        transfer_id: transfer_id.clone(),
        room_id: room.room_id.clone(),
        sender_device_id: identity.device_id.clone(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + ttl,
        item_kind: item.kind(),
        seq: stamp.seq,
    };
    match item {
        ClipboardItem::Text { text } => {
            let (nonce, ciphertext) = seal_text(epoch_key, &routing, text)?;
            conn.send_control(ControlMessage::TextEnvelope {
                routing,
                nonce,
                ciphertext,
            })
            .await?;
        }
        ClipboardItem::Image {
            mime,
            bytes,
            width,
            height,
        } => {
            let hash = clipsync_crypto::blake3_hex(bytes);
            let manifest = ManifestPlain {
                item_kind: ItemKind::Image,
                total_bytes: bytes.len() as u64,
                chunk_count: 0,
                content_hash: hash.clone(),
                files: vec![],
                image: Some(ImageManifest {
                    mime: mime.as_str().into(),
                    size: bytes.len() as u64,
                    width: *width,
                    height: *height,
                    hash,
                }),
                text_chars: None,
            };
            send_binary(conn, epoch_key, routing, manifest, bytes).await?;
        }
        ClipboardItem::Files { files } => {
            let mut payload = Vec::new();
            let mut metas = Vec::new();
            for f in files {
                payload.extend_from_slice(&f.bytes);
                metas.push(FileManifest {
                    name: f.name.clone(),
                    size: f.bytes.len() as u64,
                    mime: f.mime.clone(),
                    hash: clipsync_crypto::blake3_hex(&f.bytes),
                });
            }
            let hash = clipsync_crypto::blake3_hex(&payload);
            let manifest = ManifestPlain {
                item_kind: ItemKind::Files,
                total_bytes: payload.len() as u64,
                chunk_count: 0,
                content_hash: hash,
                files: metas,
                image: None,
                text_chars: None,
            };
            send_binary(conn, epoch_key, routing, manifest, &payload).await?;
        }
    }
    Ok(())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn send_binary(
    conn: &RelayConnection,
    epoch_key: &[u8; 32],
    routing: RoutingHeader,
    mut manifest: ManifestPlain,
    payload: &[u8],
) -> Result<(), CoreError> {
    let chunks = split_and_encrypt(epoch_key, &routing, payload)?;
    manifest.chunk_count = chunks.chunk_count;
    manifest.total_bytes = chunks.total_bytes;
    let (nonce, ciphertext) = seal_manifest(epoch_key, &routing, &manifest)?;
    conn.send_control(ControlMessage::Manifest {
        routing: routing.clone(),
        nonce,
        ciphertext,
        chunk_count: chunks.chunk_count,
        total_bytes: chunks.total_bytes,
    })
    .await?;
    for frame in chunks.frames {
        conn.send_binary(frame).await?;
    }
    Ok(())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn open_text(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    nonce_hex: &str,
    ct_hex: &str,
) -> Result<ClipboardItem, CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = decode_nonce(nonce_hex)?;
    let ct = hex::decode(ct_hex).map_err(|e| CoreError::Message(e.to_string()))?;
    let aad = envelope_aad(routing, EnvelopeFrame::Text).map_err(TransferError::from)?;
    let pt = open(&content_key, &nonce, &aad, &ct)?;
    let text = String::from_utf8(pt).map_err(|_| CoreError::Message("text not utf-8".into()))?;
    Ok(ClipboardItem::Text { text })
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn open_manifest(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    nonce_hex: &str,
    ct_hex: &str,
) -> Result<ManifestPlain, CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = decode_nonce(nonce_hex)?;
    let ct = hex::decode(ct_hex).map_err(|e| CoreError::Message(e.to_string()))?;
    let aad = envelope_aad(routing, EnvelopeFrame::Manifest).map_err(TransferError::from)?;
    let pt = open(&content_key, &nonce, &aad, &ct)?;
    Ok(serde_json::from_slice(&pt)?)
}

/// Envelope version and sequence number stamped on one outgoing item.
#[derive(Clone, Copy, Debug)]
struct EnvelopeStamp {
    version: u16,
    seq: u64,
}

impl EnvelopeStamp {
    /// Pick the version `policy` allows for `room`'s peers and reserve a
    /// persisted, strictly increasing `seq`.
    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
    )]
    fn next(
        store: &LocalStore,
        room: &RoomRecord,
        local: &DeviceId,
        policy: EnvelopePolicy,
    ) -> Result<Self, CoreError> {
        Ok(Self {
            version: sending_version(store, room, local, policy)?,
            seq: store.next_send_seq(&room.room_id, sequence_floor(Utc::now()))?,
        })
    }
}

/// Receive-side context: every inbound text envelope and manifest passes
/// through here so addressing, expiry, downgrade and replay checks cannot be
/// skipped by one entry point.
struct Inbound<'a> {
    store: &'a LocalStore,
    room: &'a RoomRecord,
    local: &'a DeviceId,
    epoch_key: &'a [u8; 32],
    policy: EnvelopePolicy,
}

impl Inbound<'_> {
    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
    )]
    fn text(
        &self,
        routing: &RoutingHeader,
        nonce_hex: &str,
        ct_hex: &str,
    ) -> Result<ClipboardItem, CoreError> {
        check_header(
            routing,
            self.room,
            self.local,
            Some(ItemKind::Text),
            Utc::now(),
        )?;
        check_version(self.store, routing, self.policy)?;
        let item = open_text(self.epoch_key, routing, nonce_hex, ct_hex)?;
        commit_sequence(self.store, routing)?;
        Ok(item)
    }

    #[expect(
        clippy::result_large_err,
        reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
    )]
    fn manifest(
        &self,
        routing: &RoutingHeader,
        nonce_hex: &str,
        ct_hex: &str,
    ) -> Result<ManifestPlain, CoreError> {
        check_header(routing, self.room, self.local, None, Utc::now())?;
        check_version(self.store, routing, self.policy)?;
        let manifest = open_manifest(self.epoch_key, routing, nonce_hex, ct_hex)?;
        if manifest.item_kind != routing.item_kind {
            return Err(EnvelopeRejection::WrongItemKind.into());
        }
        commit_sequence(self.store, routing)?;
        Ok(manifest)
    }
}

/// Decrypt a chunk frame only for a transfer whose manifest was accepted,
/// binding it to that manifest's routing header.
fn open_pending_chunk(
    epoch_key: &[u8; 32],
    manifests: &HashMap<String, PendingManifest>,
    buf: &[u8],
) -> Result<(TransferId, u32, u32, Vec<u8>), TransferError> {
    let frame = decode_chunk_frame(buf)?;
    let (routing, _) = manifests
        .get(frame.transfer_id.as_str())
        .ok_or(TransferError::Chunk("no accepted manifest for chunk"))?;
    let plaintext = open_chunk(epoch_key, routing, &frame)?;
    Ok((
        frame.transfer_id,
        frame.chunk_index,
        frame.chunk_count,
        plaintext,
    ))
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn seal_text(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    text: &str,
) -> Result<(String, String), CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = derive_nonce(&content_key, TEXT_NONCE_INDEX)?;
    let aad = envelope_aad(routing, EnvelopeFrame::Text).map_err(TransferError::from)?;
    let ct = seal(&content_key, &nonce, &aad, text.as_bytes())?;
    Ok((hex::encode(nonce), hex::encode(ct)))
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn seal_manifest(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    manifest: &ManifestPlain,
) -> Result<(String, String), CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = derive_nonce(&content_key, MANIFEST_NONCE_INDEX)?;
    let aad = envelope_aad(routing, EnvelopeFrame::Manifest).map_err(TransferError::from)?;
    let ct = seal(&content_key, &nonce, &aad, &serde_json::to_vec(manifest)?)?;
    Ok((hex::encode(nonce), hex::encode(ct)))
}

/// Nonce-derivation index for a text envelope (one message per transfer key).
const TEXT_NONCE_INDEX: u32 = 0;
/// Nonce-derivation index for a manifest; chunk indices count up from zero, so
/// the top index never collides with a chunk of a valid transfer.
const MANIFEST_NONCE_INDEX: u32 = u32::MAX;

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
fn decode_nonce(hex_str: &str) -> Result<[u8; 12], CoreError> {
    let b = hex::decode(hex_str).map_err(|e| CoreError::Message(e.to_string()))?;
    if b.len() != 12 {
        return Err(CoreError::Message("bad nonce".into()));
    }
    let mut n = [0u8; 12];
    n.copy_from_slice(&b);
    Ok(n)
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn item_from_payload(
    manifest: &ManifestPlain,
    payload: Vec<u8>,
    staging: &StagingArea,
    tid: &TransferId,
) -> Result<ClipboardItem, CoreError> {
    match manifest.item_kind {
        ItemKind::Image => {
            let mime = manifest
                .image
                .as_ref()
                .and_then(|i| ImageMime::from_mime(&i.mime))
                .or_else(|| ImageMime::detect(&payload))
                .ok_or_else(|| CoreError::Message("unknown image mime".into()))?;
            Ok(ClipboardItem::Image {
                mime,
                bytes: payload,
                width: manifest.image.as_ref().and_then(|i| i.width),
                height: manifest.image.as_ref().and_then(|i| i.height),
            })
        }
        ItemKind::Files => {
            let paths = staging.commit_files(tid, &manifest.files, &payload).await?;
            let mut files = Vec::new();
            for (meta, path) in manifest.files.iter().zip(paths.iter()) {
                files.push(FileRef {
                    name: meta.name.clone(),
                    bytes: tokio::fs::read(path).await?,
                    mime: meta.mime.clone(),
                    staged_path: Some(path.clone()),
                });
            }
            let _ = paths;
            Ok(ClipboardItem::Files { files })
        }
        ItemKind::Text => {
            let text = String::from_utf8(payload)
                .map_err(|_| CoreError::Message("text payload utf-8".into()))?;
            Ok(ClipboardItem::Text { text })
        }
    }
}

async fn apply_remote(
    clipboard: &SystemClipboard,
    guard: &mut LoopGuard,
    handle: &SyncHandle,
    notifier: &Notifier,
    message_id: &str,
    item: ClipboardItem,
    last_sig: &mut String,
) {
    guard.remember_remote(message_id, &item);
    match clipboard.write(&item).await {
        Ok(()) => {
            if let Ok(sig) = clipboard.signature().await {
                *last_sig = sig;
            }
            notifier.received(&format!("{:?}", item.kind()), "peer", &item.summary());
            *handle.last_item.lock() = Some(item);
        }
        Err(e) => {
            warn!(
                error = %e,
                kind = ?item.kind(),
                "failed to write remote clipboard item"
            );
            *handle.last_item.lock() = Some(item);
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn oneshot_push(store: &LocalStore, item: ClipboardItem) -> Result<(), CoreError> {
    let cfg = store.load_config()?;
    let identity = store.secrets.load_identity()?;
    let room = store.current_room()?;
    let epoch_key = store.load_epoch_key(&room.room_id)?;
    let ws = join_ws(&cfg.relay_url, "/v1/ws");
    let mut conn = RelayConnection::connect(&ws).await?;
    conn.send_control(ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        role: ClientRole::Member,
        room_id: Some(room.room_id.clone()),
        token: room.device_token.clone(),
        pairing_session_id: None,
    })
    .await?;
    wait_hello_ok(&mut conn).await?;
    let stamp = EnvelopeStamp::next(
        store,
        &room,
        &identity.device_id,
        EnvelopePolicy::from_config(&cfg.sync),
    )?;
    send_item(&conn, &identity, &room, &epoch_key, stamp, &item).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    conn.close().await;
    Ok(())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn wait_hello_ok(conn: &mut RelayConnection) -> Result<(), CoreError> {
    wait_hello_ok_msg(conn).await.map(|_| ())
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn wait_hello_ok_msg(conn: &mut RelayConnection) -> Result<ControlMessage, CoreError> {
    let deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Err(CoreError::Timeout),
            msg = conn.incoming.recv() => {
                match msg {
                    Some(Incoming::Control(m @ ControlMessage::HelloOk { .. })) => return Ok(m),
                    Some(Incoming::Control(ControlMessage::Error { code, message })) => {
                        return Err(CoreError::Message(format!("relay {code}: {message}")));
                    }
                    Some(_) => continue,
                    None => return Err(CoreError::Message("relay closed before hello".into())),
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Keep explicit borrows of the existing session state; reviewed in docs/compiler-warnings.md."
)]
async fn apply_hello_ok(
    hello: &ControlMessage,
    identity: &DeviceIdentity,
    room: &RoomRecord,
    inbound: &Inbound<'_>,
    clipboard: &SystemClipboard,
    guard: &mut LoopGuard,
    handle: &SyncHandle,
    notifier: &Notifier,
    last_sig: &mut String,
    handshakes: &mut HandshakeTracker,
    conn: &RelayConnection,
) {
    let ControlMessage::HelloOk {
        peer_online,
        queued_text,
        peers,
        ..
    } = hello
    else {
        return;
    };
    handle.peer_online.store(*peer_online, Ordering::Relaxed);
    if let Some(q) = queued_text {
        if let Ok(item) = inbound.text(&q.routing, &q.nonce, &q.ciphertext) {
            apply_remote(
                clipboard,
                guard,
                handle,
                notifier,
                q.routing.message_id.as_str(),
                item,
                last_sig,
            )
            .await;
        }
    }
    if *peer_online {
        start_handshakes(identity, room, peers, handshakes, conn).await;
    }
}

/// Start a handshake with every online pinned peer this device initiates for
/// and has not authenticated yet in this session.
async fn start_handshakes(
    identity: &DeviceIdentity,
    room: &RoomRecord,
    peers: &[PeerInfo],
    handshakes: &mut HandshakeTracker,
    conn: &RelayConnection,
) {
    for peer in peers.iter().filter(|peer| peer.online) {
        if let Some(message) = handshakes.initiate(identity, room, &peer.device_id, false) {
            send_handshake(conn, message).await;
        }
    }
}

async fn send_handshake(conn: &RelayConnection, message: Vec<u8>) {
    let _ = conn
        .send_control(ControlMessage::NoiseHandshake {
            message: hex::encode(message),
        })
        .await;
}

/// Send the responder reply, if any, and persist an authenticated v2
/// capability advertisement: from then on this device sends v2 to the peer.
async fn finish_handshake(
    store: &LocalStore,
    room: &RoomRecord,
    conn: &RelayConnection,
    outcome: HandshakeOutcome,
) {
    if let Some(reply) = outcome.reply {
        send_handshake(conn, reply).await;
    }
    let Some((peer, capabilities)) = outcome.completed else {
        return;
    };
    info!(peer = %peer, "noise handshake complete");
    if !capabilities.accepts_authenticated() {
        return;
    }
    match store.mark_peer_advertised_authenticated(&room.room_id, &peer) {
        Ok(true) => info!(peer = %peer, "peer accepts authenticated envelopes; sending v2"),
        Ok(false) => {}
        Err(e) => warn!(error = %e, "cannot persist peer envelope capability"),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub async fn oneshot_pull(
    store: &LocalStore,
    copy: bool,
    wait: bool,
    output: Option<PathBuf>,
) -> Result<PullResult, CoreError> {
    let cfg = store.load_config()?;
    let identity = store.secrets.load_identity()?;
    let room = store.current_room()?;
    let epoch_key = store.load_epoch_key(&room.room_id)?;
    let ws = join_ws(&cfg.relay_url, "/v1/ws");
    let mut conn = RelayConnection::connect(&ws).await?;
    conn.send_control(ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: identity.device_id.clone(),
        device_name: device_name(),
        os: current_os().into(),
        role: ClientRole::Member,
        room_id: Some(room.room_id.clone()),
        token: room.device_token.clone(),
        pairing_session_id: None,
    })
    .await?;
    let deadline = if wait {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(3)
    };
    let timeout = tokio::time::sleep(deadline);
    tokio::pin!(timeout);
    let staging = StagingArea::new(store.paths.inbox_dir.clone())?;
    let mut incoming: HashMap<String, IncomingTransfer> = HashMap::new();
    let mut manifests: HashMap<String, PendingManifest> = HashMap::new();
    let inbound = Inbound {
        store,
        room: &room,
        local: &identity.device_id,
        epoch_key: &epoch_key,
        policy: EnvelopePolicy::from_config(&cfg.sync),
    };
    loop {
        tokio::select! {
            _ = &mut timeout => return Err(CoreError::Timeout),
            msg = conn.incoming.recv() => {
                let Some(msg) = msg else { return Err(CoreError::Timeout) };
                match msg {
                    Incoming::Control(ControlMessage::HelloOk { queued_text, .. }) => {
                        if let Some(q) = queued_text {
                            let item = inbound.text(&q.routing, &q.nonce, &q.ciphertext)?;
                            return finish_pull(item, copy, output).await;
                        }
                        if !wait {
                            return Err(CoreError::Message("no queued item".into()));
                        }
                    }
                    Incoming::Control(ControlMessage::TextEnvelope { routing, nonce, ciphertext }) => {
                        let item = inbound.text(&routing, &nonce, &ciphertext)?;
                        return finish_pull(item, copy, output).await;
                    }
                    Incoming::Control(ControlMessage::Manifest { routing, nonce, ciphertext, .. }) => {
                        if let Ok(manifest) = inbound.manifest(&routing, &nonce, &ciphertext) {
                            let id = routing.transfer_id.clone();
                            let t = staging.begin(&id, manifest.chunk_count);
                            incoming.insert(id.0.clone(), t);
                            manifests.insert(id.0, (routing, manifest));
                        }
                    }
                    Incoming::Binary(buf) => {
                        if let Ok((tid, idx, count, pt)) = open_pending_chunk(&epoch_key, &manifests, &buf) {
                            if let Some(t) = incoming.get_mut(tid.as_str()) {
                                if t.push(idx, count, pt).is_err() {
                                    t.fail();
                                    continue;
                                }
                                if t.is_complete() {
                                    if let Some((_, man)) = manifests.remove(tid.as_str()) {
                                        if let Some(xfer) = incoming.remove(tid.as_str()) {
                                            if let Ok(payload) = xfer.assemble(&man.content_hash) {
                                                let item = item_from_payload(&man, payload, &staging, &tid).await?;
                                                return finish_pull(item, copy, output).await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
async fn finish_pull(
    item: ClipboardItem,
    copy: bool,
    output: Option<PathBuf>,
) -> Result<PullResult, CoreError> {
    if copy {
        if let Ok(clip) = SystemClipboard::open() {
            let _ = clip.write(&item).await;
        }
    }
    if let Some(dir) = output {
        tokio::fs::create_dir_all(&dir).await?;
        match &item {
            ClipboardItem::Files { files } => {
                for f in files {
                    tokio::fs::write(dir.join(&f.name), &f.bytes).await?;
                }
            }
            ClipboardItem::Image { mime, bytes, .. } => {
                let name = match mime {
                    ImageMime::Png => "clipboard.png",
                    ImageMime::Jpeg => "clipboard.jpg",
                    ImageMime::Webp => "clipboard.webp",
                };
                tokio::fs::write(dir.join(name), bytes).await?;
            }
            ClipboardItem::Text { text } => {
                tokio::fs::write(dir.join("clipboard.txt"), text.as_bytes()).await?;
            }
        }
    }
    Ok(PullResult {
        kind: format!("{:?}", item.kind()).to_lowercase(),
        text: match &item {
            ClipboardItem::Text { text } => Some(text.clone()),
            _ => None,
        },
        summary: item.summary(),
        files: Vec::new(),
    })
}

pub fn _unused_ws(url: &str) -> String {
    http_to_ws(url)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/support/engine_behavior.rs"]
mod behavior_tests;
