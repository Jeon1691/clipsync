use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::Mutex;
use rand::Rng;
use tokio::sync::mpsc;

use clipsync_protocol::{
    clamp_ttl, CreatePairingRequest, CreatePairingResponse, DeviceRole, JoinPairingRequest,
    JoinPairingResponse, PairingSessionId, PeerInfo, QueuedEnvelope, RoomId, HARD_MAX_TTL_SECS,
    PAIRING_MAX_ATTEMPTS, PAIRING_TTL_SECS,
};

use crate::auth::issue_token;
use crate::{JoinError, RelayConfig, WsOut};

pub struct RelayState {
    pub config: RelayConfig,
    pairing_by_code: DashMap<String, PairingSession>,
    pairing_by_id: DashMap<String, String>,
    rooms: DashMap<String, Room>,
    pub connections: DashMap<String, mpsc::Sender<WsOut>>,
    queue: DashMap<String, (QueuedEnvelope, DateTime<Utc>)>,
    ip_fails: DashMap<IpAddr, (u32, Instant)>,
    pub connections_total: AtomicU64,
    pub frames_forwarded: AtomicU64,
    pub pairing_ok: AtomicU64,
    pub pairing_fail: AtomicU64,
    pub expired: AtomicU64,
}

pub struct PairingSession {
    pub session_id: PairingSessionId,
    pub room_id: RoomId,
    pub code: String,
    pub expires_at: DateTime<Utc>,
    pub attempts: u32,
    pub creator: PeerInfo,
    pub joiner: Option<PeerInfo>,
    pub ttl_secs: u64,
    pub bind_count: u32,
    pub completed: bool,
}

pub struct Room {
    pub room_id: RoomId,
    pub devices: HashMap<String, DeviceEntry>,
    pub epoch: u64,
    pub default_ttl: u64,
}

#[derive(Clone)]
pub struct DeviceEntry {
    pub info: PeerInfo,
    pub revoked: bool,
}

impl RelayState {
    pub fn new(config: RelayConfig) -> Self {
        Self {
            config,
            pairing_by_code: DashMap::new(),
            pairing_by_id: DashMap::new(),
            rooms: DashMap::new(),
            connections: DashMap::new(),
            queue: DashMap::new(),
            ip_fails: DashMap::new(),
            connections_total: AtomicU64::new(0),
            frames_forwarded: AtomicU64::new(0),
            pairing_ok: AtomicU64::new(0),
            pairing_fail: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        }
    }

    pub fn create_pairing(
        &self,
        req: CreatePairingRequest,
    ) -> Result<CreatePairingResponse, String> {
        let ttl = req
            .ttl_secs
            .map(clamp_ttl)
            .unwrap_or(clipsync_protocol::DEFAULT_TTL_SECS);
        if ttl > HARD_MAX_TTL_SECS {
            return Err("ttl exceeds hard max".into());
        }
        let room_id = RoomId::new();
        let code = self.alloc_code()?;
        let expires_at = Utc::now() + chrono::Duration::seconds(PAIRING_TTL_SECS as i64);
        let session = PairingSession {
            session_id: req.pairing_session_id.clone(),
            room_id: room_id.clone(),
            code: code.clone(),
            expires_at,
            attempts: 0,
            creator: PeerInfo {
                device_id: req.device_id.clone(),
                device_name: req.device_name,
                os: req.os,
                role: DeviceRole::Owner,
                online: false,
            },
            joiner: None,
            ttl_secs: ttl,
            bind_count: 0,
            completed: false,
        };
        self.pairing_by_id
            .insert(req.pairing_session_id.0.clone(), code.clone());
        self.pairing_by_code.insert(code.clone(), session);
        let pairing_token = issue_token(
            &self.config.token_secret,
            "pair",
            &room_id,
            &req.device_id,
            DeviceRole::Owner,
            Some(req.pairing_session_id.as_str()),
            PAIRING_TTL_SECS as i64,
        );
        Ok(CreatePairingResponse {
            room_id,
            pairing_code: code,
            pairing_token,
            expires_at,
            ws_path: "/v1/ws".into(),
        })
    }

