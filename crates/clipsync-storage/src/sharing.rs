// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Bounded retries for the "access denied" Windows reports while another
//! handle uses a file that is being replaced or deleted.
//!
//! Files in the data directory are replaced whole by
//! [`crate::secrets::write_private`] and removed by `room leave`, while the
//! daemon, the CLI and app hosts read them from other processes. Windows
//! refuses to replace a file that another handle has open, and to open a
//! file that is being deleted (it is "delete pending" until its last handle
//! closes), both with `ERROR_ACCESS_DENIED`. Those refusals last only as long
//! as the other side's read or write, so they are retried. On other platforms
//! neither happens, and "access denied" is a real permission problem that is
//! reported at once.

use std::io::{ErrorKind, Result};
use std::path::Path;
use std::time::Duration;

/// How many times an operation refused with "access denied" is retried.
pub(crate) const ACCESS_DENIED_RETRIES: u32 = if cfg!(windows) { 20 } else { 0 };

/// Pause between those retries. The other side holds a small metadata file
/// for one read or one replace, well under a millisecond; 20 pauses of 5 ms
/// wait that out many times over while keeping a real permission error from
/// stalling a status poll for more than a tenth of a second.
pub(crate) const ACCESS_DENIED_RETRY_PAUSE: Duration = Duration::from_millis(5);

/// The pause used outside tests.
pub(crate) fn pause_before_retry() {
    std::thread::sleep(ACCESS_DENIED_RETRY_PAUSE);
}

/// Run `op`, running it again up to `retries` times while it fails with
/// "access denied", calling `pause` before each retry. Any other result,
/// and an "access denied" that outlasts the retries, is returned.
pub(crate) fn retry_access_denied<T>(
    mut op: impl FnMut() -> Result<T>,
    retries: u32,
    mut pause: impl FnMut(),
) -> Result<T> {
    let mut retries_left = retries;
    loop {
        match op() {
            Err(error) if error.kind() == ErrorKind::PermissionDenied && retries_left > 0 => {
                retries_left -= 1;
                pause();
            }
            result => return result,
        }
    }
}

/// Read a file that another process may remove at any moment: `Ok(None)`
/// when it is gone. Read without checking first, since a file gone between
/// a check and the read means no file, not an error, and on Windows a check
/// on a file that refuses opens reports it missing.
pub(crate) fn read_unless_removed(
    read: impl FnMut() -> Result<String>,
    retries: u32,
    pause: impl FnMut(),
) -> Result<Option<String>> {
    match retry_access_denied(read, retries, pause) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// [`read_unless_removed`] for `path` with the production retry policy.
pub(crate) fn read_if_present(path: &Path) -> Result<Option<String>> {
    read_unless_removed(
        || std::fs::read_to_string(path),
        ACCESS_DENIED_RETRIES,
        pause_before_retry,
    )
}

/// Read a file that must exist, with the production retry policy.
pub(crate) fn read_existing(path: &Path) -> Result<String> {
    retry_access_denied(
        || std::fs::read_to_string(path),
        ACCESS_DENIED_RETRIES,
        pause_before_retry,
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Replay `results`, one per read, counting the reads.
    fn scripted_reads(
        results: Vec<ErrorKind>,
        reads: &std::cell::Cell<usize>,
    ) -> impl FnMut() -> Result<String> + '_ {
        let mut results = results.into_iter();
        move || {
            reads.set(reads.get() + 1);
            Err(results.next().expect("no read left in the script").into())
        }
    }

    #[test]
    fn access_denied_is_retried_until_the_file_is_gone() {
        use ErrorKind::{NotFound, PermissionDenied};
        const RETRIES: u32 = 3;
        let reads = std::cell::Cell::new(0);
        let mut pauses = 0;
        let read = read_unless_removed(
            scripted_reads(vec![PermissionDenied, PermissionDenied, NotFound], &reads),
            RETRIES,
            || pauses += 1,
        );
        assert!(read.unwrap().is_none());
        assert_eq!((reads.get(), pauses), (3, 2));
    }

    #[test]
    fn access_denied_that_outlasts_the_retries_is_returned() {
        use ErrorKind::PermissionDenied;
        const RETRIES: u32 = 3;
        let reads = std::cell::Cell::new(0);
        let mut pauses = 0;
        let read = read_unless_removed(
            scripted_reads(vec![PermissionDenied; 5], &reads),
            RETRIES,
            || pauses += 1,
        );
        assert_eq!(read.unwrap_err().kind(), PermissionDenied);
        assert_eq!((reads.get(), pauses), (4, 3));
    }

    #[test]
    fn a_retried_operation_returns_the_first_success() {
        const RETRIES: u32 = 3;
        let mut attempts = 0;
        let mut pauses = 0;
        let result = retry_access_denied(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(ErrorKind::PermissionDenied.into())
                } else {
                    Ok(attempts)
                }
            },
            RETRIES,
            || pauses += 1,
        );
        assert_eq!(result.unwrap(), 3);
        assert_eq!(pauses, 2);
    }

    #[test]
    fn other_read_errors_are_returned_without_a_retry() {
        use ErrorKind::{InvalidData, PermissionDenied};
        let reads = std::cell::Cell::new(0);
        let mut pauses = 0;
        let read = read_unless_removed(
            scripted_reads(vec![InvalidData, PermissionDenied], &reads),
            ACCESS_DENIED_RETRIES,
            || pauses += 1,
        );
        assert_eq!(read.unwrap_err().kind(), InvalidData);
        assert_eq!((reads.get(), pauses), (1, 0));
    }

    #[cfg(not(windows))]
    #[test]
    fn access_denied_is_returned_at_once_outside_windows() {
        use ErrorKind::PermissionDenied;
        let reads = std::cell::Cell::new(0);
        let mut pauses = 0;
        let read = read_unless_removed(
            scripted_reads(vec![PermissionDenied, PermissionDenied], &reads),
            ACCESS_DENIED_RETRIES,
            || pauses += 1,
        );
        assert_eq!(read.unwrap_err().kind(), PermissionDenied);
        assert_eq!((reads.get(), pauses), (1, 0));
    }

    #[test]
    fn present_and_missing_files_read_as_contents_and_none() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        assert!(read_if_present(&path).unwrap().is_none());
        assert_eq!(
            read_existing(&path).unwrap_err().kind(),
            ErrorKind::NotFound
        );
        std::fs::write(&path, "contents").unwrap();
        assert_eq!(read_if_present(&path).unwrap().as_deref(), Some("contents"));
        assert_eq!(read_existing(&path).unwrap(), "contents");
    }
}
