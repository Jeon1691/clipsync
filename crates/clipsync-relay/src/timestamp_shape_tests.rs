// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Shape checks for the UTC timestamp strings the relay emits.
//!
//! The relay writes two spellings today: serde output of protocol types
//! (`Z`) and `to_rfc3339` in webhook bodies (`+00:00`). Both use a
//! four-digit year and the shortest exact fraction. These helpers pin those
//! spellings for timestamps taken from the wall clock, where a golden string
//! is impossible.

/// Layout of the seconds part: `0` marks a digit, every other byte is a
/// literal separator.
const SECONDS_TEMPLATE: &[u8] = b"0000-00-00T00:00:00";
/// Fraction digits come in groups of three (milli, micro, nano).
const FRACTION_GROUP: usize = 3;
/// Nanoseconds are the finest precision the timestamps carry.
const MAX_FRACTION_DIGITS: usize = 9;

/// Suffix of serde-serialized protocol timestamps.
pub(crate) const ZULU_SUFFIX: &str = "Z";
/// Suffix `to_rfc3339` writes for UTC, used by the webhook body's `ts`.
pub(crate) const UTC_OFFSET_SUFFIX: &str = "+00:00";

/// True when `value` is `YYYY-MM-DDTHH:MM:SS`, then nothing or `.` and 3, 6
/// or 9 digits whose last group of three is not all zeros (the shortest
/// exact spelling), then exactly `suffix`.
pub(crate) fn is_utc_timestamp(value: &str, suffix: &str) -> bool {
    let Some(body) = value.strip_suffix(suffix) else {
        return false;
    };
    let Some((seconds, fraction)) = body.split_at_checked(SECONDS_TEMPLATE.len()) else {
        return false;
    };
    let seconds_ok =
        seconds
            .bytes()
            .zip(SECONDS_TEMPLATE)
            .all(|(byte, &expected)| match expected {
                b'0' => byte.is_ascii_digit(),
                separator => byte == separator,
            });
    let fraction_ok = match fraction.strip_prefix('.') {
        None => fraction.is_empty(),
        Some(digits) => {
            !digits.is_empty()
                && digits.len() <= MAX_FRACTION_DIGITS
                && digits.len() % FRACTION_GROUP == 0
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits[digits.len() - FRACTION_GROUP..]
                    .bytes()
                    .all(|b| b == b'0')
        }
    };
    seconds_ok && fraction_ok
}

/// [`is_utc_timestamp`] with `Z`: the canonical protocol spelling.
pub(crate) fn is_canonical_utc(value: &str) -> bool {
    is_utc_timestamp(value, ZULU_SUFFIX)
}

#[test]
fn utc_shape_checks_match_the_protocol_and_rfc3339_vectors() {
    for good in [
        "2023-11-14T22:13:20Z",
        "2023-11-14T22:13:20.100Z",
        "2023-11-14T22:13:20.123456Z",
        "2023-11-14T22:13:20.000000001Z",
    ] {
        assert!(is_canonical_utc(good), "{good}");
        let offset = good.replace(ZULU_SUFFIX, UTC_OFFSET_SUFFIX);
        assert!(is_utc_timestamp(&offset, UTC_OFFSET_SUFFIX), "{offset}");
        assert!(!is_utc_timestamp(&offset, ZULU_SUFFIX), "{offset}");
        assert!(!is_utc_timestamp(good, UTC_OFFSET_SUFFIX), "{good}");
    }
    for bad in [
        "2023-11-14T22:13:20+00:00",
        "2023-11-14T22:13:20.000Z",
        "2023-11-14T22:13:20.123000Z",
        "2023-11-14T22:13:20.1Z",
        "2023-11-14T22:13:20.1234567890Z",
        "2023-11-14T22:13:20.Z",
        "2023-11-14 22:13:20Z",
        "2023-11-14T22:13:20z",
        "2023-11-14T22:13Z",
        "2023-11-14T22-13-20Z",
        "+2023-11-14T22:13:20Z",
        "Z",
    ] {
        assert!(!is_canonical_utc(bad), "{bad}");
    }
    for bad in [
        "2023-11-14T22:13:20Z",
        "2023-11-14T22:13:20.123000+00:00",
        "2023-11-14T22:13:20+0000",
        "2023-11-14T22:13:20-00:00",
    ] {
        assert!(!is_utc_timestamp(bad, UTC_OFFSET_SUFFIX), "{bad}");
    }
}
