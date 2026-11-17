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
    /// be reading) is reused with its contents untouched; any other directory
    /// is a stale commit of a retried transfer and is swapped out whole.
    ///
    /// On macOS every staged file is quarantined in the private partial
    /// directory before any bytes are written, so a published file never
    /// lacks Gatekeeper's first-open check; a reused directory gets the
    /// attribute on any file that lacks it, as does a stale commit put back
    /// after its replacement failed (before it is renamed back into place).
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
        // Checked: sizes come from the sender, and a wrapped sum must not
        // pass this check or panic.
        let expected = files
            .iter()
            .try_fold(0u64, |total, f| total.checked_add(f.size));
        if expected != Some(payload.len() as u64) {
            return Err(TransferError::Integrity);
        }
        let root = self.root.clone();
        let files = files.to_vec();
        tokio::task::spawn_blocking(move || commit_blocking(&root, &id, &files, &payload))
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))?
    }
}

/// Age after which a partial or retired directory counts as abandoned. A live
/// commit keeps its partial directory only while it writes a payload it
/// already holds in memory (at most `TRANSFER_MAX_BYTES`), which takes
/// seconds. Sixty transfer idle periods (one hour) leave a wide margin for
/// slow disks, so the sweep never removes a running receiver's partial.
///
/// Age is the partial's mtime against the wall clock, so a forward clock
/// jump of more than this (an NTP correction on a device whose clock was
/// wrong) can remove a live partial. That receiver's commit then fails with
/// an I/O error and nothing is published or corrupted. Also requiring the
/// owner's process (named in the partial) to be dead was rejected: process
/// ids are reused, and a reused id would keep a leftover forever.
const STALE_STAGING_IDLE_MULTIPLE: u64 = 60;
const STALE_STAGING_AGE: Duration =
    Duration::from_secs(clipsync_protocol::TRANSFER_IDLE_SECS * STALE_STAGING_IDLE_MULTIPLE);

/// Leftover directories removed or restored per commit. Each commit can
/// strand at most two (its partial and a retired stale commit, only if the
/// process dies), so handling more than two per commit drains leftovers
/// faster than they can accumulate while keeping each commit's extra work
/// bounded.
const STALE_STAGING_REMOVALS_PER_COMMIT: usize = 4;

/// Suffix of a directory a commit is still writing.
const PARTIAL_SUFFIX: &str = ".partial";
/// Suffix of a stale commit that was swapped out and is being removed.
const RETIRED_SUFFIX: &str = ".retired";
/// Inbox-wide lock file that serialises publication across local receivers.
/// One persistent file, never removed: removing a lock file while another
/// process waits on it would let two holders lock different files.
const PUBLISH_LOCK_NAME: &str = ".publish.lock";

/// Removes a staging directory owned by one commit when the commit ends,
/// whether it returns early, fails, or unwinds from a panic.
struct OwnedDir {
    path: PathBuf,
}

impl OwnedDir {
    /// Leave the directory in place, for the next sweep to recover.
    fn keep(mut self) {
        self.path = PathBuf::new();
    }
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
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
    let names = validated_names(files, payload)?;
    // Reclaim abandoned staging before writing anything new, so leftovers
    // that filled the disk cannot make every later commit fail before its
    // cleanup runs. A lock that cannot be taken fails the commit: every
    // publication needs it, and nothing here runs unlocked. The lock is
    // released while staging.
    {
        let _lock = publish_lock(root)?;
        sweep_stale_staging(root, SystemTime::now(), STALE_STAGING_AGE);
    }
    let dest_dir = root.join(id);
    let mark = ReceiveMark::new();
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
        write_new_file(&staged.path.join(name), &payload[offset..end], &mark).map_err(|err| {
            // The directory is fresh and this commit's names are distinct
            // strings, so an existing entry means the filesystem treats this
            // name as one written before (Unicode normalization on APFS and
            // HFS+, or case folding beyond `to_lowercase`).
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                TransferError::UnsafeName(name.clone())
            } else {
                err.into()
            }
        })?;
        offset = end;
    }
    let retired = {
        // Released when `_lock` is dropped at the end of this block, before
        // the retired directory is removed outside the critical section.
        let _lock = publish_lock(root)?;
        publish(root, id, &staged.path, &dest_dir, &names, files, &mark)?
    };
    drop(retired);
    Ok(names.into_iter().map(|name| dest_dir.join(name)).collect())
}

