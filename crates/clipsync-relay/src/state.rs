// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use clipsync_protocol::Timestamp;
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
use crate::connection_limits::{ConnectionCounts, ConnectionRefused, ConnectionSlot};
use crate::join_limits::{PairingJoinLimits, PAIRING_CODE_SPACE};
use crate::rate_limit::{client_key, Budget};
use crate::resource_limits::{
    RelayResourceLimits, SocketDeadlines, MAX_ACTIVE_PAIRING_SESSIONS,
    MAX_CLIENT_QUEUED_TEXT_ENVELOPES, MAX_PAIRING_FIELD_BYTES, MAX_QUEUED_TEXT_ENVELOPES,
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
/// Scans for an idle room to evict before giving up. A scan only misses when
/// a device attached to its candidate between the scan and the removal, so a
/// few scans always find one unless every room really has a device online.
const ROOM_EVICTION_ATTEMPTS: usize = 4;

/// One offline text envelope, charged to the client that sent it.
struct QueuedText {
    envelope: QueuedEnvelope,
    expires_at: Timestamp,
    /// The sender's client key (see [`client_key`]).
    sender: IpAddr,
}

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
    /// Sockets past a valid `hello`, pairing or room member (exported as
    /// `clipsync_ws_connections`); every open socket, authenticated or not,
    /// is counted by `connection_counts` instead.
    live_connections: Arc<AtomicU64>,
    queue: DashMap<MemberKey, QueuedText>,
    /// Queued envelopes per sending client.
    queue_senders: DashMap<IpAddr, usize>,
    /// Orders room activity for least-recently-used eviction.
    activity_clock: AtomicU64,
    /// Failed pairing joins, per client and relay-wide.
    join_budget: Budget,
    /// Created pairing sessions, per client and relay-wide.
    create_budget: Budget,
    /// Serializes the capacity check with the insert of a new pairing session,
    /// so concurrent creations cannot overshoot the session or room caps.
    pairing_admission: Mutex<()>,
    /// Test hook: every event handed to [`Self::webhook_spawn`], recorded
    /// whether or not a webhook is configured, so tests can pin the body
    /// without a reachable (non-loopback) webhook target.
    #[cfg(test)]
    pub(crate) webhook_events: Mutex<Vec<serde_json::Value>>,
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
    /// Offline text envelopes not queued because the queue, or the sender's
    /// share of it, was full.
    pub queue_full: AtomicU64,
    /// Rooms dropped to make way for a new pairing at the room cap.
    pub rooms_evicted: AtomicU64,
    pub expired: AtomicU64,
}

pub struct PairingSession {
    pub session_id: PairingSessionId,
    pub room_id: RoomId,
    pub code: String,
    pub expires_at: Timestamp,
    pub attempts: u32,
    pub creator: PeerInfo,
    pub joiner: Option<PeerInfo>,
    pub ttl_secs: u64,
    pub bind_count: u32,
    pub completed: bool,
    // Only authenticated sockets for this session may exchange pairing frames.
    pub(crate) pairing_senders: [Option<mpsc::Sender<WsOut>>; 2],
    pub(crate) pending_spake: [Option<PendingSpake>; 2],
    // The joiner's commitment, held like its SPAKE message until the creator
    // is connected. Only side 1 (the joiner) ever fills its slot.
    pub(crate) pending_commit: [Option<String>; 2],
}

/// A SPAKE message retained until the peer's authenticated socket arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSpake {
    pub(crate) message: String,
    pub(crate) after_commitment: bool,
}

impl PairingSession {
    /// Drop every retained pairing frame (expiry, completion, a replaced or
    /// closed pairing socket).
    pub(crate) fn clear_pending(&mut self) {
        self.pending_spake = [None, None];
        self.pending_commit = [None, None];
    }
}

pub struct Room {
    pub room_id: RoomId,
    pub devices: HashMap<String, DeviceEntry>,
    pub epoch: u64,
    pub default_ttl: u64,
    /// When the device tokens issued for this room expire.
    pub tokens_expire_at: Timestamp,
    /// When the room was last used, on the relay's activity clock: a device
    /// registering, sending or leaving.
    pub(crate) last_active: AtomicU64,
}

/// Stamps a room member socket's traffic and departure on its room's
/// activity clock, for least-recently-used eviction. Whether the room may be
/// evicted is decided by its member routes (`RelayState::is_online`), not by
/// this stamp.
pub(crate) struct RoomPresence {
    state: Arc<RelayState>,
    room_id: String,
}

impl RoomPresence {
    pub(crate) fn new(state: &Arc<RelayState>, room_id: &str) -> Self {
        Self {
            state: state.clone(),
            room_id: room_id.to_string(),
        }
    }

    /// Record traffic in the room.
    pub(crate) fn touch(&self) {
        self.state.touch_room(&self.room_id);
    }
}

impl Drop for RoomPresence {
    fn drop(&mut self) {
        self.state.touch_room(&self.room_id);
    }
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

