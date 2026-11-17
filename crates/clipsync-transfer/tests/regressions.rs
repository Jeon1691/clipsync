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
        Self(std::env::temp_dir().join(format!("clipsync-regression-{}", ulid::Ulid::generate())))
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

/// Rounds of two simultaneous receivers committing one transfer. A single round
/// can interleave benignly; this many rounds reliably exposed the shared
/// partial-directory race on unchanged code while staying well under a second.
const CONCURRENT_RECEIVER_ROUNDS: usize = 64;

/// Files per concurrent transfer: several writes widen the window in which a
/// second receiver can remove or rename a shared partial directory.
const CONCURRENT_RECEIVER_FILES: usize = 8;

fn partial_entries(inbox: &Inbox) -> Vec<String> {
    std::fs::read_dir(&inbox.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with('.'))
        .collect()
}

/// A daemon and `pull --wait` connected for the same device both receive every
/// file transfer and commit it to the same inbox under the same transfer id.
/// Neither receiver may fail, and neither may delete the files the other just
/// returned to its caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regression_concurrent_receivers_of_one_transfer_keep_each_others_files() {
    let inbox = Inbox::new();
    let manifests: Vec<_> = (0..CONCURRENT_RECEIVER_FILES)
        .map(|index| file(&format!("{index}.txt"), format!("body-{index}").as_bytes()))
        .collect();
    let payload: Vec<u8> = (0..CONCURRENT_RECEIVER_FILES)
        .flat_map(|index| format!("body-{index}").into_bytes())
        .collect();
    for _ in 0..CONCURRENT_RECEIVER_ROUNDS {
        let id = TransferId::new();
        let receive = |staging: StagingArea| {
            let (id, manifests, payload) = (id.clone(), manifests.clone(), payload.clone());
            tokio::spawn(async move {
                let paths = staging.commit_files(&id, &manifests, &payload).await?;
                // The engine reads each staged file right after committing it.
                let mut bodies = Vec::new();
                for path in &paths {
                    bodies.push(tokio::fs::read(path).await?);
                }
                Ok::<_, TransferError>((paths, bodies))
            })
        };
        let (daemon, pull) = tokio::join!(receive(inbox.staging()), receive(inbox.staging()));
        for (paths, bodies) in [daemon.unwrap().unwrap(), pull.unwrap().unwrap()] {
            assert_eq!(paths.len(), CONCURRENT_RECEIVER_FILES);
            for (index, (path, body)) in paths.iter().zip(&bodies).enumerate() {
                let expected = format!("body-{index}").into_bytes();
                assert_eq!(body, &expected);
                assert_eq!(std::fs::read(path).unwrap(), expected);
            }
        }
    }
    assert!(
        partial_entries(&inbox).is_empty(),
        "{:?}",
        partial_entries(&inbox)
    );
}

/// A second receiver that finds the transfer already committed with identical
/// content reuses those files instead of deleting and recreating them, and it
/// never touches another receiver's in-progress partial directory.
#[cfg(unix)]
#[tokio::test]
async fn regression_second_receiver_reuses_committed_files_and_spares_foreign_partials() {
    use std::os::unix::fs::MetadataExt;
    let inbox = Inbox::new();
    let id = TransferId::new();
    let manifests = [file("report.txt", b"payload")];
    let first = inbox
        .staging()
        .commit_files(&id, &manifests, b"payload")
        .await
        .unwrap();
    let committed_inode = std::fs::metadata(&first[0]).unwrap().ino();
    let foreign_partial = inbox.0.join(format!(".{id}.partial"));
    std::fs::create_dir(&foreign_partial).unwrap();
    std::fs::write(foreign_partial.join("report.txt"), b"in progress").unwrap();

    let second = inbox
        .staging()
        .commit_files(&id, &manifests, b"payload")
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read(&second[0]).unwrap(), b"payload");
    assert_eq!(
        std::fs::metadata(&second[0]).unwrap().ino(),
        committed_inode,
        "the committed file was deleted and recreated"
    );
    assert_eq!(
        std::fs::read(foreign_partial.join("report.txt")).unwrap(),
        b"in progress"
    );
}
