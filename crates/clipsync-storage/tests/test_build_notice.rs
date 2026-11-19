// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! A binary built with `test-isolated-keychain` outside this crate's own unit
//! tests says once that it keeps secrets file-only. Workspace `cargo test` and
//! `--all-targets` builds unify the dev-only feature into debug product
//! binaries, which would otherwise skip the keychain mirror silently. This
//! file is its own test binary, so no other test consumes the one notice.

use clipsync_storage::{AppPaths, LocalStore};
use std::sync::{Arc, Mutex};

/// Text of the one-time notice; pinned here because it is the user contract.
const FILE_ONLY_NOTICE: &str = "secrets are file-only in this test build";

/// Test-build switch that mirrors secrets under the separate test service.
const NATIVE_KEYCHAIN_ENV: &str = "CLIPSYNC_TEST_NATIVE_KEYCHAIN";

/// Whether this platform has an OS keychain the feature skips.
const PLATFORM_HAS_KEYCHAIN: bool = cfg!(any(target_os = "macos", target_os = "windows"));

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_test_build_says_once_that_secrets_are_file_only_on_keychain_platforms() {
    // An opted-in run mirrors under the test service, so it is not file-only.
    let opted_in = std::env::var_os(NATIVE_KEYCHAIN_ENV).is_some_and(|value| value == "1");
    let expect_notice = PLATFORM_HAS_KEYCHAIN && !opted_in;
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    tracing::subscriber::with_default(subscriber, || {
        for root in [&first, &second] {
            LocalStore::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap();
        }
    });
    let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let notices = output.matches(FILE_ONLY_NOTICE).count();
    assert_eq!(notices, usize::from(expect_notice), "{output}");
    if expect_notice {
        assert!(output.contains("WARN"), "{output}");
    }
}
