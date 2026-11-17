// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use rand::Rng;
use tokio::sync::{mpsc, Notify};

use clipsync_protocol::{
    clamp_ttl, CreatePairingRequest, CreatePairingResponse, DeviceRole, JoinPairingRequest,
    JoinPairingResponse, PairingSessionId, PeerInfo, QueuedEnvelope, RoomId, HARD_MAX_TTL_SECS,
    PAIRING_MAX_ATTEMPTS, PAIRING_TTL_SECS, QUEUE_TTL_SECS,
};

use crate::auth::issue_token;
use crate::client_ip::TrustedProxies;
use crate::join_limits::{PairingJoinLimits, GLOBAL_FAILED_JOIN_WINDOW};
use crate::{JoinError, RelayConfig, WsOut};

/// IPv6 sites are normally delegated at least a /64, so one client controls
/// every address in it; count them as one client.
const IPV6_CLIENT_PREFIX_LEN: u32 = 64;

/// Identifies one room-member registration, so a socket's cleanup can retire
/// only the route it installed and never a newer socket's replacement.
pub type ConnectionId = u64;

/// Device IDs are client-chosen and public (presence frames, webhooks), and
/// one device may belong to several rooms, so member state is keyed per room.
type MemberKey = (String, String);

fn member_key(room_id: &str, device_id: &str) -> MemberKey {
    (room_id.to_string(), device_id.to_string())
}

/// Most sockets one room member may have routed at once. A device runs its
/// sync daemon and, alongside it, CLI commands that open their own socket
/// (`push` of an image or file, `pull` without a daemon item, possibly both),
/// and a daemon reconnect overlaps the old socket until that one closes:
/// daemon, reconnect overlap, push and pull make four. Registering one more
/// evicts the member's oldest socket, so one device token cannot pin an
/// unbounded number of routed sockets and their outbound queues.
pub const MAX_MEMBER_SOCKETS: usize = 4;

pub(crate) struct MemberRoute {
    pub(crate) id: ConnectionId,
    pub(crate) sender: mpsc::Sender<WsOut>,
    evicted: Arc<Notify>,
}

impl MemberRoute {
    /// Tells the socket that installed this route to close. `notify_one`
    /// keeps the signal until the socket next waits, so it cannot be missed.
    fn evict(self) {
        self.evicted.notify_one();
    }
}

/// One socket's room-member registration. `evicted` fires once the relay
/// drops this registration (revocation, the per-member socket cap, or a
/// stalled outbound queue); the socket must then stop and close, because it
/// is no longer routed.
pub struct MemberRegistration {
    pub id: ConnectionId,
    pub evicted: Arc<Notify>,
}

/// Counts one authenticated socket, pairing or room member, until dropped.
pub(crate) struct LiveConnection(Arc<AtomicU64>);

