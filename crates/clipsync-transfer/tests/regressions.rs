// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Regressions for resource bounds and atomic, unambiguous file commits.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clipsync_crypto::blake3_hex;
use clipsync_protocol::{FileManifest, TransferId, CHUNK_SIZE, FILE_MAX_COUNT, TRANSFER_MAX_BYTES};
use clipsync_transfer::{prune_idle_transfers, StagingArea, TransferError};

struct Inbox(PathBuf);

impl Inbox {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("clipsync-regression-{}", ulid::Ulid::new())))
    }

    fn staging(&self) -> StagingArea {
        StagingArea::new(self.0.clone()).expect("create isolated inbox")
    }

    fn assert_empty(&self) {
        assert_eq!(std::fs::read_dir(&self.0).unwrap().count(), 0);
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn file(name: &str, contents: &[u8]) -> FileManifest {
    FileManifest {
        name: name.into(),
        size: contents.len() as u64,
        mime: None,
        hash: blake3_hex(contents),
    }
}

#[test]
fn regression_f33_fresh_transfers_evict_oldest_and_preserve_newest_data() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let start = Instant::now();
    let now = start + Duration::from_secs(40);
    let mut incoming = HashMap::new();
    let mut ids = Vec::new();
    for offset in 0..40 {
        let id = TransferId::new();
        let mut transfer = staging.begin(&id, 2);
        transfer.started = start + Duration::from_secs(offset);
        transfer.push(0, 2, b"first".to_vec()).unwrap();
        incoming.insert(id.to_string(), transfer);
        ids.push(id);
    }

    // Every transfer is fresh. Expiration alone cannot satisfy the capacity bound.
    let removed = prune_idle_transfers(&mut incoming, now, Duration::from_secs(300));
    assert_eq!(removed, 8);
    assert_eq!(incoming.len(), 32);
    for id in &ids[..8] {
        assert!(!incoming.contains_key(id.as_str()), "old transfer survived");
    }
    for id in &ids[8..] {
        let mut transfer = incoming.remove(id.as_str()).expect("new transfer retained");
        transfer.push(1, 2, b"second".to_vec()).unwrap();
        assert_eq!(
            transfer.assemble(&blake3_hex(b"firstsecond")).unwrap(),
            b"firstsecond"
        );
    }
}

#[tokio::test]
async fn regression_f37_duplicate_names_preserve_the_previous_commit() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let id = TransferId::new();
    let original = staging
        .commit_files(&id, &[file("keep.txt", b"saved")], b"saved")
        .await
        .unwrap();

    let error = staging
        .commit_files(
            &id,
            &[file("note.txt", b"old"), file("note.txt", b"new")],
            b"oldnew",
        )
        .await
        .expect_err("duplicate names must not overwrite each other");
    assert!(matches!(error, TransferError::UnsafeName(name) if name == "note.txt"));
    assert_eq!(std::fs::read(&original[0]).unwrap(), b"saved");
    assert!(!inbox.0.join(id.as_str()).join("note.txt").exists());
    assert!(!inbox.0.join(format!(".{id}.partial")).exists());
}

#[tokio::test]
async fn distinct_file_names_commit_distinct_contents() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let files = staging
        .commit_files(
            &TransferId::new(),
            &[file("first.txt", b"first"), file("second.txt", b"second")],
            b"firstsecond",
        )
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"first");
    assert_eq!(std::fs::read(&files[1]).unwrap(), b"second");
}

#[test]
fn regression_f38_chunk_count_above_limit_is_rejected_and_poisoned() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let mut transfer = staging.begin(&TransferId::new(), 1025);
    assert!(matches!(
        transfer.push(0, 1025, vec![1]),
        Err(TransferError::Chunk("chunk index out of range"))
    ));
    assert!(matches!(
        transfer.push(1, 1025, vec![2]),
        Err(TransferError::Chunk("transfer already failed"))
    ));
    assert!(!transfer.is_complete());
}

