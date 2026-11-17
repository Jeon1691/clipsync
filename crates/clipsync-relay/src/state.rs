// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::Rng;
use tokio::sync::mpsc;

use clipsync_protocol::{
    clamp_ttl, CreatePairingRequest, CreatePairingResponse, DeviceRole, JoinPairingRequest,
    JoinPairingResponse, PairingSessionId, PeerInfo, QueuedEnvelope, RoomId, HARD_MAX_TTL_SECS,
    PAIRING_MAX_ATTEMPTS, PAIRING_TTL_SECS,
};

use crate::auth::issue_token;
use crate::client_ip::TrustedProxies;
use crate::connection_limits::{ConnectionCounts, ConnectionRefused, ConnectionSlot};
use crate::join_limits::{PairingJoinLimits, PAIRING_CODE_SPACE};
use crate::rate_limit::Budget;
use crate::resource_limits::{
    RelayResourceLimits, SocketDeadlines, MAX_ACTIVE_PAIRING_SESSIONS, MAX_PAIRING_FIELD_BYTES,
    MAX_QUEUED_TEXT_ENVELOPES,
};
use crate::{CreateError, JoinError, RelayConfig, WsOut};

/// Random draws `alloc_code` makes before giving up. With at most
/// `MAX_ACTIVE_PAIRING_SESSIONS` codes in use, all of them collide with
/// probability at most 1e-96.
const ALLOC_CODE_ATTEMPTS: usize = 32;
/// Lifetime of the device tokens issued when a pairing completes: one year.
/// Tokens are not renewed, so a room is unusable once they have expired.
const DEVICE_TOKEN_TTL_SECS: i64 = 365 * 24 * 60 * 60;
/// Tokens carry whole-second expiry times and stay valid through that second,
/// so a room outlives its tokens by this margin.
const TOKEN_EXPIRY_GRACE_SECS: i64 = 1;

pub struct RelayState {
    pub config: RelayConfig,
    pairing_by_code: DashMap<String, PairingSession>,
    pairing_by_id: DashMap<String, String>,
    rooms: DashMap<String, Room>,
    pub connections: DashMap<String, mpsc::Sender<WsOut>>,
    queue: DashMap<String, (QueuedEnvelope, DateTime<Utc>)>,
    /// Failed pairing joins, per client and relay-wide.
    join_budget: Budget,
    /// Created pairing sessions, per client and relay-wide.
    create_budget: Budget,
    /// Serializes the capacity check with the insert of a new pairing session,
    /// so concurrent creations cannot overshoot the session or room caps.
    pairing_admission: Mutex<()>,
    pub(crate) connection_counts: Arc<ConnectionCounts>,
    trusted_proxies: TrustedProxies,
    join_limits: PairingJoinLimits,
    resource_limits: RelayResourceLimits,
    pub(crate) socket_deadlines: SocketDeadlines,
    pub connections_total: AtomicU64,
    pub frames_forwarded: AtomicU64,
    pub pairing_ok: AtomicU64,
    pub pairing_fail: AtomicU64,
    /// Joins refused because a failed-join budget was spent.
    pub pairing_throttled: AtomicU64,
    /// Pairing creations refused because a creation budget was spent.
    pub pairing_create_throttled: AtomicU64,
    /// Pairing creations refused because the session or room cap was reached.
    pub pairing_create_full: AtomicU64,
    /// WebSocket connections refused by a connection cap.
    pub connections_refused: AtomicU64,
    /// Offline text envelopes not queued because the queue was full.
    pub queue_full: AtomicU64,
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
    // Only authenticated sockets for this session may exchange pairing frames.
    pub(crate) pairing_senders: [Option<mpsc::Sender<WsOut>>; 2],
    pub(crate) pending_spake: [Option<String>; 2],
}

pub struct Room {
    pub room_id: RoomId,
    pub devices: HashMap<String, DeviceEntry>,
    pub epoch: u64,
    pub default_ttl: u64,
    /// When the device tokens issued for this room expire.
    pub tokens_expire_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct DeviceEntry {
    pub info: PeerInfo,
    pub revoked: bool,
}

/// Whether any client-supplied pairing field is longer than the relay stores.
fn oversized_pairing_field(fields: &[&str]) -> bool {
    fields
        .iter()
        .any(|field| field.len() > MAX_PAIRING_FIELD_BYTES)
}

impl RelayState {
    pub fn new(config: RelayConfig) -> Self {
        Self::build(
            config,
            Vec::new(),
            PairingJoinLimits::default(),
            RelayResourceLimits::default(),
        )
    }

