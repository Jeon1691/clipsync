// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Envelope version policy and receive-side admission of envelopes:
//! addressing, expiry, downgrade and replay.
//!
//! Send side ([`version_for_room_state`]): v1 to a peer until there is positive,
//! authenticated evidence that it opens v2, then v2 for good. A broadcast goes
//! out in the version of the least capable pinned peer of the room.
//!
//! Order of checks for every text envelope or manifest:
//! 1. [`check_header`] — cheap checks on the (possibly unauthenticated) header.
//! 2. [`check_version`] — v2 always; v1 only from senders never seen on v2,
//!    and never in strict mode.
//! 3. AEAD open — for v2 this authenticates every header field checked above.
//! 4. [`commit_sequence`] — v2 only: durably require a strictly newer `seq`.
//!
//! The sequence is committed only after AEAD succeeds so forged headers cannot
//! advance the persisted high-water mark.

use chrono::{DateTime, Duration, Utc};

use clipsync_protocol::{
    DeviceId, ItemKind, Recipient, RoutingHeader, CLOCK_SKEW_SECS, ENVELOPE_VERSION_AUTHENTICATED,
    ENVELOPE_VERSION_LEGACY, HARD_MAX_TTL_SECS,
};
use clipsync_storage::{LocalStore, RoomRecord, RoomReplayState, SeqCheck, SyncConfig};

use crate::CoreError;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvelopeRejection {
    #[error("unsupported envelope version {0}")]
    UnsupportedVersion(u16),
    #[error("envelope is for another room")]
    WrongRoom,
    #[error("envelope is addressed to another device")]
    WrongRecipient,
    #[error("envelope sender is not a peer of this room")]
    UnknownSender,
    #[error("envelope item kind does not match its frame")]
    WrongItemKind,
    #[error(
        "envelope expired; if this repeats for fresh items, set both devices' clocks to automatic time (tolerance {skew} s)",
        skew = CLOCK_SKEW_SECS
    )]
    Expired,
    #[error(
        "envelope sent in the future; the sender's clock is more than {skew} s ahead, set both devices' clocks to automatic time",
        skew = CLOCK_SKEW_SECS
    )]
    NotYetValid,
    #[error("envelope lifetime exceeds the protocol maximum")]
    LifetimeTooLong,
    #[error("legacy envelope from a sender that uses authenticated envelopes")]
    Downgrade,
    #[error("legacy envelope refused by sync.strict_envelopes")]
    LegacyRefused,
    #[error("replayed envelope (seq {seq} <= {highest_seq})")]
    Replayed { seq: u64, highest_seq: u64 },
}

/// How this device chooses and accepts envelope versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvelopePolicy {
    /// Default: send v1 to a peer until it has proved it opens v2, then v2;
    /// accept v2 always and v1 from a sender until its first accepted v2.
    Negotiate,
    /// `sync.legacy_envelopes`: always send v1; receive as in `Negotiate`.
    Legacy,
    /// `sync.strict_envelopes`: always send v2; refuse every v1 envelope.
    Strict,
}

impl EnvelopePolicy {
    /// Strict wins over legacy, so a conflicting config fails safe.
    pub(crate) fn from_config(sync: &SyncConfig) -> Self {
        if sync.strict_envelopes {
            Self::Strict
        } else if sync.legacy_envelopes {
            Self::Legacy
        } else {
            Self::Negotiate
        }
    }

    /// Envelope versions this device accepts, as advertised in the Noise
    /// handshake payload.
    pub(crate) fn accepted_versions(self) -> &'static [u16] {
        match self {
            Self::Strict => &[ENVELOPE_VERSION_AUTHENTICATED],
            Self::Negotiate | Self::Legacy => {
                &[ENVELOPE_VERSION_LEGACY, ENVELOPE_VERSION_AUTHENTICATED]
            }
        }
    }
}

/// [`version_for_room_state`] for the replay state currently on disk.
///
/// Test helper: a snapshot read. Stamping an envelope decides with
/// [`version_for_room_state`] inside the locked `seq` reservation instead.
#[cfg(test)]
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub(crate) fn sending_version(
    store: &LocalStore,
    room: &RoomRecord,
    local: &DeviceId,
    policy: EnvelopePolicy,
) -> Result<u16, CoreError> {
    let state = match policy {
        // The fixed policies need no state; skip reading it.
        EnvelopePolicy::Legacy | EnvelopePolicy::Strict => None,
        EnvelopePolicy::Negotiate => Some(store.load_replay_state()?),
    };
    let room_state = state
        .as_ref()
        .and_then(|state| state.rooms.get(room.room_id.as_str()));
    Ok(version_for_room_state(room_state, room, local, policy))
}

/// Envelope version for the next broadcast into `room`, decided from the
/// room's replay state `room_state` (`None`: nothing recorded for it yet).
///
/// Under [`EnvelopePolicy::Negotiate`] that is v2 only when every pinned peer
/// of the room (other than `local`) has shown, through an authenticated
/// channel, that it opens v2: it sent this device a v2 envelope, or it
/// advertised v2 in its Noise handshake payload. Otherwise the least capable
/// peer gets what it can read: v1. The evidence is persisted and never
/// revoked, so a relay cannot roll a peer back to v1; and once this device has
/// sent v2 into the room it keeps sending v2 there.
pub(crate) fn version_for_room_state(
    room_state: Option<&RoomReplayState>,
    room: &RoomRecord,
    local: &DeviceId,
    policy: EnvelopePolicy,
) -> u16 {
    match policy {
        EnvelopePolicy::Legacy => ENVELOPE_VERSION_LEGACY,
        EnvelopePolicy::Strict => ENVELOPE_VERSION_AUTHENTICATED,
        EnvelopePolicy::Negotiate => {
            // Once v2 went out, peers refuse this device's v1: never fall back,
            // even if a peer without evidence is pinned later.
            if room_state.is_some_and(|state| state.sent_authenticated) {
                return ENVELOPE_VERSION_AUTHENTICATED;
            }
            let all_accept = room
                .peers
                .iter()
                .filter(|peer| peer.device_id != *local)
                .all(|peer| {
                    room_state
                        .is_some_and(|state| state.peer_accepts_authenticated(&peer.device_id))
                });
            if all_accept {
                ENVELOPE_VERSION_AUTHENTICATED
            } else {
                ENVELOPE_VERSION_LEGACY
            }
        }
    }
}