/// Create `path`, which must not exist yet, owner-only, holding `contents`.
/// `create_new` fails with `AlreadyExists` when the filesystem resolves the
/// name to an entry this commit already wrote, instead of overwriting it.
/// The file is quarantined through its descriptor before any bytes land,
/// while it is still inside the private partial directory.
fn write_new_file(path: &Path, contents: &[u8], mark: &ReceiveMark) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
    }
    let mut file = options.open(path)?;
    mark.apply(&file);
    file.write_all(contents)
}

/// One receive event's Gatekeeper quarantine on macOS (a no-op elsewhere).
/// Every file a commit stages shares one value, as one browser download
/// does. See `quarantine::mark_or_warn` for why a failure still delivers.
struct ReceiveMark {
    #[cfg(target_os = "macos")]
    value: String,
}

impl ReceiveMark {
    fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            value: crate::quarantine::quarantine_value(),
        }
    }

    /// Quarantine a file this commit just created.
    fn apply(&self, file: &std::fs::File) {
        #[cfg(target_os = "macos")]
        crate::quarantine::mark_file_or_warn(std::os::fd::AsFd::as_fd(file), &self.value);
        #[cfg(not(target_os = "macos"))]
        let _ = file;
    }

    /// Quarantine the regular files in a transfer directory this commit did
    /// not write (a published one it reuses, or a retired one that it or the
    /// sweep is about to restore) that lack the attribute, for example
    /// because an older version wrote them. An existing value is kept: it may
    /// record the user's approval. The caller holds the publish lock, and
    /// symlinks are neither followed nor marked.
    fn backfill(&self, dir: &Path) {
        #[cfg(test)]
        BACKFILL_OBSERVER.with_borrow_mut(|observer| {
            if let Some(observer) = observer {
                observer(dir);
            }
        });
        #[cfg(target_os = "macos")]
        {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(%error, "could not list received files to quarantine");
                    return;
                }
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    crate::quarantine::mark_if_absent_or_warn(&entry.path(), &self.value);
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = dir;
    }
}

#[cfg(test)]
type BackfillObserver = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    /// Test-only callback invoked with each directory `ReceiveMark::backfill`
    /// is about to quarantine, so a test can see where and when backfill
    /// runs. Thread-local: it observes only calls on the installing thread.
    static BACKFILL_OBSERVER: std::cell::RefCell<Option<BackfillObserver>> =
        const { std::cell::RefCell::new(None) };
}

/// Exclusive lock on the inbox publish lock file, released on drop. Every
/// inspection or rename of a published transfer or retired directory happens
/// while holding it, so the sweep never sees a live publisher between its two
/// renames.
///
/// It is `flock` on Unix (Android included) and `LockFileEx` on Windows: it
/// excludes other processes and other handles in this process alike, and the
/// OS releases it if the process dies. Failing to take it is an error; there
/// is no in-process-only fallback.
struct PublishLock(std::fs::File);

impl Drop for PublishLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock too, but Windows may do that
        // lazily; unlock explicitly so the next receiver is not delayed.
        let _ = unlock(&self.0);
    }
}

fn publish_lock(root: &Path) -> std::io::Result<PublishLock> {
    let path = root.join(PUBLISH_LOCK_NAME);
    let mut options = std::fs::OpenOptions::new();
    // Never truncate: the file has no content, only a lock.
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A planted `.publish.lock` symlink fails the open instead of being
        // followed, so the lock (and the chmod below) never act on its target.
        options
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(O_NOFOLLOW_FLAG);
    }
    let file = options.open(&path)?;
    // Also tighten a lock file left by an older version. Through the handle,
    // never by path, so the chmod cannot be redirected.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE));
    }
    lock_exclusive(&file)?;
    Ok(PublishLock(file))
}

/// `O_NOFOLLOW` as an `open(2)` flag for `OpenOptionsExt::custom_flags`.
#[cfg(unix)]
const O_NOFOLLOW_FLAG: i32 = rustix::fs::OFlags::NOFOLLOW.bits() as i32;

