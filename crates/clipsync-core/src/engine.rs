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
use clipsync_crypto::{
    derive_content_key, derive_nonce, open, parse_public, seal, DeviceIdentity, NoiseHandshake,
};
use clipsync_daemon::{
    read_request, write_pid, write_response, IpcRequest, IpcResponse, IpcServer, PullPayload,
    StatusPayload,
};
use clipsync_protocol::{
    AckStatus, ClientRole, ControlMessage, FileManifest, ImageManifest, ItemKind, ManifestPlain,
    MessageId, Recipient, RoutingHeader, TransferId, PROTOCOL_VERSION,
};
use clipsync_storage::{LocalStore, RoomRecord};
use clipsync_transfer::{decrypt_chunk, split_and_encrypt, IncomingTransfer, StagingArea};
use clipsync_transport::{backoff_delay, http_to_ws, join_ws, Incoming, RelayConnection};

use crate::loop_guard::LoopGuard;
use crate::notify::Notifier;
use crate::{current_os, device_name, CoreError, PullResult};

pub struct SyncHandle {
    pub paused: Arc<AtomicBool>,
    pub connected: Arc<AtomicBool>,
    pub peer_online: Arc<AtomicBool>,
    pub last_item: Arc<Mutex<Option<ClipboardItem>>>,
}

