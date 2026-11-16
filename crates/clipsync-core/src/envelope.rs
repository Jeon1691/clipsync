// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Receive-side admission of envelopes: addressing, expiry, downgrade and replay.
//!
//! Order of checks for every text envelope or manifest:
//! 1. [`check_header`] — cheap checks on the (possibly unauthenticated) header.
//! 2. [`check_version`] — v2 always; v1 only from senders never seen on v2.
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
use clipsync_storage::{LocalStore, RoomRecord, SeqCheck};

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
    #[error("envelope expired")]
    Expired,
    #[error("envelope sent in the future")]
    NotYetValid,
    #[error("envelope lifetime exceeds the protocol maximum")]
    LifetimeTooLong,
    #[error("legacy envelope from a sender that uses authenticated envelopes")]
    Downgrade,
    #[error("replayed envelope (seq {seq} <= {highest_seq})")]
    Replayed { seq: u64, highest_seq: u64 },
}

/// Envelope version this device emits, per `sync.legacy_envelopes`.
pub(crate) fn sending_version(legacy_envelopes: bool) -> u16 {
    if legacy_envelopes {
        ENVELOPE_VERSION_LEGACY
    } else {
        ENVELOPE_VERSION_AUTHENTICATED
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
    if routing.sent_at > now + skew {
        return Err(EnvelopeRejection::NotYetValid);
    }
    if routing.expires_at + skew < now {
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
/// delivered an authenticated v2 envelope to this device.
#[expect(
    clippy::result_large_err,
    reason = "Preserve the public CoreError payload and conversions; see docs/compiler-warnings.md."
)]
pub(crate) fn check_version(store: &LocalStore, routing: &RoutingHeader) -> Result<(), CoreError> {
    if routing.version == ENVELOPE_VERSION_LEGACY
        && store.sender_is_authenticated(&routing.room_id, &routing.sender_device_id)?
    {
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