/// Block until `file` holds an exclusive `flock`.
///
/// Not `File::lock`: before Rust 1.98 std returns `Unsupported` for it on
/// Android, and the workspace MSRV is lower. std uses the same `flock` call on
/// the other Unix targets, so the semantics are unchanged.
#[cfg(unix)]
fn lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
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
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock).map_err(std::io::Error::from)
}

/// `LockFileEx` on Windows. Any other target gets std's `Unsupported` error,
/// which fails the commit instead of publishing unlocked.
#[cfg(not(unix))]
#[allow(
    clippy::disallowed_methods,
    reason = "Not Unix, so not Android; std's lock is LockFileEx on Windows."
)]
fn lock_exclusive(file: &std::fs::File) -> std::io::Result<()> {
    file.lock()
}

#[cfg(not(unix))]
#[allow(
    clippy::disallowed_methods,
    reason = "Not Unix, so not Android; std's unlock is UnlockFile on Windows."
)]
fn unlock(file: &std::fs::File) -> std::io::Result<()> {
    file.unlock()
}

/// Check every name and hash before anything touches the filesystem.
fn validated_names(files: &[FileManifest], payload: &[u8]) -> Result<Vec<String>> {
    let mut names = Vec::with_capacity(files.len());
    let mut seen = std::collections::HashSet::new();
    let mut offset = 0usize;
    for file in files {
        let name = safe_filename(&file.name)?;
        // Case-folded: on case-insensitive filesystems (macOS, Windows) two
        // names that differ only by case would land on one file. Rejected on
        // every platform so a transfer behaves the same wherever it lands.
        // Other spellings a filesystem treats as one name (Unicode
        // normalization on APFS) are caught when staging writes them.
        if !seen.insert(name.to_lowercase()) {
            return Err(TransferError::UnsafeName(name));
        }
        let end = usize::try_from(file.size)
            .ok()
            .and_then(|size| offset.checked_add(size))
            .filter(|end| *end <= payload.len())
            .ok_or(TransferError::Integrity)?;
        if blake3_hex(&payload[offset..end]) != file.hash {
            return Err(TransferError::Integrity);
        }
        names.push(name);
        offset = end;
    }
    Ok(names)
}

/// Make `staged` visible as `dest_dir`, or reuse an identical commit there.
/// The caller holds the inbox publish lock, so no other local receiver can
/// change `dest_dir` between this inspection and the renames. Returns the
/// stale commit that was moved aside, if any, for removal after the lock.
fn publish(
    root: &Path,
    id: &str,
    staged: &Path,
    dest_dir: &Path,
    names: &[String],
    files: &[FileManifest],
    mark: &ReceiveMark,
) -> Result<Option<OwnedDir>> {
    match std::fs::symlink_metadata(dest_dir) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(staged, dest_dir)?;
            Ok(None)
        }
        Err(err) => Err(err.into()),
        // Check the type explicitly instead of relying on rename: Windows
        // lets a directory rename replace a file. A file or symlink here is
        // an error so placement is never redirected or destroyed.
        Ok(meta) if !meta.is_dir() => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "transfer destination is not a directory",
        )
        .into()),
        // Another receiver already published this exact manifest, and its
        // caller may be reading it: keep its contents untouched, only adding
        // quarantine to files that lack it.
        Ok(_) if holds_exactly(dest_dir, names, files) => {
            mark.backfill(dest_dir);
            Ok(None)
        }
        Ok(_) => {
            // A stale commit of a retried transfer: move it aside whole so
            // the published directory never mixes two manifests.
            let retired = OwnedDir {
                path: root.join(scratch_name(id, RETIRED_SUFFIX)),
            };
            std::fs::rename(dest_dir, &retired.path)?;
            if let Err(err) = std::fs::rename(staged, dest_dir) {
                // Put the previous commit back rather than leave none. Mark
                // it first, while it is still the private retired directory,
                // so its files are never published unquarantined. If the
                // rename back fails, keep it: the next sweep restores it.
                mark.backfill(&retired.path);
                if std::fs::rename(&retired.path, dest_dir).is_err() {
                    retired.keep();
                }
                return Err(err.into());
            }
            Ok(Some(retired))
        }
    }
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
            && file_hash(&target).is_ok_and(|hash| hash == file.hash)
    })
}