pub async fn run_daemon(clipboard: Option<SystemClipboard>) -> Result<(), CoreError> {
    let store = LocalStore::open()?;
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
                                if matches!(req_is_shutdown(&resp), true) {
                                    // ignore
                                }
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

fn req_is_shutdown(_resp: &IpcResponse) -> bool {
    false
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
        match run_session(&store, &clipboard, &handle, &mut shutdown).await {
            Ok(()) => attempt = 0,
            Err(e) => {
                warn!(error = %e, "sync session ended");
                handle.connected.store(false, Ordering::Relaxed);
                attempt += 1;
                let delay = backoff_delay(attempt);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.changed() => break,
                }
            }
        }
    }
}

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

    let mut guard = LoopGuard::default();
    let notifier = Notifier::new(cfg.notify.clone());
    let staging = StagingArea::new(store.paths.inbox_dir.clone())?;
    let mut last_sig = clipboard.signature().await.unwrap_or_else(|_| "0".into());
    let mut incoming: HashMap<String, IncomingTransfer> = HashMap::new();
    let mut manifests: HashMap<String, ManifestPlain> = HashMap::new();
    let mut seq = 0u64;
    let mut noise: Option<NoiseHandshake> = None;
    let mut noise_done = false;
    handle.connected.store(true, Ordering::Relaxed);

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
                        seq += 1;
                        if let Err(e) = send_item(&conn, &identity, &room, &epoch_key, seq, &item).await {
                            warn!(error = %e, "send failed");
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
                    Incoming::Control(ControlMessage::HelloOk { peer_online, queued_text, peers, .. }) => {
                        handle.peer_online.store(peer_online, Ordering::Relaxed);
                        if let Some(q) = queued_text {
                            if let Ok(item) = open_text(&epoch_key, &q.routing, &q.nonce, &q.ciphertext) {
                                apply_remote(clipboard, &mut guard, &handle, &notifier, &q.routing.message_id.0, item, &mut last_sig).await;
                            }
                        }
                        if peer_online && !noise_done {
                            if let Some(peer) = peers.iter().find(|p| p.online) {
                                if let Some(rec) = room.peers.iter().find(|p| p.device_id == peer.device_id) {
                                    if let Some(pk) = parse_public(&rec.public_key_hex) {
                                        let initiator = identity.device_id.as_str() < rec.device_id.as_str();
                                        if initiator {
                                            match NoiseHandshake::initiator(&identity.static_secret(), &pk) {
                                                Ok(mut hs) => {
                                                    if let Ok(m) = hs.write_message() {
                                                        let _ = conn.send_control(ControlMessage::NoiseHandshake {
                                                            message: hex::encode(m),
                                                        }).await;
                                                    }
                                                    noise = Some(hs);
                                                }
                                                Err(e) => warn!(error = %e, "noise init"),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Incoming::Control(ControlMessage::PeerJoined { .. }) => {
                        handle.peer_online.store(true, Ordering::Relaxed);
                    }
                    Incoming::Control(ControlMessage::PeerLeft { .. }) => {
                        handle.peer_online.store(false, Ordering::Relaxed);
                    }
                    Incoming::Control(ControlMessage::NoiseHandshake { message }) => {
                        let bytes = hex::decode(&message).unwrap_or_default();
                        if let Some(peer) = room.peers.first() {
                            if let Some(pk) = parse_public(&peer.public_key_hex) {
                                if noise.is_none() {
                                    if let Ok(hs) = NoiseHandshake::responder(&identity.static_secret(), &pk) {
                                        noise = Some(hs);
                                    }
                                }
                                if let Some(hs) = noise.as_mut() {
                                    if hs.read_message(&bytes).is_ok() && !hs.is_finished() {
                                        if let Ok(m) = hs.write_message() {
                                            let _ = conn.send_control(ControlMessage::NoiseHandshake {
                                                message: hex::encode(m),
                                            }).await;
                                        }
                                    }
                                    if hs.is_finished() {
                                        noise_done = true;
                                        info!("noise handshake complete");
                                    }
                                }
                            }
                        }
                    }
                    Incoming::Control(ControlMessage::TextEnvelope { routing, nonce, ciphertext }) => {
                        if guard.seen_message(routing.message_id.as_str()) {
                            continue;
                        }
                        match open_text(&epoch_key, &routing, &nonce, &ciphertext) {
                            Ok(item) => {
                                apply_remote(clipboard, &mut guard, handle, &notifier, routing.message_id.as_str(), item, &mut last_sig).await;
                                let _ = conn.send_control(ControlMessage::Ack {
                                    message_id: routing.message_id,
                                    transfer_id: Some(routing.transfer_id),
                                    status: AckStatus::Delivered,
                                    detail: None,
                                }).await;
                            }
                            Err(e) => warn!(error = %e, "text decrypt"),
                        }
                    }
                    Incoming::Control(ControlMessage::Manifest { routing, nonce, ciphertext, .. }) => {
                        match open_manifest(&epoch_key, &routing, &nonce, &ciphertext) {
                            Ok(manifest) => {
                                let id = routing.transfer_id.clone();
                                let t = staging.begin(&id, manifest.chunk_count);
                                incoming.insert(id.0.clone(), t);
                                manifests.insert(id.0, manifest);
                            }
                            Err(e) => warn!(error = %e, "manifest"),
                        }
                    }
                    Incoming::Control(ControlMessage::Ack { status, .. }) => {
                        debug!(?status, "ack");
                    }
                    Incoming::Control(ControlMessage::Error { code, message }) => {
                        warn!(code, message, "relay error");
                    }
                    Incoming::Binary(buf) => {
                        match decrypt_chunk(&epoch_key, &buf) {
                            Ok((tid, idx, count, pt)) => {
                                if let Some(t) = incoming.get_mut(tid.as_str()) {
                                    if t.push(idx, count, pt).is_err() {
                                        t.fail();
                                        continue;
                                    }
                                    if t.is_complete() {
                                        if let Some(man) = manifests.remove(tid.as_str()) {
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

async fn send_item(
    conn: &RelayConnection,
    identity: &DeviceIdentity,
    room: &RoomRecord,
    epoch_key: &[u8; 32],
    seq: u64,
    item: &ClipboardItem,
) -> Result<(), CoreError> {
    let transfer_id = TransferId::new();
    let message_id = MessageId::new();
    let now = Utc::now();
    let ttl = ChronoDuration::seconds(clipsync_protocol::DEFAULT_TTL_SECS as i64);
    let routing = RoutingHeader {
        version: PROTOCOL_VERSION,
        message_id,
        transfer_id: transfer_id.clone(),
        room_id: room.room_id.clone(),
        sender_device_id: identity.device_id.clone(),
        recipient: Recipient::Broadcast,
        sent_at: now,
        expires_at: now + ttl,
        item_kind: item.kind(),
        seq,
    };
    match item {
        ClipboardItem::Text { text } => {
            let content_key = derive_content_key(epoch_key, transfer_id.as_str())?;
            let nonce = derive_nonce(&content_key, 0)?;
            let aad = format!("{}:text", routing.message_id);
            let ct = seal(&content_key, &nonce, aad.as_bytes(), text.as_bytes())?;
            conn.send_control(ControlMessage::TextEnvelope {
                routing,
                nonce: hex::encode(nonce),
                ciphertext: hex::encode(ct),
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

async fn send_binary(
    conn: &RelayConnection,
    epoch_key: &[u8; 32],
    routing: RoutingHeader,
    mut manifest: ManifestPlain,
    payload: &[u8],
) -> Result<(), CoreError> {
    let chunks = split_and_encrypt(epoch_key, &routing.transfer_id, payload)?;
    manifest.chunk_count = chunks.chunk_count;
    manifest.total_bytes = chunks.total_bytes;
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = derive_nonce(&content_key, u32::MAX)?;
    let aad = format!("{}:manifest", routing.transfer_id);
    let pt = serde_json::to_vec(&manifest)?;
    let ct = seal(&content_key, &nonce, aad.as_bytes(), &pt)?;
    conn.send_control(ControlMessage::Manifest {
        routing: routing.clone(),
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ct),
        chunk_count: chunks.chunk_count,
        total_bytes: chunks.total_bytes,
    })
    .await?;
    for frame in chunks.frames {
        conn.send_binary(frame).await?;
    }
    Ok(())
}

fn open_text(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    nonce_hex: &str,
    ct_hex: &str,
) -> Result<ClipboardItem, CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = decode_nonce(nonce_hex)?;
    let ct = hex::decode(ct_hex).map_err(|e| CoreError::Message(e.to_string()))?;
    let aad = format!("{}:text", routing.message_id);
    let pt = open(&content_key, &nonce, aad.as_bytes(), &ct)?;
    let text = String::from_utf8(pt).map_err(|_| CoreError::Message("text not utf-8".into()))?;
    Ok(ClipboardItem::Text { text })
}

fn open_manifest(
    epoch_key: &[u8; 32],
    routing: &RoutingHeader,
    nonce_hex: &str,
    ct_hex: &str,
) -> Result<ManifestPlain, CoreError> {
    let content_key = derive_content_key(epoch_key, routing.transfer_id.as_str())?;
    let nonce = decode_nonce(nonce_hex)?;
    let ct = hex::decode(ct_hex).map_err(|e| CoreError::Message(e.to_string()))?;
    let aad = format!("{}:manifest", routing.transfer_id);
    let pt = open(&content_key, &nonce, aad.as_bytes(), &ct)?;
    Ok(serde_json::from_slice(&pt)?)
}

fn decode_nonce(hex_str: &str) -> Result<[u8; 12], CoreError> {
    let b = hex::decode(hex_str).map_err(|e| CoreError::Message(e.to_string()))?;
    if b.len() != 12 {
        return Err(CoreError::Message("bad nonce".into()));
    }
    let mut n = [0u8; 12];
    n.copy_from_slice(&b);
    Ok(n)
}

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
    if matches!(
        // receive-only/send-only checked by caller via config in send path; apply always writes
        item.kind(),
        _
    ) {
        guard.remember_remote(message_id, &item);
        if clipboard.write(&item).await.is_ok() {
            if let Ok(sig) = clipboard.signature().await {
                *last_sig = sig;
            }
            notifier.received(&format!("{:?}", item.kind()), "peer", &item.summary());
            *handle.last_item.lock() = Some(item);
        }
    }
}

pub async fn oneshot_push(store: &LocalStore, item: ClipboardItem) -> Result<(), CoreError> {
    let cfg = store.load_config()?;
    let identity = store.secrets.load_identity()?;
    let room = store.current_room()?;
    let epoch_key = store.load_epoch_key(&room.room_id)?;
    let ws = join_ws(&cfg.relay_url, "/v1/ws");
    let conn = RelayConnection::connect(&ws).await?;
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
    send_item(&conn, &identity, &room, &epoch_key, 1, &item).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    conn.close().await;
    Ok(())
}

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
    loop {
        tokio::select! {
            _ = &mut timeout => return Err(CoreError::Timeout),
            msg = conn.incoming.recv() => {
                let Some(msg) = msg else { return Err(CoreError::Timeout) };
                match msg {
                    Incoming::Control(ControlMessage::HelloOk { queued_text, .. }) => {
                        if let Some(q) = queued_text {
                            let item = open_text(&epoch_key, &q.routing, &q.nonce, &q.ciphertext)?;
                            return finish_pull(item, copy, output).await;
                        }
                        if !wait {
                            return Err(CoreError::Message("no queued item".into()));
                        }
                    }
                    Incoming::Control(ControlMessage::TextEnvelope { routing, nonce, ciphertext }) => {
                        let item = open_text(&epoch_key, &routing, &nonce, &ciphertext)?;
                        return finish_pull(item, copy, output).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

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
    if let (Some(dir), ClipboardItem::Files { files }) = (output, &item) {
        tokio::fs::create_dir_all(&dir).await?;
        for f in files {
            tokio::fs::write(dir.join(&f.name), &f.bytes).await?;
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
