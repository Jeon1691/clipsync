// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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
use crate::{JoinError, RelayConfig, WsOut};

// Failed pairing joins are limited per client and relay-wide. The relay-wide
// budget is what bounds guessing: with `PAIRING_TTL_SECS`-long fixed windows,
// a pairing code's lifetime overlaps at most two windows, so at most
// 2 * GLOBAL_FAILED_JOIN_LIMIT = 40 wrong codes are evaluated while any code
// is live, however many addresses the attacker uses. Codes are uniform over
// 10^6 values, so one session is guessed with probability <= 40 / 10^6 =
// 4e-5 (4e-5 * S with S sessions open at once). This is below the 1e-4
// implied by NIST SP 800-63B's ceiling of 100 failed attempts against a
// 6-digit authenticator.

/// Failed joins one client may make per window: enough to retype a mistyped
/// code a few times, while one address alone tests at most 10 of 10^6 codes.
pub(crate) const IP_FAILED_JOIN_LIMIT: u32 = 10;
/// Per-client window; longer than a pairing session so a code cannot outlive
/// one client's budget, short enough that a mistyping user is not locked out
/// for long.
const IP_FAILED_JOIN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Relay-wide failed joins per window; see the bound above. A self-hosted relay
/// rarely sees more than a handful of mistyped codes per pairing window.
const GLOBAL_FAILED_JOIN_LIMIT: u32 = 20;
/// One pairing TTL, so the per-session bound above holds if the TTL changes.
const GLOBAL_FAILED_JOIN_WINDOW: Duration = Duration::from_secs(PAIRING_TTL_SECS);
/// IPv6 sites are normally delegated at least a /64, so one client controls
/// every address in it; count them as one client.
const IPV6_CLIENT_PREFIX_LEN: u32 = 64;
/// Values a pairing code can take: six decimal digits (see `alloc_code`).
const PAIRING_CODE_SPACE: u64 = 1_000_000;
/// Largest acceptable guessing success per session, as 1 in this many (1e-4).
const NEGLIGIBLE_GUESS_ODDS: u64 = 10_000;
/// Fixed windows per pairing lifetime in the worst case (see the bound above).
const WINDOWS_PER_PAIRING_LIFETIME: u64 = 2;

// Keep the bound above true if any of these values change.
const _: () = {
    assert!(GLOBAL_FAILED_JOIN_WINDOW.as_secs() >= PAIRING_TTL_SECS);
    assert!(IP_FAILED_JOIN_WINDOW.as_secs() >= PAIRING_TTL_SECS);
    assert!(IP_FAILED_JOIN_LIMIT < GLOBAL_FAILED_JOIN_LIMIT);
    assert!(
        WINDOWS_PER_PAIRING_LIFETIME * GLOBAL_FAILED_JOIN_LIMIT as u64 * NEGLIGIBLE_GUESS_ODDS
            <= PAIRING_CODE_SPACE
    );
};

pub struct RelayState {
    pub config: RelayConfig,
    pairing_by_code: DashMap<String, PairingSession>,
    pairing_by_id: DashMap<String, String>,
    rooms: DashMap<String, Room>,
    pub connections: DashMap<String, mpsc::Sender<WsOut>>,
    queue: DashMap<String, (QueuedEnvelope, DateTime<Utc>)>,
    /// Failed joins per client key (see `rate_limit_key`): count and window start.
    ip_fails: DashMap<IpAddr, (u32, Instant)>,
    /// Relay-wide failed joins: count and window start.
    global_join_fails: Mutex<(u32, Instant)>,
    trusted_proxies: TrustedProxies,
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
    // Only authenticated sockets for this session may exchange pairing frames.
    pub(crate) pairing_senders: [Option<mpsc::Sender<WsOut>>; 2],
    pub(crate) pending_spake: [Option<String>; 2],
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

/// Join attempt counted before the code lookup and refunded if the join succeeds.
struct JoinReservation {
    key: IpAddr,
    ip_window: Instant,
    global_window: Instant,
}

/// Rate-limit key for a client address: IPv4 as is, IPv6 by its /64 prefix.
fn rate_limit_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let mask = u128::MAX << (u128::BITS - IPV6_CLIENT_PREFIX_LEN);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
        v4 => v4,
    }
}

impl RelayState {
    pub fn new(config: RelayConfig) -> Self {
        Self::with_trusted_proxies(config, Vec::new())
    }