/// Streaming BLAKE3 hex digest, so checking an existing commit never holds a
/// second copy of a large file in memory.
fn file_hash(path: &Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Clean up after receivers that stopped mid-commit, handling at most
/// `STALE_STAGING_REMOVALS_PER_COMMIT` directories. The caller holds the
/// publish lock, so no live publisher is between its two renames. Commits
/// run this before staging, so leftovers that fill the disk are reclaimed
/// before the next commit needs the space.
///
/// - A partial directory last modified more than `max_age` before `now` is
///   removed; a fresher one may belong to a receiver still writing it.
/// - A retired directory whose transfer is published again is leftover
///   garbage and is removed (its owner may be removing it too).
/// - A retired directory whose transfer is missing is a replacement
///   interrupted between its renames: it is the last committed copy, so it
///   is restored as the published transfer instead of being deleted, and on
///   macOS any of its files that lacks quarantine gets it.
///
/// Published transfers, files, symlinks and other entries are never touched.
fn sweep_stale_staging(root: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut handled = 0;
    for entry in entries.flatten() {
        if handled == STALE_STAGING_REMOVALS_PER_COMMIT {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(scratch) = name.strip_prefix('.') else {
            continue;
        };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let done = if scratch.ends_with(PARTIAL_SUFFIX) {
            let stale = meta
                .modified()
                .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() > max_age);
            stale && std::fs::remove_dir_all(&path).is_ok()
        } else if scratch.ends_with(RETIRED_SUFFIX) {
            let Some(id) = scratch
                .split('.')
                .next()
                .and_then(|id| safe_transfer_id(id).ok())
            else {
                continue;
            };
            let dest = root.join(id);
            match std::fs::symlink_metadata(&dest) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    // Mark while the directory is still private, so a crash
                    // between the two steps never publishes unmarked files.
                    // Its own receive event, not the running commit's.
                    ReceiveMark::new().backfill(&path);
                    std::fs::rename(&path, &dest).is_ok()
                }
                Ok(_) => std::fs::remove_dir_all(&path).is_ok(),
                Err(_) => false,
            }
        } else {
            false
        };
        if done {
            handled += 1;
        }
    }
    handled
}

