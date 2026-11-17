// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FileManifest, TransferId, CHUNK_SIZE, FILE_MAX_COUNT, TRANSFER_MAX_BYTES};

use crate::sanitize::{safe_filename, safe_transfer_id};
use crate::{Result, TransferError};

pub struct StagingArea {
    root: PathBuf,
}

impl StagingArea {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        restrict(&root, PRIVATE_DIR_MODE);
        Ok(Self { root })
    }

    pub fn inbox(&self) -> &Path {
        &self.root
    }

    pub fn begin(&self, transfer_id: &TransferId, chunk_count: u32) -> IncomingTransfer {
        IncomingTransfer {
            transfer_id: transfer_id.clone(),
            chunk_count,
            started: std::time::Instant::now(),
            chunks: BTreeMap::new(),
            bytes: 0,
            failed: false,
        }
    }

    /// Validate `files` against `payload` and publish exactly those files as
    /// `<inbox>/<transfer_id>/`.
    ///
    /// Several local receivers (for example the daemon and `clipsync pull
    /// --wait` connected for the same device) can receive the same transfer
    /// and commit it to the same inbox at the same time. Each call therefore
    /// stages into its own uniquely named partial directory and publishes it
    /// with one directory rename, so the published directory always holds one
    /// complete manifest and nothing else. A directory that already holds
    /// exactly this manifest (another receiver's commit, which its caller may
    /// be reading) is reused untouched; any other directory is a stale commit
    /// of a retried transfer and is swapped out whole.
    ///
    /// The filesystem work runs to completion on the blocking pool even if
    /// this future is dropped, and the partial directory is removed on every
    /// exit path, so cancelling a receive never strands staged bytes.
    pub async fn commit_files(
        &self,
        transfer_id: &TransferId,
        files: &[FileManifest],
        payload: impl Into<Vec<u8>>,
    ) -> Result<Vec<PathBuf>> {
        let payload = payload.into();
        if files.len() > FILE_MAX_COUNT || payload.len() as u64 > TRANSFER_MAX_BYTES {
            return Err(TransferError::Integrity);
        }
        let id = safe_transfer_id(transfer_id.as_str())?;
        let expected = files.iter().map(|f| f.size as usize).sum::<usize>();
        if payload.len() != expected {
            return Err(TransferError::Integrity);
        }
        let root = self.root.clone();
        let files = files.to_vec();
        tokio::task::spawn_blocking(move || commit_blocking(&root, &id, &files, &payload))
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))?
    }
}

/// A partial directory may be swept once it is this much older than the
/// transfer idle limit. A live commit keeps its partial directory only while
/// it writes a payload it already holds in memory (at most
/// `TRANSFER_MAX_BYTES`), which takes seconds; sixty idle periods (one hour)
/// leaves a wide margin for slow disks, so the sweep never removes the
/// directory of a receiver that is still running.
const STALE_STAGING_IDLE_MULTIPLE: u64 = 60;
const STALE_STAGING_AGE: Duration =
    Duration::from_secs(clipsync_protocol::TRANSFER_IDLE_SECS * STALE_STAGING_IDLE_MULTIPLE);

/// Leftover directories removed per commit. Each commit can strand at most
/// two (its partial and a retired stale commit, only if the process dies),
/// so removing more than two per commit drains leftovers faster than they
/// can accumulate while keeping each commit's extra work bounded.
const STALE_STAGING_REMOVALS_PER_COMMIT: usize = 4;

/// Publication attempts before giving up. Each retry means another local
/// receiver changed the destination between inspection and rename; a device
/// runs only a few receivers at once (daemon, `pull`), so a handful of
/// attempts always converges on one complete commit.
const PUBLISH_ATTEMPTS: usize = 4;

/// Suffix of a directory a commit is still writing.
const PARTIAL_SUFFIX: &str = ".partial";
/// Suffix of a stale commit that was swapped out and is being removed.
const RETIRED_SUFFIX: &str = ".retired";

