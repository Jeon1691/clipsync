// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! macOS Gatekeeper quarantine marking for files received from peers.
//!
//! Apple documents quarantine only through the LaunchServices dictionary API
//! (`LSQuarantine.h`: `kLSQuarantineAgentNameKey`, `kLSQuarantineTimeStampKey`,
//! `kLSQuarantineEventIdentifierKey`; exposed in Foundation as
//! `NSURLQuarantinePropertiesKey`). LaunchServices serializes those properties
//! into the `com.apple.quarantine` extended attribute as the UTF-8 string
//! `<flags hex>;<unix seconds hex>;<agent name>;<event UUID>` with no trailing
//! NUL. That on-disk form is undocumented but long-stable; see
//! <https://eclecticlight.co/2017/12/11/xattr-com-apple-quarantine-the-quarantine-flag/>
//! and, for the current encoding, the value LaunchServices writes for a browser
//! download through the public API (for example
//! `0081;6af728cc;Chrome;99A89314-4279-459D-BA85-977BCD747F1B`).
//!
//! Writing the attribute directly with `setxattr(2)` avoids linking
//! Foundation/CoreServices into this crate. The attribute alone is what
//! Gatekeeper and LaunchServices consult on first open; no
//! `QuarantineEventsV2` database row is created, so Finder's "downloaded from"
//! details are absent, which matches peer-to-peer receipt with no origin URL.
//!
//! Every path that writes peer-received bytes to disk (the inbox commit, the
//! `pull --output` copy, the pasteboard image sidecar) uses this module so the
//! format and the failure policy stay identical.

use std::ffi::CString;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Extended attribute name consulted by LaunchServices and Gatekeeper.
const QUARANTINE_XATTR: &std::ffi::CStr = c"com.apple.quarantine";

/// Flags field LaunchServices writes for an ordinary downloaded file that has
/// not yet been approved by the user (the value observed for browser
/// downloads). The user-approved bit (`0x0040`) is clear, so the first open
/// goes through Gatekeeper's assessment and confirmation prompt.
const QUARANTINE_FLAGS: &str = "0081";

/// Agent name shown by Gatekeeper ("... downloaded by ClipSync") and returned
/// as `LSQuarantineAgentName`.
pub const QUARANTINE_AGENT: &str = "ClipSync";

/// `setxattr(2)` position argument; only resource forks use non-zero offsets.
const XATTR_POSITION: u32 = 0;

/// Read buffer for [`value_of`]. A LaunchServices value is four short fields
/// (`0081;` + 8 hex digits + `;` + agent + `;` + 36-character UUID), well under
/// this; a longer foreign value is reported as `ERANGE`.
const QUARANTINE_READ_CAPACITY: usize = 256;

/// RFC 9562 §5.8: UUIDv8 version nibble (octet 6, high nibble) and its value.
const UUID_VERSION_MASK: u128 = 0xF << 76;
const UUID_VERSION_8: u128 = 0x8 << 76;
/// RFC 9562 §4.1: variant bits (octet 8, top two bits) set to `10`.
const UUID_VARIANT_MASK: u128 = 0b11 << 62;
const UUID_VARIANT_RFC: u128 = 0b10 << 62;

/// Builds one quarantine value for a receive event. All files committed by a
/// single transfer share the event identifier, as one browser download does.
pub fn quarantine_value() -> String {
    // A clock before 1970 is a misconfigured host; a zero timestamp still
    // quarantines the file, which is the property that matters.
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!(
        "{QUARANTINE_FLAGS};{seconds:08x};{QUARANTINE_AGENT};{}",
        event_uuid()
    )
}

