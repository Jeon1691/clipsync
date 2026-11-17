// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Persisted envelope sequence state that survives daemon restarts.
//!
//! Senders keep their last issued `seq` per room so it stays monotonic across
//! restarts. Receivers keep, per room and sender, the highest `seq` accepted
//! from an authenticated (v2) envelope, and whether that sender has ever used
//! one; a sender seen on v2 may not fall back to unauthenticated v1 envelopes.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::Path;

use serde::{Deserialize, Serialize};

use clipsync_protocol::{DeviceId, RoomId};

use crate::paths::set_private_file;
#[cfg(unix)]
use crate::paths::PRIVATE_FILE_MODE;
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
    /// This device has sent an authenticated envelope into the room. Peers
    /// that accepted it refuse its v1 from then on, so negotiation never falls
    /// back to v1 for this room (explicit `sync.legacy_envelopes` aside).
    #[serde(default)]
    pub sent_authenticated: bool,
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
    /// The peer advertised authenticated envelopes in an authenticated Noise
    /// handshake. Send-side only: this device sends v2 to it from now on, but
    /// it does not by itself make legacy envelopes from the peer a downgrade.
    #[serde(default)]
    pub advertised_authenticated: bool,
}

impl SenderReplayState {
    /// Positive evidence that the peer can open v2: it sent one, or it
    /// advertised the capability in an authenticated handshake. Sticky.
    pub fn accepts_authenticated(&self) -> bool {
        self.authenticated || self.advertised_authenticated
    }
}

impl RoomReplayState {
    /// Whether `peer` has shown it can open authenticated envelopes.
    pub fn peer_accepts_authenticated(&self, peer: &DeviceId) -> bool {
        self.senders
            .get(peer.as_str())
            .is_some_and(SenderReplayState::accepts_authenticated)
    }
}

/// Outcome of offering an authenticated envelope's `seq` to the replay state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqCheck {
    /// Newer than anything accepted before; now recorded durably.
    Accepted,
    /// Not newer than an already accepted envelope from the same sender.
    Replayed { highest_seq: u64 },
}

/// Exclusive advisory lock on the replay-state lock file, held for one
/// read-modify-write. It is `flock` on Unix (Android included) and
/// `LockFileEx` on Windows: it excludes other processes and other open handles
/// in this process alike, and the OS releases it if the process dies. Failing
/// to take it is an error; there is no in-process-only fallback.
struct ReplayStateLock(File);

impl ReplayStateLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        let mut options = OpenOptions::new();
        // Never truncate: the file has no content, only a lock.
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Created private, like every other file in the data directory.
            options.mode(PRIVATE_FILE_MODE);
        }
        let file = options.open(path)?;
        // Also tighten a lock file left behind by an older or foreign process.
        set_private_file(path)?;
        lock_exclusive(&file)?;
        Ok(Self(file))
    }
}

impl Drop for ReplayStateLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock too, but Windows may do that
        // lazily; unlock explicitly so the next writer is not delayed.
        let _ = unlock(&self.0);
    }
}

/// Block until `file` holds an exclusive `flock`.
///
/// Not `File::lock`: before Rust 1.98 std returns `Unsupported` for it on
/// Android, and the workspace MSRV is lower. std uses the same `flock` call on
/// the other Unix targets, so the semantics are unchanged.
#[cfg(unix)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    use rustix::fs::{flock, FlockOperation};
    loop {
        match flock(file, FlockOperation::LockExclusive) {
            // A signal interrupted the wait; the lock is not held yet.
            Err(rustix::io::Errno::INTR) => {}
            result => return result.map_err(std::io::Error::from),
        }
    }
}

#[cfg(unix)]
fn unlock(file: &File) -> std::io::Result<()> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock).map_err(std::io::Error::from)
}

/// `LockFileEx` on Windows. Any other target gets std's `Unsupported` error,
/// which fails the replay-state update instead of running it unlocked.
#[cfg(not(unix))]
#[expect(
    clippy::disallowed_methods,
    reason = "Not Unix, so not Android; std's lock is LockFileEx on Windows."
)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    file.lock()
}