/// Removes a staging directory owned by one commit when the commit ends,
/// whether it returns early, fails, or unwinds from a panic.
struct OwnedDir {
    path: PathBuf,
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        // After a successful publish the directory no longer exists here, and
        // its unique name means nothing else can have taken its place.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A fresh, unique dot-prefixed name for this commit's scratch directories.
/// The process id names the owner for diagnostics and a fresh ULID separates
/// concurrent calls inside one process. Transfer ids are ULID-alphabet only,
/// so these names can never collide with a published `<transfer_id>`.
fn scratch_name(id: &str, suffix: &str) -> String {
    format!(
        ".{id}.{}.{}{suffix}",
        std::process::id(),
        ulid::Ulid::generate()
    )
}

fn commit_blocking(
    root: &Path,
    id: &str,
    files: &[FileManifest],
    payload: &[u8],
) -> Result<Vec<PathBuf>> {
    sweep_stale_staging(root, SystemTime::now(), STALE_STAGING_AGE);
    let names = validated_names(files, payload)?;
    let dest_dir = root.join(id);
    let staged = OwnedDir {
        path: root.join(scratch_name(id, PARTIAL_SUFFIX)),
    };
    // `create_dir`, not `create_dir_all`: an existing entry is never
    // adopted, so this call only ever writes into a directory it owns.
    std::fs::create_dir(&staged.path)?;
    restrict(&staged.path, PRIVATE_DIR_MODE);
    let mut offset = 0usize;
    for (name, file) in names.iter().zip(files) {
        let end = offset + file.size as usize;
        let target = staged.path.join(name);
        std::fs::write(&target, &payload[offset..end])?;
        restrict(&target, PRIVATE_FILE_MODE);
        offset = end;
    }
    publish(root, id, &staged.path, &dest_dir, &names, files)?;
    Ok(names.into_iter().map(|name| dest_dir.join(name)).collect())
}

/// Check every name and hash before anything touches the filesystem.
fn validated_names(files: &[FileManifest], payload: &[u8]) -> Result<Vec<String>> {
    let mut names = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::new();
    let mut offset = 0usize;
    for file in files {
        let name = safe_filename(&file.name)?;
        if !seen.insert(name.clone()) {
            return Err(TransferError::UnsafeName(name));
        }
        let end = offset + file.size as usize;
        if end > payload.len() || blake3_hex(&payload[offset..end]) != file.hash {
            return Err(TransferError::Integrity);
        }
        names.push(name);
        offset = end;
    }
    Ok(names)
}

/// Make `staged` visible as `dest_dir`, or reuse an identical commit there.
fn publish(
    root: &Path,
    id: &str,
    staged: &Path,
    dest_dir: &Path,
    names: &[String],
    files: &[FileManifest],
) -> Result<()> {
    for _ in 0..PUBLISH_ATTEMPTS {
        match std::fs::symlink_metadata(dest_dir) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // A rename onto a directory another receiver published in the
                // meantime fails; inspect the destination again.
                if std::fs::rename(staged, dest_dir).is_ok() {
                    return Ok(());
                }
            }
            Err(err) => return Err(err.into()),
            // Check the type explicitly instead of relying on rename: Windows
            // lets a directory rename replace a file. A file or symlink here
            // is an error so placement is never redirected or destroyed.
            Ok(meta) if !meta.is_dir() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "transfer destination is not a directory",
                )
                .into());
            }
            Ok(_) if holds_exactly(dest_dir, names, files) => return Ok(()),
            Ok(_) => {
                // A stale commit of a retried transfer: move it aside whole
                // so the published directory never mixes two manifests.
                let retired = OwnedDir {
                    path: root.join(scratch_name(id, RETIRED_SUFFIX)),
                };
                match std::fs::rename(dest_dir, &retired.path) {
                    Ok(()) => {
                        if std::fs::rename(staged, dest_dir).is_ok() {
                            return Ok(());
                        }
                    }
                    // Another receiver moved it first; inspect again.
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
            }
        }
    }
    Err(std::io::Error::other("transfer destination kept changing during publication").into())
}

/// True when `dir` contains exactly the manifest's files, each a regular file
/// whose size and BLAKE3 hash match.
fn holds_exactly(dir: &Path, names: &[String], files: &[FileManifest]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    if entries.count() != names.len() {
        return false;
    }
    names.iter().zip(files).all(|(name, file)| {
        let target = dir.join(name);
        std::fs::symlink_metadata(&target)
            .is_ok_and(|meta| meta.is_file() && meta.len() == file.size)
            && std::fs::read(&target).is_ok_and(|bytes| blake3_hex(&bytes) == file.hash)
    })
}

