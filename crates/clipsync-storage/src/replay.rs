// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Persisted envelope sequence state that survives daemon restarts.
//!
//! Senders keep their last issued `seq` per room so it stays monotonic across
//! restarts. Receivers keep, per room and sender, the highest `seq` accepted
//! from an authenticated (v2) envelope, and whether that sender has ever used
//! one; a sender seen on v2 may not fall back to unauthenticated v1 envelopes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use clipsync_protocol::{DeviceId, RoomId};

use crate::store::LocalStore;
use crate::Result;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayState {
    #[serde(default)]
    pub rooms: BTreeMap<String, RoomReplayState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomReplayState {
    /// Last `seq` this device put in an outgoing envelope for the room.
    #[serde(default)]
    pub last_sent_seq: u64,
    /// Receive-side state keyed by sender device ID.
    #[serde(default)]
    pub senders: BTreeMap<String, SenderReplayState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderReplayState {
    /// Highest `seq` accepted from an authenticated envelope of this sender.
    #[serde(default)]
    pub highest_seq: u64,
    /// The sender has delivered an authenticated envelope; refuse legacy ones.
    #[serde(default)]
    pub authenticated: bool,
}

/// Outcome of offering an authenticated envelope's `seq` to the replay state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqCheck {
    /// Newer than anything accepted before; now recorded durably.
    Accepted,
    /// Not newer than an already accepted envelope from the same sender.
    Replayed { highest_seq: u64 },
}

impl LocalStore {
    pub fn load_replay_state(&self) -> Result<ReplayState> {
        let path = self.paths.replay_state_file();
        if !path.exists() {
            return Ok(ReplayState::default());
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    fn save_replay_state(&self, state: &ReplayState) -> Result<()> {
        crate::secrets::write_private(
            &self.paths.replay_state_file(),
            &serde_json::to_vec_pretty(state)?,
        )?;
        Ok(())
    }

    /// Reserve the next outgoing `seq` for `room_id` and persist it before use.
    ///
    /// The value is at least `floor` (callers pass a wall-clock reading), so a
    /// lost state file or two processes sending concurrently still move forward;
    /// it is always above the last persisted value, so a clock stepping back
    /// does not reuse a number.
    pub fn next_send_seq(&self, room_id: &RoomId, floor: u64) -> Result<u64> {
        let mut state = self.load_replay_state()?;
        let room = state.rooms.entry(room_id.to_string()).or_default();
        let seq = room.last_sent_seq.saturating_add(1).max(floor);
        room.last_sent_seq = seq;
        self.save_replay_state(&state)?;
        Ok(seq)
    }

    /// Accept `seq` from an authenticated envelope only if it is newer than
    /// every earlier one from `sender`; record it durably before returning.
    pub fn accept_authenticated_seq(
        &self,
        room_id: &RoomId,
        sender: &DeviceId,
        seq: u64,
    ) -> Result<SeqCheck> {
        let mut state = self.load_replay_state()?;
        let entry = state
            .rooms
            .entry(room_id.to_string())
            .or_default()
            .senders
            .entry(sender.to_string())
            .or_default();
        if entry.authenticated && seq <= entry.highest_seq {
            return Ok(SeqCheck::Replayed {
                highest_seq: entry.highest_seq,
            });
        }
        entry.highest_seq = seq;
        entry.authenticated = true;
        self.save_replay_state(&state)?;
        Ok(SeqCheck::Accepted)
    }

    /// Whether `sender` has already delivered an authenticated envelope here.
    pub fn sender_is_authenticated(&self, room_id: &RoomId, sender: &DeviceId) -> Result<bool> {
        Ok(self
            .load_replay_state()?
            .rooms
            .get(room_id.as_str())
            .and_then(|room| room.senders.get(sender.as_str()))
            .is_some_and(|entry| entry.authenticated))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::AppPaths;

    fn store(root: &std::path::Path) -> LocalStore {
        LocalStore::open_from(AppPaths::from_root(root.into())).unwrap()
    }

    #[test]
    fn accepted_sequence_numbers_survive_reopening_the_store() {
        let root = tempfile::tempdir().unwrap();
        let room = RoomId::from("room");
        let sender = DeviceId::from("sender");
        let first = store(root.path());
        assert!(!first.sender_is_authenticated(&room, &sender).unwrap());
        assert_eq!(
            first.accept_authenticated_seq(&room, &sender, 5).unwrap(),
            SeqCheck::Accepted
        );
        assert_eq!(
            first.accept_authenticated_seq(&room, &sender, 5).unwrap(),
            SeqCheck::Replayed { highest_seq: 5 }
        );
        drop(first);

        let reopened = store(root.path());
        assert!(reopened.sender_is_authenticated(&room, &sender).unwrap());
        for stale in [0, 4, 5] {
            assert_eq!(
                reopened
                    .accept_authenticated_seq(&room, &sender, stale)
                    .unwrap(),
                SeqCheck::Replayed { highest_seq: 5 }
            );
        }
        assert_eq!(
            reopened
                .accept_authenticated_seq(&room, &sender, 6)
                .unwrap(),
            SeqCheck::Accepted
        );
        // State is per room and per sender.
        let other = DeviceId::from("other");
        assert_eq!(
            reopened.accept_authenticated_seq(&room, &other, 1).unwrap(),
            SeqCheck::Accepted
        );
        assert_eq!(
            reopened
                .accept_authenticated_seq(&RoomId::from("room2"), &sender, 1)
                .unwrap(),
            SeqCheck::Accepted
        );
    }

    #[test]
    fn send_sequence_is_monotonic_across_restarts_clock_steps_and_lost_state() {
        let root = tempfile::tempdir().unwrap();
        let room = RoomId::from("room");
        let first = store(root.path());
        assert_eq!(first.next_send_seq(&room, 0).unwrap(), 1);
        assert_eq!(first.next_send_seq(&room, 100).unwrap(), 100);
        drop(first);
        let reopened = store(root.path());
        // A clock that stepped backwards never reuses an issued number.
        assert_eq!(reopened.next_send_seq(&room, 50).unwrap(), 101);
        std::fs::remove_file(reopened.paths.replay_state_file()).unwrap();
        // Without state the wall-clock floor still moves past old numbers.
        assert_eq!(reopened.next_send_seq(&room, 1_000).unwrap(), 1_000);
    }

    #[test]
    fn legacy_and_partial_files_load_with_defaults_and_corruption_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        std::fs::write(
            store.paths.replay_state_file(),
            br#"{"rooms":{"room":{"senders":{"sender":{}}}},"future_field":1}"#,
        )
        .unwrap();
        let state = store.load_replay_state().unwrap();
        assert_eq!(
            state.rooms["room"].senders["sender"],
            SenderReplayState::default()
        );
        std::fs::write(store.paths.replay_state_file(), b"{broken").unwrap();
        assert!(store.load_replay_state().is_err());
        assert!(store
            .accept_authenticated_seq(&RoomId::from("room"), &DeviceId::from("sender"), 1)
            .is_err());
        assert!(store.next_send_seq(&RoomId::from("room"), 1).is_err());
        assert_eq!(
            std::fs::read(store.paths.replay_state_file()).unwrap(),
            b"{broken",
            "a corrupt replay file must not be silently reset"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replay_state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        store.next_send_seq(&RoomId::from("room"), 1).unwrap();
        let mode = std::fs::metadata(store.paths.replay_state_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