#[cfg(not(unix))]
#[expect(
    clippy::disallowed_methods,
    reason = "Not Unix, so not Android; std's unlock is UnlockFile on Windows."
)]
fn unlock(file: &File) -> std::io::Result<()> {
    file.unlock()
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

    /// Load, change and save the replay state while holding an exclusive
    /// advisory lock, so a daemon and a one-shot process sharing the data
    /// directory cannot overwrite each other's updates (which could roll back
    /// a `highest_seq` or clear a downgrade flag). `update` returns its result
    /// and whether the state changed; unchanged state is not rewritten.
    ///
    /// Readers need no lock: each save atomically replaces the whole file.
    fn update_replay_state<T>(
        &self,
        update: impl FnOnce(&mut ReplayState) -> (T, bool),
    ) -> Result<T> {
        let _lock = ReplayStateLock::acquire(&self.paths.replay_state_lock_file())?;
        let mut state = self.load_replay_state()?;
        let (value, changed) = update(&mut state);
        if changed {
            self.save_replay_state(&state)?;
        }
        Ok(value)
    }

    /// Reserve the next outgoing `seq` for `room_id` and persist it before use.
    ///
    /// The value is at least `floor` (callers pass a wall-clock reading), so a
    /// lost state file or two processes sending concurrently still move forward;
    /// it is always above the last persisted value, so a clock stepping back
    /// does not reuse a number.
    pub fn next_send_seq(&self, room_id: &RoomId, floor: u64) -> Result<u64> {
        self.reserve_send(room_id, floor, false)
    }

    /// [`Self::next_send_seq`] for an envelope of the given kind; an
    /// authenticated one also records `sent_authenticated` in the same write.
    pub fn reserve_send(&self, room_id: &RoomId, floor: u64, authenticated: bool) -> Result<u64> {
        self.update_replay_state(|state| {
            let room = state.rooms.entry(room_id.to_string()).or_default();
            let seq = room.last_sent_seq.saturating_add(1).max(floor);
            room.last_sent_seq = seq;
            room.sent_authenticated |= authenticated;
            (seq, true)
        })
    }

    /// Accept `seq` from an authenticated envelope only if it is newer than
    /// every earlier one from `sender`; record it durably before returning.
    pub fn accept_authenticated_seq(
        &self,
        room_id: &RoomId,
        sender: &DeviceId,
        seq: u64,
    ) -> Result<SeqCheck> {
        self.update_replay_state(|state| {
            let entry = state
                .rooms
                .entry(room_id.to_string())
                .or_default()
                .senders
                .entry(sender.to_string())
                .or_default();
            if entry.authenticated && seq <= entry.highest_seq {
                let highest_seq = entry.highest_seq;
                return (SeqCheck::Replayed { highest_seq }, false);
            }
            entry.highest_seq = seq;
            entry.authenticated = true;
            (SeqCheck::Accepted, true)
        })
    }

    /// Record that `peer` advertised authenticated envelopes over an
    /// authenticated channel. Returns whether this changed the stored state.
    pub fn mark_peer_advertised_authenticated(
        &self,
        room_id: &RoomId,
        peer: &DeviceId,
    ) -> Result<bool> {
        self.update_replay_state(|state| {
            let entry = state
                .rooms
                .entry(room_id.to_string())
                .or_default()
                .senders
                .entry(peer.to_string())
                .or_default();
            let changed = !entry.advertised_authenticated;
            entry.advertised_authenticated = true;
            (changed, changed)
        })
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
    fn send_side_capability_is_sticky_and_separate_from_the_downgrade_flag() {
        let root = tempfile::tempdir().unwrap();
        let room = RoomId::from("room");
        let advertised = DeviceId::from("advertised");
        let sent_v2 = DeviceId::from("sent-v2");
        let first = store(root.path());
        let accepts = |store: &LocalStore, peer: &DeviceId| {
            store
                .load_replay_state()
                .unwrap()
                .rooms
                .get(room.as_str())
                .is_some_and(|state| state.peer_accepts_authenticated(peer))
        };
        assert!(!accepts(&first, &advertised));
        assert!(first
            .mark_peer_advertised_authenticated(&room, &advertised)
            .unwrap());
        assert!(!first
            .mark_peer_advertised_authenticated(&room, &advertised)
            .unwrap());
        first.accept_authenticated_seq(&room, &sent_v2, 1).unwrap();
        drop(first);
        let reopened = store(root.path());
        assert!(accepts(&reopened, &advertised));
        assert!(accepts(&reopened, &sent_v2));
        // Advertising alone does not refuse the peer's legacy envelopes.
        assert!(!reopened
            .sender_is_authenticated(&room, &advertised)
            .unwrap());
        assert!(reopened.sender_is_authenticated(&room, &sent_v2).unwrap());
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
        assert!(!reopened.load_replay_state().unwrap().rooms[room.as_str()].sent_authenticated);
        assert_eq!(reopened.reserve_send(&room, 0, true).unwrap(), 1_001);
        // A later legacy send (explicit override) does not clear the mark.
        assert_eq!(reopened.reserve_send(&room, 0, false).unwrap(), 1_002);
        assert!(reopened.load_replay_state().unwrap().rooms[room.as_str()].sent_authenticated);
    }

    /// A daemon and a one-shot `push`/`pull` open separate stores on one data
    /// directory. Their read-modify-write cycles must not lose each other's
    /// updates: a lost `highest_seq` would let a replay through, and a lost
    /// `authenticated` flag would re-open the v1 downgrade.
    #[test]
    fn concurrent_stores_on_one_directory_do_not_lose_replay_updates() {
        // Enough interleaved cycles that an unlocked load-modify-save loses
        // updates on every run (each cycle includes an fsync).
        const ROUNDS: u64 = 200;
        let root = tempfile::tempdir().unwrap();
        let room = RoomId::from("room");
        let sender = DeviceId::from("sender");
        let start = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let receiver = store(root.path());
                start.wait();
                for seq in 1..=ROUNDS {
                    assert_eq!(
                        receiver
                            .accept_authenticated_seq(&room, &sender, seq)
                            .unwrap(),
                        SeqCheck::Accepted
                    );
                }
            });
            scope.spawn(|| {
                let one_shot = store(root.path());
                start.wait();
                for _ in 0..ROUNDS {
                    one_shot.reserve_send(&room, 0, true).unwrap();
                }
            });
            scope.spawn(|| {
                let handshakes = store(root.path());
                start.wait();
                for peer in 0..ROUNDS {
                    let peer = DeviceId::from(format!("peer-{peer}").as_str());
                    assert!(handshakes
                        .mark_peer_advertised_authenticated(&room, &peer)
                        .unwrap());
                }
            });
        });
        let reopened = store(root.path());
        let state = reopened.load_replay_state().unwrap();
        let room_state = &state.rooms[room.as_str()];
        assert_eq!(room_state.last_sent_seq, ROUNDS);
        assert!(room_state.sent_authenticated);
        assert_eq!(room_state.senders[sender.as_str()].highest_seq, ROUNDS);
        assert!(room_state.senders[sender.as_str()].authenticated);
        let advertised = room_state
            .senders
            .values()
            .filter(|entry| entry.advertised_authenticated)
            .count();
        assert_eq!(u64::try_from(advertised).unwrap(), ROUNDS);
        assert_eq!(
            reopened
                .accept_authenticated_seq(&room, &sender, ROUNDS)
                .unwrap(),
            SeqCheck::Replayed {
                highest_seq: ROUNDS
            }
        );
    }

    /// The lock must exclude a second handle on the same file (as a second
    /// process would hold), and release it on drop. Checked with std's
    /// `try_lock`, which is `flock`/`LockFileEx` on the test platforms, so this
    /// also proves the lock interoperates with std's file locks.
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "Tests run on desktop targets, where std's file locks work."
    )]
    fn replay_lock_excludes_other_handles_until_released() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let path = store.paths.replay_state_lock_file();
        let held = ReplayStateLock::acquire(&path).unwrap();
        let other = File::open(&path).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        other.try_lock().unwrap();
        other.unlock().unwrap();
        // The store's own updates still go through once the handle is free.
        store.next_send_seq(&RoomId::from("room"), 1).unwrap();
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
        for path in [
            store.paths.replay_state_file(),
            store.paths.replay_state_lock_file(),
        ] {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        }
        // A lock file left with wider permissions is tightened on next use.
        std::fs::set_permissions(
            store.paths.replay_state_lock_file(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        store.next_send_seq(&RoomId::from("room"), 1).unwrap();
        let mode = std::fs::metadata(store.paths.replay_state_lock_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