impl Drop for LiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct RelayState {
    pub config: RelayConfig,
    pairing_by_code: DashMap<String, PairingSession>,
    pairing_by_id: DashMap<String, String>,
    rooms: DashMap<String, Room>,
    // Only sockets authenticated with a device token for a non-revoked room
    // member are routed here. Pairing sockets route via `pairing_senders`.
    // Each member's registrations are kept oldest first, never empty, and at
    // most `MAX_MEMBER_SOCKETS` long.
    pub(crate) member_routes: DashMap<MemberKey, Vec<MemberRoute>>,
    next_connection_id: AtomicU64,
    live_connections: Arc<AtomicU64>,
    // How long a socket may send nothing before the relay closes it
    // (`ws::READ_IDLE_TIMEOUT`); a field only so tests can shorten it.
    pub(crate) read_idle_timeout: Duration,
    queue: DashMap<MemberKey, (QueuedEnvelope, DateTime<Utc>)>,
    /// Failed joins per client key (see `rate_limit_key`): count and window start.
    ip_fails: DashMap<IpAddr, (u32, Instant)>,
    /// Relay-wide failed joins: count and window start.
    global_join_fails: Mutex<(u32, Instant)>,
    trusted_proxies: TrustedProxies,
    join_limits: PairingJoinLimits,
    pub connections_total: AtomicU64,
    pub frames_forwarded: AtomicU64,
    pub pairing_ok: AtomicU64,
    pub pairing_fail: AtomicU64,
    /// Joins refused because a failed-join budget was spent.
    pub pairing_throttled: AtomicU64,
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
        Self::build(config, Vec::new(), PairingJoinLimits::default())
    }

    /// Like [`RelayState::new`], also honoring `X-Forwarded-For` from
    /// `trusted_proxies` and applying `join_limits` to failed pairing joins.
    /// Fails if `join_limits` does not pass [`PairingJoinLimits::validate`].
    pub fn with_join_policy(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        join_limits: PairingJoinLimits,
    ) -> anyhow::Result<Self> {
        join_limits.validate()?;
        Ok(Self::build(config, trusted_proxies, join_limits))
    }

    fn build(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        join_limits: PairingJoinLimits,
    ) -> Self {
        Self {
            config,
            pairing_by_code: DashMap::new(),
            pairing_by_id: DashMap::new(),
            rooms: DashMap::new(),
            member_routes: DashMap::new(),
            next_connection_id: AtomicU64::new(0),
            live_connections: Arc::new(AtomicU64::new(0)),
            read_idle_timeout: crate::ws::READ_IDLE_TIMEOUT,
            queue: DashMap::new(),
            ip_fails: DashMap::new(),
            global_join_fails: Mutex::new((0, Instant::now())),
            trusted_proxies: TrustedProxies::new(trusted_proxies),
            join_limits,
            connections_total: AtomicU64::new(0),
            frames_forwarded: AtomicU64::new(0),
            pairing_ok: AtomicU64::new(0),
            pairing_fail: AtomicU64::new(0),
            pairing_throttled: AtomicU64::new(0),
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
        let reservation = self
            .reserve_join_attempt(rate_limit_key(ip))
            .inspect_err(|_| {
                self.pairing_throttled.fetch_add(1, Ordering::Relaxed);
            })?;
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
            if now.duration_since(entry.1) > self.join_limits.client_fail_window() {
                *entry = (0, now);
            }
            if entry.0 >= self.join_limits.client_fail_limit {
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
        if global.0 >= self.join_limits.relay_fail_limit {
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
        if now.duration_since(entry.1) > self.join_limits.client_fail_window() {
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

    pub(crate) fn live_connection(&self) -> LiveConnection {
        self.live_connections.fetch_add(1, Ordering::Relaxed);
        LiveConnection(self.live_connections.clone())
    }

    /// Adds `tx` to the sockets that receive room traffic for `device_id` in
    /// `room_id`. A member may hold several sockets at once (daemon plus CLI
    /// one-shot commands); past `MAX_MEMBER_SOCKETS` the oldest is evicted.
    /// Only non-revoked members qualify.
    pub fn register_conn(
        &self,
        room_id: &str,
        device_id: &str,
        tx: mpsc::Sender<WsOut>,
    ) -> Result<MemberRegistration, &'static str> {
        // Holding the room entry orders this insert with `revoke`, which
        // removes the member's routes under the same entry's write lock.
        let room = self.rooms.get(room_id).ok_or("unknown room")?;
        let device = room.devices.get(device_id).ok_or("unknown device")?;
        if device.revoked {
            return Err("device revoked");
        }
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        let evicted = Arc::new(Notify::new());
        let overflow: Vec<MemberRoute> = {
            let mut routes = self
                .member_routes
                .entry(member_key(room_id, device_id))
                .or_default();
            routes.push(MemberRoute {
                id,
                sender: tx,
                evicted: evicted.clone(),
            });
            let excess = routes.len().saturating_sub(MAX_MEMBER_SOCKETS);
            routes.drain(..excess).collect()
        };
        drop(room);
        for route in overflow {
            route.evict();
        }
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        Ok(MemberRegistration { id, evicted })
    }

    /// Whether registration `id` is still routed for the member. Only routed
    /// sockets may send room traffic; an evicted or revoked one may not.
    pub fn owns_route(&self, room_id: &str, device_id: &str, id: ConnectionId) -> bool {
        self.member_routes
            .get(&member_key(room_id, device_id))
            .is_some_and(|routes| routes.iter().any(|route| route.id == id))
    }

    /// Removes registration `id` from the member's routes, leaving the
    /// member's other sockets in place.
    fn remove_registration(
        &self,
        room_id: &str,
        device_id: &str,
        id: ConnectionId,
    ) -> Option<MemberRoute> {
        let Entry::Occupied(mut entry) = self.member_routes.entry(member_key(room_id, device_id))
        else {
            return None;
        };
        let routes = entry.get_mut();
        let position = routes.iter().position(|route| route.id == id)?;
        let removed = routes.remove(position);
        if routes.is_empty() {
            entry.remove();
        }
        Some(removed)
    }

    /// Removes only registration `id`; returns whether it was still routed.
    pub fn unregister_conn(&self, room_id: &str, device_id: &str, id: ConnectionId) -> bool {
        self.remove_registration(room_id, device_id, id).is_some()
    }

    /// Removes registration `id` and tells its socket to close.
    pub(crate) fn evict_conn(&self, room_id: &str, device_id: &str, id: ConnectionId) -> bool {
        self.remove_registration(room_id, device_id, id)
            .map(MemberRoute::evict)
            .is_some()
    }

    /// Every routed socket of the member, oldest first.
    pub(crate) fn member_senders(
        &self,
        room_id: &str,
        device_id: &str,
    ) -> Vec<(ConnectionId, mpsc::Sender<WsOut>)> {
        self.member_routes
            .get(&member_key(room_id, device_id))
            .map(|routes| {
                routes
                    .iter()
                    .map(|route| (route.id, route.sender.clone()))
                    .collect()
            })
            .unwrap_or_default()
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

    pub fn is_online(&self, room_id: &str, device_id: &str) -> bool {
        self.member_routes
            .get(&member_key(room_id, device_id))
            .is_some_and(|routes| !routes.is_empty())
    }

    /// Keeps the latest text for an offline member until the sender's expiry,
    /// but never longer than `QUEUE_TTL_SECS`: the expiry is sender-chosen.
    pub fn queue_text(
        &self,
        room_id: &str,
        device_id: &str,
        env: QueuedEnvelope,
        expires_at: DateTime<Utc>,
    ) {
        let retention_limit = Utc::now() + chrono::Duration::seconds(QUEUE_TTL_SECS as i64);
        self.queue.insert(
            member_key(room_id, device_id),
            (env, expires_at.min(retention_limit)),
        );
    }

    pub fn take_queue(&self, room_id: &str, device_id: &str) -> Option<QueuedEnvelope> {
        let (_, (env, exp)) = self.queue.remove(&member_key(room_id, device_id))?;
        if exp < Utc::now() {
            self.expired.fetch_add(1, Ordering::Relaxed);
            None
        } else {
            Some(env)
        }
    }

    /// Revokes a room member, removes all of its routes and evicts every one
    /// of its sockets, so no socket of a revoked member stays routed. Returns
    /// `false` when the device is not a member of the room.
    pub fn revoke(&self, room_id: &str, device_id: &str) -> bool {
        let Some(mut room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        let Some(device) = room.devices.get_mut(device_id) else {
            return false;
        };
        device.revoked = true;
        room.epoch += 1;
        let removed = self.member_routes.remove(&member_key(room_id, device_id));
        drop(room);
        for route in removed.into_iter().flat_map(|(_, routes)| routes) {
            route.evict();
        }
        true
    }

    pub fn gc(&self) {
        let now = Utc::now();
        self.pairing_by_code
            .retain(|_, s| s.expires_at > now && !s.completed);
        let client_window = self.join_limits.client_fail_window();
        self.ip_fails
            .retain(|_, (_, window)| window.elapsed() <= client_window);
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
            "clipsync_ws_connections {}\nclipsync_frames_forwarded {}\nclipsync_pairing_ok {}\nclipsync_pairing_fail {}\nclipsync_pairing_throttled {}\nclipsync_expired {}\n",
            self.live_connections.load(Ordering::Relaxed),
            self.frames_forwarded.load(Ordering::Relaxed),
            self.pairing_ok.load(Ordering::Relaxed),
            self.pairing_fail.load(Ordering::Relaxed),
            self.pairing_throttled.load(Ordering::Relaxed),
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