/// Floor for outgoing `seq`: microseconds since the Unix epoch, so numbers keep
/// increasing even if the persisted counter is lost or two processes send.
pub(crate) fn sequence_floor(now: DateTime<Utc>) -> u64 {
    u64::try_from(now.timestamp_micros()).unwrap_or(0)
}

/// Header checks that do not need the key. For v1 they run on unauthenticated
/// values (a relay may rewrite them); for v2 the AEAD open that follows binds
/// every checked value.
pub(crate) fn check_header(
    routing: &RoutingHeader,
    room: &RoomRecord,
    local: &DeviceId,
    expected_kind: Option<ItemKind>,
    now: DateTime<Utc>,
) -> Result<(), EnvelopeRejection> {
    if routing.version != ENVELOPE_VERSION_LEGACY
        && routing.version != ENVELOPE_VERSION_AUTHENTICATED
    {
        return Err(EnvelopeRejection::UnsupportedVersion(routing.version));
    }
    if routing.room_id != room.room_id {
        return Err(EnvelopeRejection::WrongRoom);
    }
    match &routing.recipient {
        Recipient::Broadcast => {}
        Recipient::Device(device) if device == local => {}
        Recipient::Device(_) => return Err(EnvelopeRejection::WrongRecipient),
    }
    if routing.sender_device_id == *local
        || !room
            .peers
            .iter()
            .any(|peer| peer.device_id == routing.sender_device_id)
    {
        return Err(EnvelopeRejection::UnknownSender);
    }
    if expected_kind.is_some_and(|kind| kind != routing.item_kind) {
        return Err(EnvelopeRejection::WrongItemKind);
    }
    let skew = Duration::seconds(CLOCK_SKEW_SECS);
    // Only v2 binds `sent_at`. For v1 a relay can rewrite it, so refusing an
    // early v1 adds nothing and would stop released peers whose clock runs
    // ahead, which synced before v2 existed. Expiry stays for both versions.
    // Checked arithmetic throughout: header timestamps come from the relay
    // before authentication, and an overflowing `DateTime + Duration` panics
    // (process abort in release builds). Overflow rejects the envelope.
    if routing.version == ENVELOPE_VERSION_AUTHENTICATED {
        let latest_sent = now
            .checked_add_signed(skew)
            .ok_or(EnvelopeRejection::NotYetValid)?;
        if routing.sent_at > latest_sent {
            return Err(EnvelopeRejection::NotYetValid);
        }
    }
    let honoured_until = routing
        .expires_at
        .checked_add_signed(skew)
        .ok_or(EnvelopeRejection::LifetimeTooLong)?;
    if honoured_until < now {
        return Err(EnvelopeRejection::Expired);
    }
    // Saturate: an unrepresentable TTL can only be "too long".
    let max_lifetime = Duration::seconds(i64::try_from(HARD_MAX_TTL_SECS).unwrap_or(i64::MAX));
    if routing.expires_at.signed_duration_since(routing.sent_at) > max_lifetime {
        return Err(EnvelopeRejection::LifetimeTooLong);
    }
    Ok(())
}

/// Legacy v1 envelopes are accepted only from senders that have never
/// delivered an authenticated v2 envelope to this device, and never in strict
/// mode. A handshake capability advertisement alone does not refuse v1: the
/// peer may run with `sync.legacy_envelopes`.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub(crate) fn check_version(
    store: &LocalStore,
    routing: &RoutingHeader,
    policy: EnvelopePolicy,
) -> Result<(), CoreError> {
    if routing.version != ENVELOPE_VERSION_LEGACY {
        return Ok(());
    }
    if policy == EnvelopePolicy::Strict {
        return Err(EnvelopeRejection::LegacyRefused.into());
    }
    if store.sender_is_authenticated(&routing.room_id, &routing.sender_device_id)? {
        return Err(EnvelopeRejection::Downgrade.into());
    }
    Ok(())
}

/// After a successful AEAD open, durably require that a v2 `seq` is newer than
/// every earlier v2 envelope from the same sender. v1 headers are not
/// authenticated and legacy senders restart `seq` at 1, so v1 relies on the
/// in-memory message-ID cache and the header checks only.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub(crate) fn commit_sequence(
    store: &LocalStore,
    routing: &RoutingHeader,
) -> Result<(), CoreError> {
    if routing.version != ENVELOPE_VERSION_AUTHENTICATED {
        return Ok(());
    }
    match store.accept_authenticated_seq(
        &routing.room_id,
        &routing.sender_device_id,
        routing.seq,
    )? {
        SeqCheck::Accepted => Ok(()),
        SeqCheck::Replayed { highest_seq } => Err(EnvelopeRejection::Replayed {
            seq: routing.seq,
            highest_seq,
        }
        .into()),
    }
}