#[test]
fn regression_f38_oversized_chunk_cannot_be_assembled() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let mut transfer = staging.begin(&TransferId::new(), 1);
    assert!(matches!(
        transfer.push(0, 1, vec![7; CHUNK_SIZE + 1]),
        Err(TransferError::Chunk("transfer too large"))
    ));
    assert!(!transfer.is_complete());
    assert!(matches!(
        transfer.assemble("unused because transfer failed"),
        Err(TransferError::Chunk("incomplete transfer"))
    ));
}

#[test]
fn regression_f38_cumulative_size_rejects_one_byte_over_limit() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let full_chunks = (TRANSFER_MAX_BYTES as usize / CHUNK_SIZE) as u32;
    let count = full_chunks + 1;
    let mut transfer = staging.begin(&TransferId::new(), count);
    for index in 0..full_chunks {
        transfer.push(index, count, vec![3; CHUNK_SIZE]).unwrap();
    }
    assert!(matches!(
        transfer.push(full_chunks, count, vec![3]),
        Err(TransferError::Chunk("transfer too large"))
    ));
    assert!(!transfer.is_complete());
    assert!(matches!(
        transfer.push(full_chunks, count, Vec::new()),
        Err(TransferError::Chunk("transfer already failed"))
    ));
}

#[test]
fn maximum_sized_chunk_still_roundtrips() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let payload = vec![7; CHUNK_SIZE];
    let mut transfer = staging.begin(&TransferId::new(), 1);
    transfer.push(0, 1, payload.clone()).unwrap();
    assert!(transfer.is_complete());
    assert_eq!(transfer.assemble(&blake3_hex(&payload)).unwrap(), payload);
}

#[test]
fn maximum_chunk_count_assembles_out_of_order_data() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let payload = b"abcd".repeat(256);
    let mut transfer = staging.begin(&TransferId::new(), 1024);
    for (index, byte) in payload.iter().enumerate().rev() {
        transfer.push(index as u32, 1024, vec![*byte]).unwrap();
    }
    assert!(transfer.is_complete());
    assert_eq!(transfer.assemble(&blake3_hex(&payload)).unwrap(), payload);
}

#[tokio::test]
async fn regression_f39_excessive_file_count_leaves_no_output() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let files: Vec<_> = (0..=FILE_MAX_COUNT)
        .map(|index| file(&format!("{index}.txt"), b""))
        .collect();
    assert!(matches!(
        staging.commit_files(&TransferId::new(), &files, b"").await,
        Err(TransferError::Integrity)
    ));
    inbox.assert_empty();
}

#[tokio::test]
async fn regression_f39_excessive_payload_leaves_no_output() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let payload = vec![0; TRANSFER_MAX_BYTES as usize + 1];
    let half = TRANSFER_MAX_BYTES as usize / 2;
    let files = [
        file("first.bin", &payload[..half]),
        file("second.bin", &payload[half..2 * half]),
        file("last.bin", &payload[2 * half..]),
    ];
    assert!(matches!(
        staging
            .commit_files(&TransferId::new(), &files, &payload)
            .await,
        Err(TransferError::Integrity)
    ));
    inbox.assert_empty();
}

#[tokio::test]
async fn maximum_file_count_still_commits_every_file() {
    let inbox = Inbox::new();
    let staging = inbox.staging();
    let manifests: Vec<_> = (0..FILE_MAX_COUNT)
        .map(|index| file(&format!("{index}.txt"), b"x"))
        .collect();
    let payload = vec![b'x'; FILE_MAX_COUNT];
    let paths = staging
        .commit_files(&TransferId::new(), &manifests, &payload)
        .await
        .unwrap();
    assert_eq!(paths.len(), FILE_MAX_COUNT);
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(path.file_name().unwrap(), format!("{index}.txt").as_str());
        assert_eq!(std::fs::read(path).unwrap(), b"x");
    }
}