/// Owner-only permissions for staged directories and files on Unix.
const PRIVATE_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
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

    /// How long the lock test waits to show a commit stays blocked; a
    /// small unblocked commit publishes in milliseconds.
    const BLOCKED_PUBLISH_WINDOW: Duration = Duration::from_millis(300);

    /// Inspecting the destination and replacing it must be one step with
    /// respect to every other local receiver: otherwise a receiver that saw
    /// a stale commit could retire another receiver's fresh copy of the same
    /// manifest after that receiver returned it. Publication therefore waits
    /// for the inbox publish lock, which another process may hold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publication_waits_for_the_inbox_publish_lock() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("inbox");
        let staging = StagingArea::new(inbox.clone()).unwrap();
        let held = publish_lock(&inbox).unwrap();
        let id = TransferId::new();
        let commit = tokio::spawn({
            let id = id.clone();
            async move {
                let manifest = clipsync_protocol::FileManifest {
                    name: "report.txt".into(),
                    size: 7,
                    mime: None,
                    hash: blake3_hex(b"payload"),
                };
                staging.commit_files(&id, &[manifest], b"payload").await
            }
        });
        tokio::time::sleep(BLOCKED_PUBLISH_WINDOW).await;
        assert!(!commit.is_finished());
        assert!(!inbox.join(id.as_str()).exists());
        drop(held);
        let paths = commit.await.unwrap().unwrap();
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"payload");
    }

    /// A process that dies between retiring a stale commit and renaming its
    /// replacement in leaves the transfer only in the retired directory. The
    /// next commit must restore it, not sweep it away, and a retired
    /// directory whose transfer is published again is plain garbage.
    #[tokio::test]
    async fn an_interrupted_replacement_is_restored_and_finished_ones_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let manifest = |name: &str| clipsync_protocol::FileManifest {
            name: name.into(),
            size: 7,
            mime: None,
            hash: blake3_hex(b"payload"),
        };
        let interrupted = TransferId::new();
        staging
            .commit_files(&interrupted, &[manifest("kept.txt")], b"payload")
            .await
            .unwrap();
        let published = staging.inbox().join(interrupted.as_str());
        // Simulate the crash: the first rename of a replacement happened,
        // the second never did.
        let orphan = staging
            .inbox()
            .join(scratch_name(interrupted.as_str(), RETIRED_SUFFIX));
        std::fs::rename(&published, &orphan).unwrap();

        let finished = TransferId::new();
        staging
            .commit_files(&finished, &[manifest("a.txt")], b"payload")
            .await
            .unwrap();
        let leftover = staging
            .inbox()
            .join(scratch_name(finished.as_str(), RETIRED_SUFFIX));
        std::fs::create_dir(&leftover).unwrap();

        // An unrelated commit runs the sweep.
        staging
            .commit_files(&TransferId::new(), &[manifest("b.txt")], b"payload")
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(published.join("kept.txt")).unwrap(),
            b"payload"
        );
        assert!(!orphan.exists());
        assert!(!leftover.exists());
        assert_eq!(
            std::fs::read(staging.inbox().join(finished.as_str()).join("a.txt")).unwrap(),
            b"payload"
        );
    }

    /// When a replacement retires a stale commit but cannot rename its own
    /// staging into place, the stale commit is put back. Files in it may lack
    /// quarantine (an older version wrote them), so it must be marked while
    /// it is still the private retired directory: marking after the rename
    /// back would leave unmarked files at the published path, if only for a
    /// moment. The staged path here does not exist, which makes exactly that
    /// second rename fail on every platform.
    #[test]
    fn a_failed_replacement_marks_the_previous_commit_before_restoring_it() {
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let inbox = staging.inbox().to_owned();
        let id = TransferId::new();
        let dest_dir = inbox.join(id.as_str());
        std::fs::create_dir(&dest_dir).unwrap();
        // Written without the attribute, as an older version did.
        std::fs::write(dest_dir.join("kept.command"), b"payload").unwrap();
        let missing_staging = inbox.join(scratch_name(id.as_str(), PARTIAL_SUFFIX));
        let names = vec!["new.txt".to_owned()];
        let files = vec![FileManifest {
            name: "new.txt".into(),
            size: b"new".len() as u64,
            mime: None,
            hash: blake3_hex(b"new"),
        }];

        // Each backfill target, and whether the published path existed then.
        let backfills = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        BACKFILL_OBSERVER.set(Some(Box::new({
            let backfills = std::rc::Rc::clone(&backfills);
            let dest_dir = dest_dir.clone();
            move |dir: &Path| {
                backfills
                    .borrow_mut()
                    .push((dir.to_owned(), dest_dir.exists()));
            }
        })));
        let result = {
            let _lock = publish_lock(&inbox).unwrap();
            publish(
                &inbox,
                id.as_str(),
                &missing_staging,
                &dest_dir,
                &names,
                &files,
                &ReceiveMark::new(),
            )
        };
        BACKFILL_OBSERVER.set(None);

        assert!(
            matches!(&result, Err(TransferError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound),
            "unexpected result {:?}",
            result.map(|retired| retired.map(|dir| dir.path.clone()))
        );
        let backfills = backfills.take();
        assert_eq!(backfills.len(), 1, "backfill calls: {backfills:?}");
        let (target, published_then) = &backfills[0];
        let target_name = target.file_name().unwrap().to_str().unwrap();
        assert_eq!(target.parent(), Some(inbox.as_path()));
        assert!(
            target_name.starts_with(&format!(".{id}.")) && target_name.ends_with(RETIRED_SUFFIX),
            "backfilled {} instead of the retired directory",
            target.display()
        );
        assert!(
            !published_then,
            "the previous commit was published again before it was marked"
        );
        // The previous commit is back in place and nothing else is left.
        assert_eq!(
            std::fs::read(dest_dir.join("kept.command")).unwrap(),
            b"payload"
        );
        assert_eq!(std::fs::read_dir(&dest_dir).unwrap().count(), 1);
        let leftovers = std::fs::read_dir(&inbox)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&format!(".{id}.")))
            .count();
        assert_eq!(leftovers, 0);
        #[cfg(target_os = "macos")]
        {
            let value = crate::quarantine::value_of(&dest_dir.join("kept.command"))
                .unwrap()
                .expect("restored file lacks com.apple.quarantine");
            assert_eq!(
                value.split(';').nth(2),
                Some(crate::quarantine::QUARANTINE_AGENT)
            );
        }
    }

    /// Cleanup must come before a commit writes its own staging: when
    /// abandoned staging has filled the disk, a sweep that only ran after
    /// staging would never be reached. The commit therefore sweeps under the
    /// publish lock before it creates its partial directory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_staging_is_reclaimed_before_a_commit_writes_its_own() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("inbox");
        let staging = StagingArea::new(inbox.clone()).unwrap();
        let orphaned = TransferId::new();
        let orphan = inbox.join(scratch_name(orphaned.as_str(), RETIRED_SUFFIX));
        std::fs::create_dir(&orphan).unwrap();
        std::fs::write(orphan.join("kept.txt"), b"kept").unwrap();
        let held = publish_lock(&inbox).unwrap();
        let id = TransferId::new();
        let commit = tokio::spawn({
            let id = id.clone();
            async move {
                let manifest = clipsync_protocol::FileManifest {
                    name: "report.txt".into(),
                    size: 7,
                    mime: None,
                    hash: blake3_hex(b"payload"),
                };
                staging.commit_files(&id, &[manifest], b"payload").await
            }
        });
        tokio::time::sleep(BLOCKED_PUBLISH_WINDOW).await;
        let own_partials = std::fs::read_dir(&inbox)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&format!(".{id}.")))
            .count();
        assert_eq!(
            own_partials, 0,
            "staged before reclaiming abandoned staging"
        );
        drop(held);
        commit.await.unwrap().unwrap();
        assert_eq!(
            std::fs::read(inbox.join(orphaned.as_str()).join("kept.txt")).unwrap(),
            b"kept"
        );
        assert!(!orphan.exists());
    }

    /// The publish lock must exclude a second handle on the same file, as a
    /// second receiver process would open, until the first is released. It
    /// is taken with `flock`/`LockFileEx`, never std's `File::lock`, which
    /// returns `Unsupported` on Android at the workspace MSRV.
    #[test]
    fn publish_lock_excludes_a_second_handle_until_released() {
        let root = tempfile::tempdir().unwrap();
        let held = publish_lock(root.path()).unwrap();
        let (locked, waiter_locked) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn({
            let root = root.path().to_owned();
            move || {
                let second = publish_lock(&root)?;
                let _ = locked.send(());
                drop(second);
                std::io::Result::Ok(())
            }
        });
        assert!(
            waiter_locked.recv_timeout(BLOCKED_PUBLISH_WINDOW).is_err(),
            "a second handle took the lock while it was held"
        );
        drop(held);
        waiter_locked.recv().unwrap();
        waiter.join().unwrap().unwrap();
    }

    /// A `.publish.lock` symlink planted in the inbox is neither followed nor
    /// has its target chmodded, and a commit then fails instead of running
    /// unlocked.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_publish_lock_is_not_followed_and_fails_the_commit() {
        use std::os::unix::fs::PermissionsExt;
        /// A mode the lock code would never set, to detect a chmod.
        const FOREIGN_MODE: u32 = 0o644;
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("inbox");
        let staging = StagingArea::new(inbox.clone()).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"foreign").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(FOREIGN_MODE)).unwrap();
        std::os::unix::fs::symlink(&target, inbox.join(PUBLISH_LOCK_NAME)).unwrap();
        assert!(publish_lock(&inbox).is_err());
        let manifest = clipsync_protocol::FileManifest {
            name: "report.txt".into(),
            size: 7,
            mime: None,
            hash: blake3_hex(b"payload"),
        };
        let id = TransferId::new();
        assert!(staging
            .commit_files(&id, &[manifest], b"payload")
            .await
            .is_err());
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS, FOREIGN_MODE);
        assert_eq!(std::fs::read(&target).unwrap(), b"foreign");
        assert!(!inbox.join(id.as_str()).exists());
    }

    /// Mask of the permission bits in `st_mode`, without the file type.
    #[cfg(unix)]
    const PERMISSION_BITS: u32 = 0o777;

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
            .map(|_| {
                let path = inbox.join(scratch_name(id.as_str(), PARTIAL_SUFFIX));
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

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn committed_files_carry_launch_services_quarantine_attribute() {
        fn quarantine_of(path: &Path) -> Option<String> {
            crate::quarantine::value_of(path).unwrap()
        }

        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let payload = b"#!/bin/sh\necho hi\nreadme";
        let (script, readme) = payload.split_at(18);
        let files = vec![
            FileManifest {
                name: "run.command".into(),
                size: script.len() as u64,
                mime: None,
                hash: blake3_hex(script),
            },
            FileManifest {
                name: "README".into(),
                size: readme.len() as u64,
                mime: None,
                hash: blake3_hex(readme),
            },
        ];
        let paths = staging
            .commit_files(&TransferId::new(), &files, payload)
            .await
            .unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        assert_eq!(paths.len(), 2);
        for path in &paths {
            let value = quarantine_of(path)
                .unwrap_or_else(|| panic!("{} lacks com.apple.quarantine", path.display()));
            let fields: Vec<&str> = value.split(';').collect();
            assert_eq!(fields.len(), 4, "unexpected quarantine value {value:?}");
            assert_eq!(fields[0], "0081");
            let stamp = u64::from_str_radix(fields[1], 16).unwrap();
            assert!((before..=after).contains(&stamp), "timestamp {stamp:#x}");
            assert_eq!(fields[2], "ClipSync");
            let uuid: Vec<&str> = fields[3].split('-').collect();
            assert_eq!(
                uuid.iter().map(|g| g.len()).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12]
            );
            assert!(fields[3].chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
        }
    }

    /// A published directory that a commit reuses (identical manifest) or
    /// that the sweep restores (interrupted replacement) may hold files
    /// written without the attribute, for example by an older ClipSync. Both
    /// paths must leave every received file quarantined, without replacing a
    /// value that is already there (it may record the user's approval).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn reused_and_restored_commits_quarantine_unmarked_files_and_keep_existing_values() {
        use crate::quarantine::{mark, value_of, QUARANTINE_AGENT};
        use std::os::unix::fs::MetadataExt;
        // A value LaunchServices could have written after the user approved
        // the file (flag 0x0040 set), from a different agent.
        const APPROVED: &str = "00c1;5f5e1000;Safari;3F2504E0-4F89-11D3-9A0C-0305E82C3301";
        let agent_of = |path: &Path| {
            let value = value_of(path)
                .unwrap()
                .unwrap_or_else(|| panic!("{} lacks com.apple.quarantine", path.display()));
            value.split(';').nth(2).unwrap().to_owned()
        };
        let manifest = |name: &str, bytes: &[u8]| FileManifest {
            name: name.into(),
            size: bytes.len() as u64,
            mime: None,
            hash: blake3_hex(bytes),
        };
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();

        // Reuse: the identical commit is already published, unmarked.
        let reused = TransferId::new();
        let published = staging.inbox().join(reused.as_str());
        std::fs::create_dir(&published).unwrap();
        std::fs::write(published.join("run.command"), b"script").unwrap();
        std::fs::write(published.join("approved.txt"), b"seen").unwrap();
        mark(&published.join("approved.txt"), APPROVED).unwrap();
        let inode = std::fs::metadata(&published).unwrap().ino();
        let paths = staging
            .commit_files(
                &reused,
                &[
                    manifest("run.command", b"script"),
                    manifest("approved.txt", b"seen"),
                ],
                b"scriptseen".to_vec(),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(&published).unwrap().ino(),
            inode,
            "not reused"
        );
        assert_eq!(agent_of(&paths[0]), QUARANTINE_AGENT);
        assert_eq!(value_of(&paths[1]).unwrap().as_deref(), Some(APPROVED));

        // Restore: a replacement was interrupted between its two renames.
        let interrupted = TransferId::new();
        let orphan = staging
            .inbox()
            .join(scratch_name(interrupted.as_str(), RETIRED_SUFFIX));
        std::fs::create_dir(&orphan).unwrap();
        std::fs::write(orphan.join("kept.command"), b"payload").unwrap();
        // An unrelated commit runs the sweep, which restores the orphan.
        staging
            .commit_files(
                &TransferId::new(),
                &[manifest("b.txt", b"x")],
                b"x".to_vec(),
            )
            .await
            .unwrap();
        let restored = staging
            .inbox()
            .join(interrupted.as_str())
            .join("kept.command");
        assert_eq!(std::fs::read(&restored).unwrap(), b"payload");
        assert_eq!(agent_of(&restored), QUARANTINE_AGENT);
    }
}
