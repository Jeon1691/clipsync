// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
pub const PAIRING_TTL_SECS: u64 = 120;
pub const PAIRING_MAX_ATTEMPTS: u32 = 3;
pub const PAIRING_MAX_JOINERS: u32 = 1;

pub const DEFAULT_TTL_SECS: u64 = 60;
pub const MIN_TTL_SECS: u64 = 10;
pub const HARD_MAX_TTL_SECS: u64 = 300;
pub const QUEUE_TTL_SECS: u64 = 60;

pub const TEXT_MAX_BYTES: usize = 256 * 1024;
pub const IMAGE_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const FILE_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const TRANSFER_MAX_BYTES: u64 = 128 * 1024 * 1024;
pub const FILE_MAX_COUNT: usize = 16;

pub const CHUNK_SIZE: usize = 256 * 1024;
pub const FRAME_HARD_CAP: usize = 384 * 1024;
pub const AEAD_NONCE_LEN: usize = 12;
pub const AEAD_TAG_LEN: usize = 16;

pub const DEBOUNCE_MS: u64 = 200;
pub const WATCH_POLL_MS: u64 = 200;
pub const HEARTBEAT_SECS: u64 = 20;
pub const TRANSFER_IDLE_SECS: u64 = 60;
pub const REPLAY_CACHE_TTL_SECS: u64 = 120;
pub const LOOP_CACHE_TTL_SECS: u64 = 15;
/// Tolerated sender/receiver wall-clock difference when a receiver checks an
/// envelope's `sent_at`/`expires_at`. NTP-synced hosts agree to well under a
/// second; 30 s also absorbs loosely synced mobile clocks. Replay protection for
/// authenticated envelopes rests on per-sender sequence numbers, not on expiry,
/// so this margin does not widen the replay window.
pub const CLOCK_SKEW_SECS: i64 = 30;

pub const NOTIFY_COOLDOWN_SECS: u64 = 8;

pub fn clamp_ttl(secs: u64) -> u64 {
    secs.clamp(MIN_TTL_SECS, HARD_MAX_TTL_SECS)
}