    /// Test helper: apply `resource_limits` without validating them, for
    /// caps far below what a relay accepts at startup.
    #[cfg(test)]
    pub(crate) fn unvalidated(
        config: RelayConfig,
        trusted_proxies: Vec<ipnet::IpNet>,
        resource_limits: RelayResourceLimits,
    ) -> Self {
        Self::build(
            config,
            trusted_proxies,
            PairingJoinLimits::default(),
            resource_limits,
        )
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
            member_routes: DashMap::new(),
            next_connection_id: AtomicU64::new(0),
            live_connections: Arc::new(AtomicU64::new(0)),
            queue: DashMap::new(),
            queue_senders: DashMap::new(),
            activity_clock: AtomicU64::new(0),
            join_budget: Budget::default(),
            create_budget: Budget::default(),
            pairing_admission: Mutex::new(()),
            #[cfg(test)]
            webhook_events: Mutex::default(),
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
            rooms_evicted: AtomicU64::new(0),
            expired: AtomicU64::new(0),
        }
    }

    /// Open a pairing session for a request from client address `ip`. Every
    /// accepted creation counts against the client's and the relay-wide
    /// creation budgets; a spent budget refuses with `Throttled`. At the room
    /// cap the least recently used room with no device online is evicted;
    /// only open sessions at their cap, or a room cap where every room has a
    /// device online, refuse with `Full`.
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
            self.prune_pairings(Timestamp::now());
        }
        while self.pairing_by_code.len() < MAX_ACTIVE_PAIRING_SESSIONS
            && !self.has_pairing_capacity()
            && self.evict_idle_room()
        {}
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
        let expires_at = Timestamp::now() + time::Duration::seconds(PAIRING_TTL_SECS as i64);
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
            pending_commit: [None, None],
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
    /// errs toward evicting or refusing.
    fn has_pairing_capacity(&self) -> bool {
        let sessions = self.pairing_by_code.len();
        sessions < MAX_ACTIVE_PAIRING_SESSIONS
            && self.rooms.len() + sessions < self.resource_limits.max_rooms
    }

    /// Drop the least recently used room that has no device online. Returns
    /// false when every room has one. Its devices then get `unknown room` and
    /// must pair again, as after a relay restart. The scan is linear in the
    /// room count, which creation's rate limit keeps cheap.
    fn evict_idle_room(&self) -> bool {
        for _ in 0..ROOM_EVICTION_ATTEMPTS {
            let candidate = self
                .rooms
                .iter()
                .filter(|room| !self.room_has_device_online(room.key(), room.value()))
                .map(|room| (room.last_active.load(Ordering::Relaxed), room.key().clone()))
                .min();
            let Some((_, room_id)) = candidate else {
                return false;
            };
            // Re-checked under the room's write lock. `register_conn` holds
            // the room's entry while it inserts a member route, so a room a
            // device just registered in is never removed. A room used since
            // the scan but idle again is still a fair victim. No test pins
            // this re-check: losing the race needs a registration to land
            // between the scan and the removal, which the tests cannot
            // schedule deterministically, so it rests on the lock argument
            // above.
            let removed = self
                .rooms
                .remove_if(&room_id, |id, room| !self.room_has_device_online(id, room));
            if removed.is_some() {
                self.rooms_evicted.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    fn next_activity(&self) -> u64 {
        self.activity_clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn touch_room(&self, room_id: &str) {
        let stamp = self.next_activity();
        if let Some(room) = self.rooms.get(room_id) {
            room.last_active.store(stamp, Ordering::Relaxed);
        }
    }

    /// Whether any device of `room` has a routed member socket. Lock order is
    /// rooms, then member routes, as in `register_conn` and `revoke`.
    fn room_has_device_online(&self, room_id: &str, room: &Room) -> bool {
        room.devices
            .keys()
            .any(|device_id| self.is_online(room_id, device_id))
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
        if session.expires_at < Timestamp::now() {
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
        if session.expires_at < Timestamp::now() {
            session.clear_pending();
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
        session.clear_pending();
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
                tokens_expire_at: Timestamp::now()
                    + time::Duration::seconds(DEVICE_TOKEN_TTL_SECS + TOKEN_EXPIRY_GRACE_SECS),
                last_active: AtomicU64::new(self.next_activity()),
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
        // removes the member's routes under the same entry's write lock, and
        // with room eviction, which re-checks the routes under that lock.
        let room = self.rooms.get(room_id).ok_or("unknown room")?;
        let device = room.devices.get(device_id).ok_or("unknown device")?;
        if device.revoked {
            return Err("device revoked");
        }
        let id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        room.last_active
            .store(self.next_activity(), Ordering::Relaxed);
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

    /// Queue `env` as the latest offline text for `device_id` in `room_id`,
    /// sent from client
    /// address `sender`, replacing any earlier one, until `expires_at` but
    /// never longer than `QUEUE_TTL_SECS`: the expiry is sender-chosen.
    /// Returns false, queuing nothing, when the queue already holds
    /// `MAX_QUEUED_TEXT_ENVELOPES` other recipients or the sender already has
    /// `MAX_CLIENT_QUEUED_TEXT_ENVELOPES` other envelopes queued. Concurrent
    /// senders may pass the checks together, so either cap can be exceeded by
    /// at most the number of concurrent connections.
    pub fn queue_text(
        &self,
        room_id: &str,
        device_id: &str,
        env: QueuedEnvelope,
        expires_at: Timestamp,
        sender: IpAddr,
    ) -> bool {
        let sender = client_key(sender);
        let now = Timestamp::now();
        let expires_at = expires_at.min(now + time::Duration::seconds(QUEUE_TTL_SECS as i64));
        let key = member_key(room_id, device_id);
        let replaced_sender = self.queue.get(&key).map(|queued| queued.sender);
        if replaced_sender != Some(sender)
            && !self.queue_has_room(replaced_sender.is_some(), sender)
        {
            self.prune_queue(now);
            let replaced_sender = self.queue.get(&key).map(|queued| queued.sender);
            if !self.queue_has_room(replaced_sender.is_some(), sender) {
                self.queue_full.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        *self.queue_senders.entry(sender).or_insert(0) += 1;
        let replaced = self.queue.insert(
            key,
            QueuedText {
                envelope: env,
                expires_at,
                sender,
            },
        );
        if let Some(replaced) = replaced {
            self.uncharge_sender(replaced.sender);
        }
        true
    }

    /// Whether `sender` may add an envelope: below its own share, and either
    /// replacing a recipient's envelope or below the relay-wide cap.
    fn queue_has_room(&self, replaces: bool, sender: IpAddr) -> bool {
        let held = self.queue_senders.get(&sender).map_or(0, |held| *held);
        held < MAX_CLIENT_QUEUED_TEXT_ENVELOPES
            && (replaces || self.queue.len() < MAX_QUEUED_TEXT_ENVELOPES)
    }

    fn uncharge_sender(&self, sender: IpAddr) {
        self.queue_senders.remove_if_mut(&sender, |_, held| {
            *held = held.saturating_sub(1);
            *held == 0
        });
    }

    pub fn take_queue(&self, room_id: &str, device_id: &str) -> Option<QueuedEnvelope> {
        let (_, queued) = self.queue.remove(&member_key(room_id, device_id))?;
        self.uncharge_sender(queued.sender);
        if queued.expires_at < Timestamp::now() {
            self.expired.fetch_add(1, Ordering::Relaxed);
            None
        } else {
            Some(queued.envelope)
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

    /// Take a connection slot for a WebSocket from client address `ip`, or
    /// refuse when the relay, that client or its IPv6 site holds its maximum.
    pub(crate) fn acquire_connection(
        &self,
        ip: IpAddr,
    ) -> Result<ConnectionSlot, ConnectionRefused> {
        self.connection_counts
            .acquire(ip, self.resource_limits.connection_limits())
            .inspect_err(|_| {
                self.connections_refused.fetch_add(1, Ordering::Relaxed);
            })
    }

    pub fn gc(&self) {
        let now = Timestamp::now();
        self.prune_pairings(now);
        self.join_budget
            .prune(self.join_limits.client_fail_window());
        self.create_budget
            .prune(self.resource_limits.create_budget().client_window);
        self.prune_rooms(now);
        self.prune_queue(now);
    }

    /// Drop expired and completed pairing sessions from both indexes.
    fn prune_pairings(&self, now: Timestamp) {
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
    fn prune_rooms(&self, now: Timestamp) {
        self.rooms.retain(|room_id, room| {
            let any_active = room.devices.values().any(|device| !device.revoked);
            let tokens_valid = room.tokens_expire_at > now;
            any_active && (tokens_valid || self.room_has_device_online(room_id, room))
        });
    }

    fn prune_queue(&self, now: Timestamp) {
        let mut expired = Vec::new();
        self.queue.retain(|_, queued| {
            let keep = queued.expires_at > now;
            if !keep {
                expired.push(queued.sender);
            }
            keep
        });
        for sender in expired {
            self.expired.fetch_add(1, Ordering::Relaxed);
            self.uncharge_sender(sender);
        }
    }

    pub fn metrics_text(&self) -> String {
        format!(
            "clipsync_ws_connections {}\nclipsync_frames_forwarded {}\nclipsync_pairing_ok {}\nclipsync_pairing_fail {}\nclipsync_pairing_throttled {}\nclipsync_expired {}\nclipsync_ws_sockets {}\nclipsync_ws_refused {}\nclipsync_pairing_sessions {}\nclipsync_pairing_create_throttled {}\nclipsync_pairing_create_full {}\nclipsync_rooms {}\nclipsync_queue_full {}\nclipsync_rooms_evicted {}\n",
            self.live_connections.load(Ordering::Relaxed),
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
            self.rooms_evicted.load(Ordering::Relaxed),
        )
    }

    pub fn webhook_spawn(&self, event: serde_json::Value) {
        #[cfg(test)]
        self.webhook_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
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
