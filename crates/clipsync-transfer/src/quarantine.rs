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

use std::ffi::CString;
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
pub(crate) const QUARANTINE_AGENT: &str = "ClipSync";

/// `setxattr(2)` position argument; only resource forks use non-zero offsets.
const XATTR_POSITION: u32 = 0;

/// RFC 9562 §5.8: UUIDv8 version nibble (octet 6, high nibble) and its value.
const UUID_VERSION_MASK: u128 = 0xF << 76;
const UUID_VERSION_8: u128 = 0x8 << 76;
/// RFC 9562 §4.1: variant bits (octet 8, top two bits) set to `10`.
const UUID_VARIANT_MASK: u128 = 0b11 << 62;
const UUID_VARIANT_RFC: u128 = 0b10 << 62;

/// Builds one quarantine value for a receive event. All files committed by a
/// single transfer share the event identifier, as one browser download does.
pub(crate) fn quarantine_value() -> String {
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
pub(crate) fn mark(path: &Path, value: &str) -> std::io::Result<()> {
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
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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
    }
}