    fn alloc_code(&self) -> Result<String, String> {
        let mut rng = rand::thread_rng();
        for _ in 0..32 {
            let n: u32 = rng.gen_range(0..1_000_000);
            let code = format!("{n:06}");
            if !self.pairing_by_code.contains_key(&code) {
                return Ok(code);
            }
        }
        Err("could not allocate pairing code".into())
    }

    pub fn join_pairing(
        &self,
        req: JoinPairingRequest,
        ip: IpAddr,
    ) -> Result<JoinPairingResponse, JoinError> {
        self.check_ip(ip)?;
        let mut session = self
            .pairing_by_code
            .get_mut(&req.pairing_code)
            .ok_or(JoinError::NotFound)?;
        if session.expires_at < Utc::now() {
            return Err(JoinError::Expired);
        }
        if session.completed {
            return Err(JoinError::Busy);
        }
        if session.joiner.is_some() {
            return Err(JoinError::Busy);
        }
        session.attempts += 1;
        if session.attempts > PAIRING_MAX_ATTEMPTS {
            return Err(JoinError::Attempts);
        }
        if req.device_id == session.creator.device_id {
            return Err(JoinError::Other("cannot join as the same device".into()));
        }
        let joiner = PeerInfo {
            device_id: req.device_id.clone(),
            device_name: req.device_name,
            os: req.os,
            role: DeviceRole::Member,
            online: false,
        };
        session.joiner = Some(joiner.clone());
        let pairing_token = issue_token(
            &self.config.token_secret,
            "pair",
            &session.room_id,
            &req.device_id,
            DeviceRole::Member,
            Some(session.session_id.as_str()),
            PAIRING_TTL_SECS as i64,
        );
        Ok(JoinPairingResponse {
            pairing_session_id: session.session_id.clone(),
            room_id: session.room_id.clone(),
            pairing_token,
            peer: session.creator.clone(),
            expires_at: session.expires_at,
            ws_path: "/v1/ws".into(),
        })
    }

    fn check_ip(&self, ip: IpAddr) -> Result<(), JoinError> {
        let mut entry = self.ip_fails.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() > Duration::from_secs(3600) {
            *entry = (0, Instant::now());
        }
        if entry.0 >= 30 {
            return Err(JoinError::Attempts);
        }
        Ok(())
    }

    pub fn record_ip_fail(&self, ip: IpAddr) {
        self.ip_fails
            .entry(ip)
            .and_modify(|e| e.0 += 1)
            .or_insert((1, Instant::now()));
        self.pairing_fail.fetch_add(1, Ordering::Relaxed);
    }