/// A unique event identifier formatted as a UUID. The 128 bits come from a
/// ULID (millisecond time plus 80 random bits); stamping the RFC 9562 version-8
/// ("custom") and variant bits keeps the string a well-formed UUID without
/// claiming it is a random v4 UUID.
fn event_uuid() -> String {
    let bits = (u128::from(ulid::Ulid::generate()) & !UUID_VERSION_MASK & !UUID_VARIANT_MASK)
        | UUID_VERSION_8
        | UUID_VARIANT_RFC;
    let hex = format!("{bits:032X}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Sets `com.apple.quarantine` on `path` without following a final symlink.
/// Use it for files ClipSync created itself inside a private directory.
pub fn mark(path: &Path, value: &str) -> std::io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `c_path` and `QUARANTINE_XATTR` are NUL-terminated and live for
    // the call; `value` is a valid buffer of `value.len()` bytes that the
    // kernel only reads.
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            QUARANTINE_XATTR.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            XATTR_POSITION,
            libc::XATTR_NOFOLLOW,
        )
    };
    status(rc)
}

/// Sets `com.apple.quarantine` on an open file. Use it when the destination
/// path is outside ClipSync's control (for example `pull --output DIR`): the
/// attribute lands on the inode that receives the bytes, even if the path was
/// a pre-existing symlink that the open followed.
pub fn mark_file(file: BorrowedFd<'_>, value: &str) -> std::io::Result<()> {
    // SAFETY: `file` is a live descriptor for the duration of the borrow;
    // `QUARANTINE_XATTR` is NUL-terminated and `value` is a valid buffer of
    // `value.len()` bytes that the kernel only reads. `XATTR_NOFOLLOW` has no
    // meaning for a descriptor, so no options are passed.
    let rc = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            QUARANTINE_XATTR.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            XATTR_POSITION,
            0,
        )
    };
    status(rc)
}

/// [`mark`] with the received-file failure policy: a volume without xattr
/// support (for example some network or FAT volumes) must not make peer
/// transfers fail outright, so the error is logged and the caller still
/// delivers the integrity-checked file.
pub fn mark_or_warn(path: &Path, value: &str) {
    if let Err(error) = mark(path, value) {
        warn_unmarked(&error);
    }
}

/// [`mark_file`] with the same failure policy as [`mark_or_warn`].
pub fn mark_file_or_warn(file: BorrowedFd<'_>, value: &str) {
    if let Err(error) = mark_file(file, value) {
        warn_unmarked(&error);
    }
}

/// Reads `com.apple.quarantine` from `path` without following a final
/// symlink. `Ok(None)` means the attribute is absent.
pub fn value_of(path: &Path) -> std::io::Result<Option<String>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let mut buf = vec![0u8; QUARANTINE_READ_CAPACITY];
    // SAFETY: both strings are NUL-terminated and `buf` outlives the call with
    // its real length passed as the size.
    let len = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            QUARANTINE_XATTR.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            XATTR_POSITION,
            libc::XATTR_NOFOLLOW,
        )
    };
    let Ok(len) = usize::try_from(len) else {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOATTR) {
            Ok(None)
        } else {
            Err(error)
        };
    };
    buf.truncate(len);
    String::from_utf8(buf)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn status(rc: libc::c_int) -> std::io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn warn_unmarked(error: &std::io::Error) {
    tracing::warn!(%error, "could not quarantine received file");
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn event_uuid_is_rfc9562_version_8_and_unique() {
        let a = event_uuid();
        let b = event_uuid();
        assert_ne!(a, b);
        let groups: Vec<&str> = a.split('-').collect();
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(groups[2].starts_with('8'), "version nibble in {a}");
        assert!(
            matches!(groups[3].as_bytes()[0], b'8' | b'9' | b'A' | b'B'),
            "variant bits in {a}"
        );
    }

    #[test]
    fn mark_reports_failure_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let err = mark(&dir.path().join("missing"), &quarantine_value()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        mark_or_warn(&dir.path().join("missing"), &quarantine_value());
    }

    #[test]
    fn value_of_distinguishes_absent_from_marked_and_marks_through_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let file = std::fs::File::create(&path).unwrap();
        assert_eq!(value_of(&path).unwrap(), None);
        let value = quarantine_value();
        mark_file(std::os::fd::AsFd::as_fd(&file), &value).unwrap();
        assert_eq!(value_of(&path).unwrap().as_deref(), Some(value.as_str()));
        assert_eq!(
            value_of(&dir.path().join("missing")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
