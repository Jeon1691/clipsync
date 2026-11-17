// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        }
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

    pub async fn commit_files(
        &self,
        transfer_id: &TransferId,
        files: &[FileManifest],
        payload: &[u8],
    ) -> Result<Vec<PathBuf>> {
        if files.len() > FILE_MAX_COUNT || payload.len() as u64 > TRANSFER_MAX_BYTES {
            return Err(TransferError::Integrity);
        }
        let id = safe_transfer_id(transfer_id.as_str())?;
        let expected = files.iter().map(|f| f.size as usize).sum::<usize>();
        if payload.len() != expected {
            return Err(TransferError::Integrity);
        }
        let mut seen_names = std::collections::HashSet::new();
        let dest_dir = self.root.join(&id);
        let tmp_dir = self.root.join(format!(".{id}.partial"));
        if tmp_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        }
        tokio::fs::create_dir_all(&tmp_dir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp_dir, std::fs::Permissions::from_mode(0o700));
        }
        // One receive event per commit: every file shares its quarantine value.
        #[cfg(target_os = "macos")]
        let quarantine = crate::quarantine::quarantine_value();
        let mut offset = 0usize;
        let mut written = Vec::new();
        for file in files {
            let name = safe_filename(&file.name)?;
            if !seen_names.insert(name.clone()) {
                let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
                return Err(TransferError::UnsafeName(name));
            }
            let end = offset + file.size as usize;
            if end > payload.len() {
                let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
                return Err(TransferError::Integrity);
            }
            let slice = &payload[offset..end];
            if blake3_hex(slice) != file.hash {
                let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
                return Err(TransferError::Integrity);
            }
            let dest = tmp_dir.join(&name);
            tokio::fs::write(&dest, slice).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
            // Mark before the directory is published so a received file is never
            // visible in the inbox without Gatekeeper's first-open check. See
            // `quarantine::mark_or_warn` for why a failure still delivers.
            #[cfg(target_os = "macos")]
            crate::quarantine::mark_or_warn(&dest, &quarantine);
            written.push(name);
            offset = end;
        }
        if dest_dir.exists() {
            tokio::fs::remove_dir_all(&dest_dir).await?;
        }
        tokio::fs::rename(&tmp_dir, &dest_dir).await?;
        Ok(written.into_iter().map(|n| dest_dir.join(n)).collect())
    }
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
    async fn retries_replace_stale_partial_and_previous_commit_only_after_hash_validation() {
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
        assert!(!partial.exists());
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
    async fn blocked_partial_or_destination_paths_and_unsafe_names_leave_existing_data_intact() {
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
        assert!(staging
            .commit_files(&blocked_partial, &[file("report.txt")], b"payload")
            .await
            .is_err());
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

    #[cfg(unix)]
    #[tokio::test]
    async fn read_only_inbox_keeps_validated_bytes_private_when_commit_cannot_publish() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let staging = StagingArea::new(root.path().join("inbox")).unwrap();
        let id = TransferId::new();
        let partial = staging.inbox().join(format!(".{id}.partial"));
        std::fs::create_dir(&partial).unwrap();
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o500)).unwrap();
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
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(TransferError::Io(_))));
        assert_eq!(
            std::fs::read(partial.join("report.txt")).unwrap(),
            b"payload"
        );
        assert!(!staging.inbox().join(id.as_str()).exists());
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
}