    pub fn pairing(
        &self,
        code_or_sid: &str,
    ) -> Option<dashmap::mapref::one::Ref<'_, String, PairingSession>> {
        if let Some(sess) = self.pairing_by_code.get(code_or_sid) {
            return Some(sess);
        }
        let code = self.pairing_by_id.get(code_or_sid)?.clone();
        self.pairing_by_code.get(&code)
    }

    pub fn pairing_mut(
        &self,
        sid: &str,
    ) -> Option<dashmap::mapref::one::RefMut<'_, String, PairingSession>> {
        let code = self.pairing_by_id.get(sid)?.clone();
        self.pairing_by_code.get_mut(&code)
    }

    pub fn complete_pairing(
        &self,
        sid: &str,
    ) -> Option<(String, String, RoomId, PeerInfo, PeerInfo)> {
        let mut session = self.pairing_mut(sid)?;
        if session.completed {
            return None;
        }
        session.bind_count += 1;
        if session.bind_count < 2 {
            return None;
        }
        session.completed = true;
        let joiner = session.joiner.clone()?;
        let creator = session.creator.clone();
        let room_id = session.room_id.clone();
        let owner_token = issue_token(
            &self.config.token_secret,
            "device",
            &room_id,
            &creator.device_id,
            DeviceRole::Owner,
            None,
            86400 * 365,
        );
        let member_token = issue_token(
            &self.config.token_secret,
            "device",
            &room_id,
            &joiner.device_id,
            DeviceRole::Member,
            None,
            86400 * 365,
        );
        let mut devices = HashMap::new();
        devices.insert(
            creator.device_id.as_str().to_string(),
            DeviceEntry {
                info: creator.clone(),
                revoked: false,
            },
        );
        devices.insert(
            joiner.device_id.as_str().to_string(),
            DeviceEntry {
                info: joiner.clone(),
                revoked: false,
            },
        );
        self.rooms.insert(
            room_id.as_str().to_string(),
            Room {
                room_id: room_id.clone(),
                devices,
                epoch: 1,
                default_ttl: session.ttl_secs,
            },
        );
        self.pairing_ok.fetch_add(1, Ordering::Relaxed);
        Some((owner_token, member_token, room_id, creator, joiner))
    }

    pub fn abort_pairing(&self, sid: &str) {
        if let Some(code) = self.pairing_by_id.remove(sid) {
            self.pairing_by_code.remove(&code.1);
        }
        self.pairing_fail.fetch_add(1, Ordering::Relaxed);
    }

    pub fn room(&self, id: &str) -> Option<dashmap::mapref::one::Ref<'_, String, Room>> {
        self.rooms.get(id)
    }

    pub fn room_mut(&self, id: &str) -> Option<dashmap::mapref::one::RefMut<'_, String, Room>> {
        self.rooms.get_mut(id)
    }

    pub fn register_conn(&self, device_id: &str, tx: mpsc::Sender<WsOut>) {
        self.connections.insert(device_id.to_string(), tx);
        self.connections_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn unregister_conn(&self, device_id: &str) {
        self.connections.remove(device_id);
    }

    pub fn peer_ids(&self, room_id: &str, except: &str) -> Vec<String> {
        let Some(room) = self.rooms.get(room_id) else {
            return Vec::new();
        };
        room.devices
            .iter()
            .filter(|(id, d)| id.as_str() != except && !d.revoked)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn is_online(&self, device_id: &str) -> bool {
        self.connections.contains_key(device_id)
    }

    pub fn queue_text(&self, device_id: &str, env: QueuedEnvelope, expires_at: DateTime<Utc>) {
        self.queue.insert(device_id.to_string(), (env, expires_at));
    }

    pub fn take_queue(&self, device_id: &str) -> Option<QueuedEnvelope> {
        let (_, (env, exp)) = self.queue.remove(device_id)?;
        if exp < Utc::now() {
            self.expired.fetch_add(1, Ordering::Relaxed);
            None
        } else {
            Some(env)
        }
    }

    pub fn revoke(&self, room_id: &str, device_id: &str) -> bool {
        let Some(mut room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        if let Some(dev) = room.devices.get_mut(device_id) {
            dev.revoked = true;
            self.connections.remove(device_id);
            room.epoch += 1;
            true
        } else {
            false
        }
    }

    pub fn gc(&self) {
        let now = Utc::now();
        self.pairing_by_code
            .retain(|_, s| s.expires_at > now && !s.completed);
        self.queue.retain(|_, (_, exp)| {
            if *exp > now {
                true
            } else {
                self.expired.fetch_add(1, Ordering::Relaxed);
                false
            }
        });
    }

    pub fn metrics_text(&self) -> String {
        format!(
            "clipsync_ws_connections {}\nclipsync_frames_forwarded {}\nclipsync_pairing_ok {}\nclipsync_pairing_fail {}\nclipsync_expired {}\n",
            self.connections.len(),
            self.frames_forwarded.load(Ordering::Relaxed),
            self.pairing_ok.load(Ordering::Relaxed),
            self.pairing_fail.load(Ordering::Relaxed),
            self.expired.load(Ordering::Relaxed),
        )
    }

    pub fn webhook_spawn(&self, event: serde_json::Value) {
        if let (Some(url), Some(secret)) = (
            self.config.webhook_url.clone(),
            self.config.webhook_secret.clone(),
        ) {
            tokio::spawn(async move {
                let _ = crate::webhook::post_event(&url, &secret, event).await;
            });
        }
    }
}

// silence unused mutex import if any
#[allow(dead_code)]
fn _mutex() -> Mutex<()> {
    Mutex::new(())
}