    /// Like [`RelayState::new`], also honoring `X-Forwarded-For` from
    /// `trusted_proxies` and applying `join_limits` to failed pairing joins.
    /// Fails if `join_limits` does not pass [`PairingJoinLimits::validate`].
    pub fn with_join_policy(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        join_limits: PairingJoinLimits,
    ) -> anyhow::Result<Self> {
        Self::with_limits(
            config,
            trusted_proxies,
            join_limits,
            RelayResourceLimits::default(),
        )
    }

    /// Like [`RelayState::with_join_policy`], also applying `resource_limits`.
    /// Fails if either set of limits does not validate.
    pub fn with_limits(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        join_limits: PairingJoinLimits,
        resource_limits: RelayResourceLimits,
    ) -> anyhow::Result<Self> {
        join_limits.validate()?;
        resource_limits.validate()?;
        Ok(Self::build(
            config,
            trusted_proxies,
            join_limits,
            resource_limits,
        ))
    }

    fn build(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        join_limits: PairingJoinLimits,
        resource_limits: RelayResourceLimits,
    ) -> Self {
        Self {
            config,
            pairing_by_code: DashMap::new(),
            pairing_by_id: DashMap::new(),
            rooms: DashMap::new(),
            connections: DashMap::new(),
            queue: DashMap::new(),
            join_budget: Budget::default(),
            create_budget: Budget::default(),
            pairing_admission: Mutex::new(()),
            connection_counts: Arc::default(),
            trusted_proxies: TrustedProxies::new(trusted_proxies),
            join_limits,
            resource_limits,
            socket_deadlines: SocketDeadlines::default(),
            connections_total: AtomicU64::new(0),
            frames_forwarded: AtomicU64::new(0),
            pairing_ok: AtomicU64::new(0),
            pairing_fail: AtomicU64::new(0),
            pairing_throttled: AtomicU64::new(0),
            pairing_create_throttled: AtomicU64::new(0),
            pairing_create_full: AtomicU64::new(0),
            connections_refused: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        }
    }

    /// Open a pairing session for a request from client address `ip`. Every
    /// accepted creation counts against the client's and the relay-wide
    /// creation budgets; a spent budget refuses with `Throttled`, and a full
    /// relay (open sessions or rooms at their cap) refuses with `Full`.
    pub fn create_pairing(
        &self,
        req: CreatePairingRequest,
        ip: IpAddr,
    ) -> Result<CreatePairingResponse, CreateError> {
        let ttl = req
            .ttl_secs
            .map(clamp_ttl)
            .unwrap_or(clipsync_protocol::DEFAULT_TTL_SECS);
        if ttl > HARD_MAX_TTL_SECS {
            return Err(CreateError::Invalid("ttl exceeds hard max".into()));
        }
        if oversized_pairing_field(&[
            req.pairing_session_id.as_str(),
            req.device_id.as_str(),
            &req.device_name,
            &req.os,
        ]) {
            return Err(CreateError::Invalid(format!(
                "pairing fields must be at most {MAX_PAIRING_FIELD_BYTES} bytes"
            )));
        }
        let reservation = self
            .create_budget
            .reserve(ip, self.resource_limits.create_budget())
            .ok_or_else(|| {
                self.pairing_create_throttled
                    .fetch_add(1, Ordering::Relaxed);
                CreateError::Throttled
            })?;
        let admission = self
            .pairing_admission
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !self.has_pairing_capacity() {
            // Expired sessions linger until the next GC pass; never let them
            // refuse a creation.
            self.prune_pairings(Utc::now());
        }
        let code = self
            .has_pairing_capacity()
            .then(|| self.alloc_code())
            .flatten();
        let Some(code) = code else {
            drop(admission);
            self.create_budget.release(reservation);
            self.pairing_create_full.fetch_add(1, Ordering::Relaxed);
            return Err(CreateError::Full);
        };
        let room_id = RoomId::new();
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
            pairing_senders: [None, None],
            pending_spake: [None, None],
        };
        self.pairing_by_code.insert(code.clone(), session);
        self.pairing_by_id
            .insert(req.pairing_session_id.0.clone(), code.clone());
        drop(admission);
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