/// Remove up to `STALE_STAGING_REMOVALS_PER_COMMIT` partial or retired
/// directories last modified more than `max_age` before `now`, left behind by
/// a receiver process that exited mid-commit. Published transfers, files,
/// symlinks and fresh directories are never touched.
fn sweep_stale_staging(root: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if removed == STALE_STAGING_REMOVALS_PER_COMMIT {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with('.')
            || !(name.ends_with(PARTIAL_SUFFIX) || name.ends_with(RETIRED_SUFFIX))
        {
            continue;
        }
        let path = entry.path();
        let is_stale_dir = std::fs::symlink_metadata(&path).is_ok_and(|meta| {
            meta.is_dir()
                && meta.modified().is_ok_and(|modified| {
                    now.duration_since(modified).unwrap_or_default() > max_age
                })
        });
        if is_stale_dir && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Owner-only permissions for staged directories and files on Unix.
const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

pub struct IncomingTransfer {
    pub transfer_id: TransferId,
    pub chunk_count: u32,
    pub started: std::time::Instant,
    chunks: BTreeMap<u32, Vec<u8>>,
    bytes: usize,
    failed: bool,
}

impl IncomingTransfer {
    pub fn push(&mut self, index: u32, count: u32, data: Vec<u8>) -> Result<()> {
        if self.failed {
            return Err(TransferError::Chunk("transfer already failed"));
        }
        if count != self.chunk_count {
            self.failed = true;
            return Err(TransferError::Chunk("chunk count mismatch"));
        }
        if count > 1024 || index >= count {
            self.failed = true;
            return Err(TransferError::Chunk("chunk index out of range"));
        }
        if data.len() > CHUNK_SIZE || self.bytes + data.len() > TRANSFER_MAX_BYTES as usize {
            self.failed = true;
            return Err(TransferError::Chunk("transfer too large"));
        }
        if self.chunks.contains_key(&index) {
            self.failed = true;
            return Err(TransferError::Chunk("duplicate chunk"));
        }
        self.bytes += data.len();
        self.chunks.insert(index, data);
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        !self.failed && self.chunks.len() as u32 == self.chunk_count
    }

    pub fn assemble(self, expected_hash: &str) -> Result<Vec<u8>> {
        if self.failed || self.chunks.len() as u32 != self.chunk_count {
            return Err(TransferError::Chunk("incomplete transfer"));
        }
        // sequential 0..count required
        let mut out = Vec::new();
        for i in 0..self.chunk_count {
            let chunk = self
                .chunks
                .get(&i)
                .ok_or(TransferError::Chunk("missing chunk"))?;
            out.extend_from_slice(chunk);
        }
        if blake3_hex(&out) != expected_hash {
            return Err(TransferError::Integrity);
        }
        Ok(out)
    }

    pub fn fail(&mut self) {
        self.failed = true;
        self.chunks.clear();
    }

    pub fn is_idle(&self, now: std::time::Instant, ttl: std::time::Duration) -> bool {
        now.saturating_duration_since(self.started) > ttl
    }
}

/// Drop unfinished transfers that have been sitting longer than `ttl`.
pub fn prune_idle_transfers(
    incoming: &mut std::collections::HashMap<String, IncomingTransfer>,
    now: std::time::Instant,
    ttl: std::time::Duration,
) -> usize {
    let before = incoming.len();
    incoming.retain(|_, t| !t.is_idle(now, ttl) && !t.failed);
    while incoming.len() > 32 {
        let oldest = incoming
            .iter()
            .min_by_key(|(_, t)| t.started)
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => {
                incoming.remove(&id);
            }
            None => break,
        }
    }
    before.saturating_sub(incoming.len())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn stability_idle_transfers_are_pruned() {
        let mut map = std::collections::HashMap::new();
        let id = TransferId("old".into());
        let started = Instant::now();
        let t = IncomingTransfer {
            transfer_id: id.clone(),
            chunk_count: 2,
            started,
            chunks: BTreeMap::new(),
            bytes: 0,
            failed: false,
        };
        map.insert(id.0.clone(), t);
        let n = prune_idle_transfers(
            &mut map,
            started + Duration::from_secs(120),
            Duration::from_secs(60),
        );
        assert_eq!(n, 1);
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn transfer_id_cannot_escape_inbox() {
        let root = std::env::temp_dir().join(format!("clipsync-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let staging = StagingArea::new(root.clone()).unwrap();
        let payload = b"x";
        let files = vec![clipsync_protocol::FileManifest {
            name: "a.txt".into(),
            size: 1,
            mime: None,
            hash: clipsync_crypto::blake3_hex(payload),
        }];
        let err = staging
            .commit_files(&TransferId::from("../escape"), &files, payload)
            .await;
        assert!(err.is_err());
        assert!(!root.join("..").join("escape").join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn incomplete_and_poisoned_transfers_never_assemble_and_are_pruned() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let id = TransferId::new();
        let mut incomplete = staging.begin(&id, 2);
        incomplete.push(1, 2, b"second".to_vec()).unwrap();
        assert!(!incomplete.is_complete());
        assert!(matches!(
            incomplete.assemble("unused"),
            Err(TransferError::Chunk("incomplete transfer"))
        ));
        let mut poisoned = staging.begin(&id, 1);
        poisoned.push(0, 1, b"first".to_vec()).unwrap();
        assert!(matches!(
            poisoned.push(0, 1, vec![]),
            Err(TransferError::Chunk("duplicate chunk"))
        ));
        assert!(matches!(
            poisoned.push(0, 1, vec![]),
            Err(TransferError::Chunk("transfer already failed"))
        ));
        poisoned.fail();
        assert!(!poisoned.is_complete());
        let mut incoming = std::collections::HashMap::from([(id.to_string(), poisoned)]);
        assert_eq!(
            prune_idle_transfers(&mut incoming, Instant::now(), Duration::from_secs(60)),
            1
        );
        assert!(incoming.is_empty());
        let mut wrong_index = staging.begin(&id, 1);
        assert!(matches!(
            wrong_index.push(1, 1, vec![]),
            Err(TransferError::Chunk("chunk index out of range"))
        ));
    }

    #[tokio::test]
    async fn retries_ignore_foreign_partials_and_replace_previous_commit_only_after_hash_validation(
    ) {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let id = TransferId::new();
        let partial = staging.inbox().join(format!(".{id}.partial"));
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("stale.txt"), b"discard").unwrap();
        let file = |payload: &[u8]| clipsync_protocol::FileManifest {
            name: "report.txt".into(),
            size: payload.len() as u64,
            mime: Some("text/plain".into()),
            hash: blake3_hex(payload),
        };
        let first = staging
            .commit_files(&id, &[file(b"original")], b"original")
            .await
            .unwrap();
        assert_eq!(std::fs::read(&first[0]).unwrap(), b"original");
        // Another receiver may still be writing there: never remove or adopt it.
        assert_eq!(
            std::fs::read(partial.join("stale.txt")).unwrap(),
            b"discard"
        );
        assert!(!staging.inbox().join(id.as_str()).join("stale.txt").exists());
        assert!(staging
            .commit_files(&id, &[file(b"different length")], b"small")
            .await
            .is_err());
        assert_eq!(std::fs::read(&first[0]).unwrap(), b"original");
        let second = staging
            .commit_files(&id, &[file(b"replacement")], b"replacement")
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&second[0]).unwrap(), b"replacement");
    }

    #[tokio::test]
    async fn foreign_partial_markers_blocked_destinations_and_unsafe_names_leave_existing_data_intact(
    ) {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let file = |name: &str| clipsync_protocol::FileManifest {
            name: name.into(),
            size: 7,
            mime: None,
            hash: blake3_hex(b"payload"),
        };
        let blocked_partial = TransferId::new();
        let partial = staging.inbox().join(format!(".{blocked_partial}.partial"));
        std::fs::write(&partial, b"existing partial marker").unwrap();
        let committed = staging
            .commit_files(&blocked_partial, &[file("report.txt")], b"payload")
            .await
            .unwrap();
        assert_eq!(std::fs::read(&committed[0]).unwrap(), b"payload");
        assert_eq!(std::fs::read(partial).unwrap(), b"existing partial marker");
        let blocked_destination = TransferId::new();
        let destination = staging.inbox().join(blocked_destination.as_str());
        std::fs::write(&destination, b"existing destination marker").unwrap();
        assert!(staging
            .commit_files(&blocked_destination, &[file("report.txt")], b"payload")
            .await
            .is_err());
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"existing destination marker"
        );
        // The validated bytes staged for the blocked destination were removed
        // with this call's own partial directory instead of lingering.
        let leftover_partials = std::fs::read_dir(staging.inbox())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&format!(".{blocked_destination}.")))
            .count();
        assert_eq!(leftover_partials, 0);
        let outside = staging.inbox().join("outside.txt");
        std::fs::write(&outside, b"must remain private").unwrap();
        assert!(matches!(
            staging
                .commit_files(&TransferId::new(), &[file("../outside.txt")], b"payload")
                .await,
            Err(TransferError::UnsafeName(_))
        ));
        assert_eq!(std::fs::read(outside).unwrap(), b"must remain private");
        let file_root = root.path().join("file-root");
        std::fs::write(&file_root, b"file").unwrap();
        assert!(StagingArea::new(file_root).is_err());
    }

    #[test]
    fn owned_staging_directory_is_removed_with_its_contents_when_dropped() {
        let root = tempfile::tempdir().unwrap();
        let path = root
            .path()
            .join(scratch_name(TransferId::new().as_str(), PARTIAL_SUFFIX));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("staged.txt"), b"staged").unwrap();
        drop(OwnedDir { path: path.clone() });
        assert!(!path.exists());
    }

    #[test]
    fn stale_staging_sweep_is_bounded_and_spares_live_and_published_entries() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path();
        let id = TransferId::new();
        let leftovers: Vec<_> = (0..STALE_STAGING_REMOVALS_PER_COMMIT)
            .map(|index| {
                let suffix = if index % 2 == 0 {
                    PARTIAL_SUFFIX
                } else {
                    RETIRED_SUFFIX
                };
                let path = inbox.join(scratch_name(id.as_str(), suffix));
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("staged.txt"), b"staged").unwrap();
                path
            })
            .collect();
        let published = inbox.join(id.as_str());
        std::fs::create_dir(&published).unwrap();
        let partial_marker_file = inbox.join(format!(".{id}{PARTIAL_SUFFIX}"));
        std::fs::write(&partial_marker_file, b"not a directory").unwrap();
        let unrelated = inbox.join(".unrelated");
        std::fs::create_dir(&unrelated).unwrap();

        // Every entry was just written, so a sweep at the real time keeps
        // them all: a receiver may still be writing into a fresh partial.
        assert_eq!(
            sweep_stale_staging(inbox, SystemTime::now(), STALE_STAGING_AGE),
            0
        );
        assert!(leftovers.iter().all(|path| path.exists()));

        let extra = inbox.join(scratch_name(id.as_str(), PARTIAL_SUFFIX));
        std::fs::create_dir(&extra).unwrap();
        let later = SystemTime::now() + STALE_STAGING_AGE * 2;
        assert_eq!(
            sweep_stale_staging(inbox, later, STALE_STAGING_AGE),
            STALE_STAGING_REMOVALS_PER_COMMIT
        );
        assert_eq!(sweep_stale_staging(inbox, later, STALE_STAGING_AGE), 1);
        assert!(leftovers.iter().chain([&extra]).all(|path| !path.exists()));
        assert!(published.is_dir());
        assert_eq!(
            std::fs::read(&partial_marker_file).unwrap(),
            b"not a directory"
        );
        assert!(unrelated.is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_only_inbox_fails_without_publishing_or_staging_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let id = TransferId::new();
        std::fs::set_permissions(staging.inbox(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = staging
            .commit_files(
                &id,
                &[clipsync_protocol::FileManifest {
                    name: "report.txt".into(),
                    size: 7,
                    mime: None,
                    hash: blake3_hex(b"payload"),
                }],
                b"payload",
            )
            .await;
        // Restore fixture permissions before assertions or cleanup.
        std::fs::set_permissions(staging.inbox(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(TransferError::Io(_))));
        assert_eq!(std::fs::read_dir(staging.inbox()).unwrap().count(), 0);
    }
}