    /// Like [`RelayState::new`], also honoring `X-Forwarded-For` from `trusted_proxies`.
    pub fn with_trusted_proxies(config: RelayConfig, trusted_proxies: Vec<ipnet::IpNet>) -> Self {
        Self {
            config,
            pairing_by_code: DashMap::new(),
            pairing_by_id: DashMap::new(),
            rooms: DashMap::new(),
            connections: DashMap::new(),
            queue: DashMap::new(),
            ip_fails: DashMap::new(),
            global_join_fails: Mutex::new((0, Instant::now())),
            trusted_proxies: TrustedProxies::new(trusted_proxies),
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
            pairing_senders: [None, None],
            pending_spake: [None, None],
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
        let mut rng = rand::rng();
        for _ in 0..32 {
            let n: u32 = rng.random_range(0..1_000_000);
            let code = format!("{n:06}");
            if !self.pairing_by_code.contains_key(&code) {
                return Ok(code);
            }
        }
        Err("could not allocate pairing code".into())
    }

    /// Client address for per-client limits of a request received from `peer`.
    pub fn client_ip(&self, peer: IpAddr, headers: &axum::http::HeaderMap) -> IpAddr {
        self.trusted_proxies.client_ip(peer, headers)
    }

    /// Join a pairing session by code. Every failed join, whatever its cause,
    /// counts against the client and the relay-wide budget; once either is
    /// exhausted every join is refused with `Attempts`, including one with a
    /// correct code, so the response never reveals whether a code exists.
    pub fn join_pairing(
        &self,
        req: JoinPairingRequest,
        ip: IpAddr,
    ) -> Result<JoinPairingResponse, JoinError> {
        let reservation = self.reserve_join_attempt(rate_limit_key(ip))?;
        let result = self.join_pairing_session(req);
        if result.is_ok() {
            self.release_join_attempt(reservation);
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

    /// Count a join attempt before the code is looked up, so concurrent guesses
    /// cannot overshoot either budget. Refused attempts are not counted.
    fn reserve_join_attempt(&self, key: IpAddr) -> Result<JoinReservation, JoinError> {
        let ip_window = {
            let mut entry = self
                .ip_fails
                .entry(key)
                .or_insert_with(|| (0, Instant::now()));
            // Read the clock while holding the entry so a reset never
            // back-dates a window past a concurrent update.
            let now = Instant::now();
            if now.duration_since(entry.1) > IP_FAILED_JOIN_WINDOW {
                *entry = (0, now);
            }
            if entry.0 >= IP_FAILED_JOIN_LIMIT {
                return Err(JoinError::Attempts);
            }
            entry.0 += 1;
            entry.1
        };
        let mut global = self
            .global_join_fails
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if now.duration_since(global.1) > GLOBAL_FAILED_JOIN_WINDOW {
            *global = (0, now);
        }
        if global.0 >= GLOBAL_FAILED_JOIN_LIMIT {
            drop(global);
            self.release_ip_attempt(key, ip_window);
            return Err(JoinError::Attempts);
        }
        global.0 += 1;
        Ok(JoinReservation {
            key,
            ip_window,
            global_window: global.1,
        })
    }

    /// Refund a successful join; a refund for an already-reset window is void.
    fn release_join_attempt(&self, reservation: JoinReservation) {
        self.release_ip_attempt(reservation.key, reservation.ip_window);
        let mut global = self
            .global_join_fails
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if global.1 == reservation.global_window {
            global.0 = global.0.saturating_sub(1);
        }
    }

    fn release_ip_attempt(&self, key: IpAddr, window: Instant) {
        self.ip_fails.remove_if_mut(&key, |_, entry| {
            if entry.1 == window {
                entry.0 = entry.0.saturating_sub(1);
            }
            entry.0 == 0
        });
    }

    /// Test helper: charge failed joins to one client without the relay-wide budget.
    #[cfg(test)]
    pub(crate) fn record_ip_fail(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut entry = self.ip_fails.entry(rate_limit_key(ip)).or_insert((0, now));
        if now.duration_since(entry.1) > IP_FAILED_JOIN_WINDOW {
            *entry = (0, now);
        }
        entry.0 = entry.0.saturating_add(1);
        drop(entry);
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
        self.ip_fails
            .retain(|_, (_, window)| window.elapsed() <= IP_FAILED_JOIN_WINDOW);
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "state_tests.rs"]
mod tests;