    /// Room for one more pairing session: below the open-session cap, and
    /// every open session can still complete into a room without passing the
    /// room cap. Completed sessions count until GC removes them, which only
    /// errs toward refusing.
    fn has_pairing_capacity(&self) -> bool {
        let sessions = self.pairing_by_code.len();
        sessions < MAX_ACTIVE_PAIRING_SESSIONS
            && self.rooms.len() + sessions < self.resource_limits.max_rooms
    }

    fn alloc_code(&self) -> Option<String> {
        let mut rng = rand::rng();
        for _ in 0..ALLOC_CODE_ATTEMPTS {
            let n = rng.random_range(0..PAIRING_CODE_SPACE);
            let code = format!("{n:06}");
            if !self.pairing_by_code.contains_key(&code) {
                return Some(code);
            }
        }
        None
    }

    /// Client address for per-client limits of a request received from `peer`.
    pub fn client_ip(&self, peer: IpAddr, headers: &axum::http::HeaderMap) -> IpAddr {
        self.trusted_proxies.client_ip(peer, headers)
    }

    /// Join a pairing session by code. Every failed join of a well-formed
    /// request, whatever its cause, counts against the client and the
    /// relay-wide budget; once either is exhausted every join is refused with
    /// `Attempts`, including one with a correct code, so the response never
    /// reveals whether a code exists.
    pub fn join_pairing(
        &self,
        req: JoinPairingRequest,
        ip: IpAddr,
    ) -> Result<JoinPairingResponse, JoinError> {
        // Malformed before any budget: the request never reaches a session.
        if oversized_pairing_field(&[req.device_id.as_str(), &req.device_name, &req.os]) {
            return Err(JoinError::Other(format!(
                "pairing fields must be at most {MAX_PAIRING_FIELD_BYTES} bytes"
            )));
        }
        let reservation = self
            .join_budget
            .reserve(ip, self.join_limits.budget())
            .ok_or_else(|| {
                self.pairing_throttled.fetch_add(1, Ordering::Relaxed);
                JoinError::Attempts
            })?;
        let result = self.join_pairing_session(req);
        if result.is_ok() {
            self.join_budget.release(reservation);
        } else {
            self.pairing_fail.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn join_pairing_session(
        &self,
        req: JoinPairingRequest,
    ) -> Result<JoinPairingResponse, JoinError> {
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

    /// Test helper: charge failed joins to one client without the relay-wide budget.
    #[cfg(test)]
    pub(crate) fn record_ip_fail(&self, ip: IpAddr) {
        self.join_budget
            .charge_client(ip, self.join_limits.client_fail_window());
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
        self.complete_pairing_bound(sid, None)
    }

    pub(crate) fn complete_pairing_bound(
        &self,
        sid: &str,
        expected_room: Option<&str>,
    ) -> Option<(String, String, RoomId, PeerInfo, PeerInfo)> {
        let mut session = self.pairing_mut(sid)?;
        if expected_room.is_some_and(|room| room != session.room_id.as_str()) {
            return None;
        }
        if session.expires_at < Utc::now() {
            session.pending_spake = [None, None];
            return None;
        }
        if session.completed {
            return None;
        }
        session.bind_count += 1;
        if session.bind_count < 2 {
            return None;
        }
        session.completed = true;
        session.pending_spake = [None, None];
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
            DEVICE_TOKEN_TTL_SECS,
        );
        let member_token = issue_token(
            &self.config.token_secret,
            "device",
            &room_id,
            &joiner.device_id,
            DeviceRole::Member,
            None,
            DEVICE_TOKEN_TTL_SECS,
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
                // Read after both tokens were issued, so neither outlives it.
                tokens_expire_at: Utc::now()
                    + chrono::Duration::seconds(DEVICE_TOKEN_TTL_SECS + TOKEN_EXPIRY_GRACE_SECS),
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

    /// Queue `env` as `device_id`'s latest offline text, replacing any earlier
    /// one, until `expires_at` but never past the protocol's hard TTL. Returns
    /// false, queuing nothing, when the queue already holds
    /// `MAX_QUEUED_TEXT_ENVELOPES` other recipients. Concurrent senders may
    /// pass the check together, so the queue can exceed the cap by at most the
    /// number of concurrent connections.
    pub fn queue_text(
        &self,
        device_id: &str,
        env: QueuedEnvelope,
        expires_at: DateTime<Utc>,
    ) -> bool {
        let now = Utc::now();
        let expires_at = expires_at.min(now + chrono::Duration::seconds(HARD_MAX_TTL_SECS as i64));
        if !self.queue.contains_key(device_id) && self.queue.len() >= MAX_QUEUED_TEXT_ENVELOPES {
            self.prune_queue(now);
            if self.queue.len() >= MAX_QUEUED_TEXT_ENVELOPES {
                self.queue_full.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        self.queue.insert(device_id.to_string(), (env, expires_at));
        true
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

    /// Take a connection slot for a WebSocket from client address `ip`, or
    /// refuse when the relay or that client holds its maximum.
    pub(crate) fn acquire_connection(
        &self,
        ip: IpAddr,
    ) -> Result<ConnectionSlot, ConnectionRefused> {
        self.connection_counts
            .acquire(
                ip,
                self.resource_limits.max_connections,
                self.resource_limits.max_client_connections,
            )
            .inspect_err(|_| {
                self.connections_refused.fetch_add(1, Ordering::Relaxed);
            })
    }

    pub fn gc(&self) {
        let now = Utc::now();
        self.prune_pairings(now);
        self.join_budget
            .prune(self.join_limits.client_fail_window());
        self.create_budget
            .prune(self.resource_limits.create_budget().client_window);
        self.prune_rooms(now);
        self.prune_queue(now);
    }

    /// Drop expired and completed pairing sessions from both indexes.
    fn prune_pairings(&self, now: DateTime<Utc>) {
        let mut removed = Vec::new();
        self.pairing_by_code.retain(|code, session| {
            let keep = session.expires_at > now && !session.completed;
            if !keep {
                removed.push((session.session_id.0.clone(), code.clone()));
            }
            keep
        });
        for (sid, code) in removed {
            // A newer session may have reused this ID; leave its mapping.
            self.pairing_by_id
                .remove_if(&sid, |_, mapped| *mapped == code);
        }
    }

    /// Drop rooms nobody can use again: every device revoked, or every device
    /// token expired while no device is still connected. Tokens are only
    /// issued at pairing, so neither kind of room can be re-entered.
    fn prune_rooms(&self, now: DateTime<Utc>) {
        self.rooms.retain(|_, room| {
            let any_active = room.devices.values().any(|device| !device.revoked);
            let tokens_valid = room.tokens_expire_at > now;
            let any_online = room
                .devices
                .keys()
                .any(|device_id| self.connections.contains_key(device_id));
            any_active && (tokens_valid || any_online)
        });
    }

    fn prune_queue(&self, now: DateTime<Utc>) {
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
            "clipsync_ws_connections {}\nclipsync_frames_forwarded {}\nclipsync_pairing_ok {}\nclipsync_pairing_fail {}\nclipsync_pairing_throttled {}\nclipsync_expired {}\nclipsync_ws_sockets {}\nclipsync_ws_refused {}\nclipsync_pairing_sessions {}\nclipsync_pairing_create_throttled {}\nclipsync_pairing_create_full {}\nclipsync_rooms {}\nclipsync_queue_full {}\n",
            self.connections.len(),
            self.frames_forwarded.load(Ordering::Relaxed),
            self.pairing_ok.load(Ordering::Relaxed),
            self.pairing_fail.load(Ordering::Relaxed),
            self.pairing_throttled.load(Ordering::Relaxed),
            self.expired.load(Ordering::Relaxed),
            self.connection_counts.total(),
            self.connections_refused.load(Ordering::Relaxed),
            self.pairing_by_code.len(),
            self.pairing_create_throttled.load(Ordering::Relaxed),
            self.pairing_create_full.load(Ordering::Relaxed),
            self.rooms.len(),
            self.queue_full.load(Ordering::Relaxed),
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "state_tests.rs"]
mod tests;
