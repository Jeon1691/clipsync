use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FileManifest, TransferId};

use crate::sanitize::{safe_filename, safe_transfer_id};
use clipsync_protocol::{CHUNK_SIZE, FILE_MAX_COUNT, TRANSFER_MAX_BYTES};
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
}
