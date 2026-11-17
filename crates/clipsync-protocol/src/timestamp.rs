// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! UTC instants as ClipSync puts them on the wire and in persisted state.
//!
//! Released peers, relays and state files exchange RFC 3339 strings written
//! and read by `chrono` 0.4. [`Timestamp`] keeps that contract byte for byte
//! (see `tests/timestamp_vectors.rs`) while the calendar arithmetic comes
//! from the `time` crate:
//!
//! - output is `YYYY-MM-DDTHH:MM:SS`, then 0, 3, 6 or 9 fraction digits
//!   (the shortest exact rendering), then `Z`; years outside 0000-9999 carry
//!   an explicit sign;
//! - input is the same relaxed RFC 3339 grammar `chrono` accepted: `t`/`T`/space
//!   separator, `Z`/`z`/`UTC`/numeric offsets (with or without a colon),
//!   unpadded fields, surrounding whitespace, extra fraction digits (dropped),
//!   and leap seconds (`:60`) at any minute;
//! - the range is `-262143-01-01T00:00:00Z` through
//!   `+262142-12-31T23:59:59.999999999Z`;
//! - a leap second is kept as its `:59` second plus 1e9 or more nanoseconds,
//!   which the v2 envelope AAD binds, and arithmetic and differences treat it
//!   exactly as `chrono` did.

use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, Duration, Month, OffsetDateTime};

const NANOS_PER_SEC: u32 = 1_000_000_000;
const SECS_PER_MINUTE: i64 = 60;
const SECS_PER_HOUR: i64 = 3_600;
const SECS_PER_DAY: i64 = 86_400;
/// Second of the minute that a leap second extends (`:59` plus 1e9 ns).
const LEAP_SECOND_BASE: u8 = 59;
/// The `:60` second, accepted on input as a leap second.
const LEAP_SECOND: i64 = 60;
/// Year range of `chrono` 0.4 (`NaiveDate::MIN`/`MAX`), which bounds what
/// released peers can send and what existing state files can hold.
const MIN_YEAR: i32 = -262_143;
const MAX_YEAR: i32 = 262_142;
/// Unix seconds of `-262143-01-01T00:00:00Z` and `+262142-12-31T23:59:59Z`.
const MIN_UNIX_SECS: i64 = -8_334_601_228_800;
const MAX_UNIX_SECS: i64 = 8_210_266_876_799;
/// Years 0000-9999 are written with four digits and no sign (RFC 3339).
const PLAIN_YEARS: std::ops::RangeInclusive<i32> = 0..=9_999;
/// Digits of an unsigned year; a sign allows any number.
const UNSIGNED_YEAR_DIGITS: usize = 4;
/// Digits of the other date and time fields; one is enough.
const FIELD_DIGITS: usize = 2;
/// Fraction digits that carry nanoseconds; later digits are dropped.
const FRACTION_DIGITS: usize = 9;
/// Fraction digits per SI group (milli, micro, nano) on output.
const NANOS_PER_MILLI: u32 = 1_000_000;
const NANOS_PER_MICRO: u32 = 1_000;
/// A numeric offset must be under a day, as `chrono::FixedOffset` requires.
const MAX_OFFSET_SECS: i64 = SECS_PER_DAY - 1;
/// Literal `UTC` accepted (in any case) in place of an offset.
const UTC_DESIGNATOR: &[u8] = b"UTC";
/// U+2212 MINUS SIGN, accepted like `-` before an offset (ISO 8601).
const MINUS_SIGN: char = '\u{2212}';

/// A UTC instant with `chrono`-compatible RFC 3339 serde, ordering and
/// arithmetic. Construct one with [`Timestamp::now`], by parsing, or with
/// [`Timestamp::from_unix`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    /// Unix seconds. For a leap second, the `:59` second it extends.
    secs: i64,
    /// Nanoseconds; 1e9 and above only within a leap second.
    nanos: u32,
}

impl Timestamp {
    /// `-262143-01-01T00:00:00Z`, the earliest representable instant.
    pub const MIN: Self = Self {
        secs: MIN_UNIX_SECS,
        nanos: 0,
    };
    /// `+262142-12-31T23:59:59.999999999Z`, the latest representable instant.
    pub const MAX: Self = Self {
        secs: MAX_UNIX_SECS,
        nanos: NANOS_PER_SEC - 1,
    };

    /// The current system time.
    pub fn now() -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            secs: now.unix_timestamp(),
            nanos: now.nanosecond(),
        }
    }

    /// The instant `secs` Unix seconds and `nanos` (below 1e9) after the
    /// epoch, or `None` outside the representable range.
    pub fn from_unix(secs: i64, nanos: u32) -> Option<Self> {
        (nanos < NANOS_PER_SEC && (MIN_UNIX_SECS..=MAX_UNIX_SECS).contains(&secs))
            .then_some(Self { secs, nanos })
    }

    /// Whole Unix seconds, floored; a leap second reports its `:59` second.
    pub fn unix_timestamp(self) -> i64 {
        self.secs
    }

    /// Nanoseconds past [`Self::unix_timestamp`]; 1e9 or more in a leap second.
    pub fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Unix microseconds, truncated.
    pub fn unix_timestamp_micros(self) -> i64 {
        // Within the range this cannot overflow: |secs| < 9.3e12 s.
        self.secs * i64::from(NANOS_PER_SEC / NANOS_PER_MICRO)
            + i64::from(self.nanos / NANOS_PER_MICRO)
    }

    /// `self + rhs`, or `None` outside the representable range.
    ///
    /// Leaving a leap second by whole seconds counts it as its `:59` second
    /// going forward and as a full second going back, as `chrono` did.
    pub fn checked_add(self, rhs: Duration) -> Option<Self> {
        const NANOS: i64 = NANOS_PER_SEC as i64;
        let secs_to_add = rhs.whole_seconds();
        let nanos_to_add = i64::from(rhs.subsec_nanoseconds());
        let mut secs = self.secs;
        let mut nanos = i64::from(self.nanos);
        if nanos >= NANOS {
            if secs_to_add > 0 || (nanos_to_add > 0 && nanos + nanos_to_add >= 2 * NANOS) {
                nanos -= NANOS;
            } else if secs_to_add < 0 {
                nanos -= NANOS;
                secs += 1;
            } else {
                // Stays within (or just before) the leap second.
                return Some(Self {
                    secs,
                    nanos: u32::try_from(nanos + nanos_to_add).ok()?,
                });
            }
        }
        let mut secs = secs.checked_add(secs_to_add)?;
        nanos += nanos_to_add;
        if nanos < 0 {
            nanos += NANOS;
            secs -= 1;
        } else if nanos >= NANOS {
            nanos -= NANOS;
            secs += 1;
        }
        (MIN_UNIX_SECS..=MAX_UNIX_SECS)
            .contains(&secs)
            .then_some(Self {
                secs,
                nanos: u32::try_from(nanos).ok()?,
            })
    }

    /// `self - rhs`, or `None` outside the representable range.
    pub fn checked_sub(self, rhs: Duration) -> Option<Self> {
        self.checked_add(rhs.checked_neg()?)
    }

    /// The signed time from `earlier` to `self`. A leap second between the
    /// two counts as one second when the clock time of the later one is past
    /// it, as `chrono` did.
    pub fn signed_duration_since(self, earlier: Self) -> Duration {
        let (days, time_of_day) = (
            self.secs.div_euclid(SECS_PER_DAY),
            self.secs.rem_euclid(SECS_PER_DAY),
        );
        let (earlier_days, earlier_time_of_day) = (
            earlier.secs.div_euclid(SECS_PER_DAY),
            earlier.secs.rem_euclid(SECS_PER_DAY),
        );
        let mut secs = (days - earlier_days) * SECS_PER_DAY + time_of_day - earlier_time_of_day;
        if time_of_day > earlier_time_of_day && earlier.nanos >= NANOS_PER_SEC {
            secs += 1;
        } else if time_of_day < earlier_time_of_day && self.nanos >= NANOS_PER_SEC {
            secs -= 1;
        }
        let nanos = i64::from(self.nanos) - i64::from(earlier.nanos);
        let nanos_per_sec = i64::from(NANOS_PER_SEC);
        Duration::new(
            secs + nanos.div_euclid(nanos_per_sec),
            // In 0..1e9, so it fits.
            nanos.rem_euclid(nanos_per_sec) as i32,
        )
    }

    /// RFC 3339 with a `+00:00` offset, as `chrono`'s `to_rfc3339` wrote it.
    /// The pairing file and the mobile FFI carry this spelling.
    pub fn to_rfc3339(self) -> String {
        let mut text = String::new();
        // `String` writes cannot fail; `write` fails only on a broken range invariant.
        let _ = self.write(&mut text, 'T', Zone::NumericUtc);
        text
    }

    fn write(self, out: &mut impl fmt::Write, separator: char, zone: Zone) -> fmt::Result {
        let (secs, nanos) = if self.nanos >= NANOS_PER_SEC {
            (self.secs, self.nanos - NANOS_PER_SEC)
        } else {
            (self.secs, self.nanos)
        };
        // Every constructor keeps `secs` within the range, which `time` (with
        // `large-dates`) represents; report an error rather than print a
        // different instant if that invariant were ever broken.
        let civil = OffsetDateTime::from_unix_timestamp(secs).map_err(|_| fmt::Error)?;
        let year = civil.year();
        if PLAIN_YEARS.contains(&year) {
            write!(out, "{year:04}")?;
        } else {
            write!(out, "{year:+05}")?;
        }
        let second = if self.nanos >= NANOS_PER_SEC {
            LEAP_SECOND_BASE + 1
        } else {
            civil.second()
        };
        write!(
            out,
            "-{:02}-{:02}{separator}{:02}:{:02}:{second:02}",
            u8::from(civil.month()),
            civil.day(),
            civil.hour(),
            civil.minute(),
        )?;
        if nanos == 0 {
        } else if nanos % NANOS_PER_MILLI == 0 {
            write!(out, ".{:03}", nanos / NANOS_PER_MILLI)?;
        } else if nanos % NANOS_PER_MICRO == 0 {
            write!(out, ".{:06}", nanos / NANOS_PER_MICRO)?;
        } else {
            write!(out, ".{nanos:09}")?;
        }
        out.write_str(match zone {
            Zone::Z => "Z",
            Zone::NumericUtc => "+00:00",
            Zone::Named => " UTC",
        })
    }
}

/// How the UTC offset is spelled after the time.
#[derive(Clone, Copy)]
enum Zone {
    /// `Z`: serde, `Debug`.
    Z,
    /// `+00:00`: [`Timestamp::to_rfc3339`].
    NumericUtc,
    /// ` UTC`: `Display`.
    Named,
}

/// `2023-11-14T22:13:20.5Z` spelled `2023-11-14 22:13:20.500 UTC`, as the CLI
/// prints a pairing expiry.
impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, ' ', Zone::Named)
    }
}

/// The canonical RFC 3339 spelling, as on the wire.
impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, 'T', Zone::Z)
    }
}

struct Canonical(Timestamp);

impl fmt::Display for Canonical {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.write(f, 'T', Zone::Z)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Canonical(*self))
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Timestamp;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an RFC 3339 formatted date and time string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Timestamp, E> {
                value.parse().map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

impl std::ops::Add<Duration> for Timestamp {
    type Output = Self;

    /// Panics outside the representable range, as `chrono` did; use
    /// [`Timestamp::checked_add`] on untrusted values.
    fn add(self, rhs: Duration) -> Self {
        self.checked_add(rhs)
            .expect("`Timestamp + Duration` overflowed")
    }
}

impl std::ops::Sub<Duration> for Timestamp {
    type Output = Self;

    /// Panics outside the representable range, as `chrono` did; use
    /// [`Timestamp::checked_sub`] on untrusted values.
    fn sub(self, rhs: Duration) -> Self {
        self.checked_sub(rhs)
            .expect("`Timestamp - Duration` overflowed")
    }
}

impl std::ops::AddAssign<Duration> for Timestamp {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl std::ops::SubAssign<Duration> for Timestamp {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl std::ops::Sub for Timestamp {
    type Output = Duration;

    fn sub(self, rhs: Self) -> Duration {
        self.signed_duration_since(rhs)
    }
}

/// Why a string is not an accepted timestamp. The messages match `chrono`'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimestampParseError {
    #[error("input is out of range")]
    OutOfRange,
    #[error("no possible date and time matching input")]
    Impossible,
    #[error("input contains invalid characters")]
    Invalid,
    #[error("premature end of input")]
    TooShort,
    #[error("trailing input")]
    TooLong,
}

use TimestampParseError::{Impossible, Invalid, OutOfRange, TooLong, TooShort};

impl FromStr for Timestamp {
    type Err = TimestampParseError;

    /// The relaxed RFC 3339 grammar of `chrono`'s `DateTime<FixedOffset>`
    /// parser, which every released ClipSync build used for these fields.
    fn from_str(input: &str) -> Result<Self, TimestampParseError> {
        let mut s = input;
        let year = year(&mut s)?;
        literal(&mut s, "-")?;
        let month = field(&mut s, 1..=12)?;
        literal(&mut s, "-")?;
        let day = field(&mut s, 1..=31)?;
        s = match s.as_bytes().first() {
            Some(b't' | b'T' | b' ') => &s[1..],
            Some(_) => return Err(Invalid),
            None => return Err(TooShort),
        };
        let hour = field(&mut s, 0..=23)?;
        literal(&mut s, ":")?;
        let minute = field(&mut s, 0..=59)?;
        literal(&mut s, ":")?;
        let second = field(&mut s, 0..=LEAP_SECOND)?;
        let mut nanos = 0;
        if let Some(rest) = s.strip_prefix('.') {
            s = rest;
            nanos = fraction(&mut s)?;
        }
        s = s.trim_start();
        let offset = if s.len() >= UTC_DESIGNATOR.len()
            && s.as_bytes()[..UTC_DESIGNATOR.len()].eq_ignore_ascii_case(UTC_DESIGNATOR)
        {
            s = &s[UTC_DESIGNATOR.len()..];
            0
        } else {
            offset(&mut s)?
        };
        if !s.trim_start().is_empty() {
            return Err(TooLong);
        }
        let (second, nanos) = if second == LEAP_SECOND {
            (i64::from(LEAP_SECOND_BASE), nanos + NANOS_PER_SEC)
        } else {
            (second, nanos)
        };
        if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
            return Err(OutOfRange);
        }
        let month = Month::try_from(u8::try_from(month).map_err(|_| OutOfRange)?)
            .map_err(|_| OutOfRange)?;
        let date =
            Date::from_calendar_date(year, month, u8::try_from(day).map_err(|_| OutOfRange)?)
                .map_err(|_| OutOfRange)?;
        if offset.abs() > MAX_OFFSET_SECS {
            return Err(OutOfRange);
        }
        let midnight = date.midnight().assume_utc().unix_timestamp();
        let local = midnight + hour * SECS_PER_HOUR + minute * SECS_PER_MINUTE + second;
        let secs = local - offset;
        if !(MIN_UNIX_SECS..=MAX_UNIX_SECS).contains(&secs) {
            return Err(Impossible);
        }
        Ok(Self { secs, nanos })
    }
}

/// A literal, after optional whitespace.
fn literal(s: &mut &str, expected: &str) -> Result<(), TimestampParseError> {
    *s = s.trim_start();
    if s.len() < expected.len() {
        return Err(TooShort);
    }
    *s = s.strip_prefix(expected).ok_or(Invalid)?;
    Ok(())
}

/// One to `max` ASCII digits; more digits are left in `s`.
fn digits(s: &mut &str, max: usize) -> Result<i64, TimestampParseError> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err(TooShort);
    }
    let count = bytes
        .iter()
        .take(max)
        .take_while(|b| b.is_ascii_digit())
        .count();
    if count == 0 {
        return Err(Invalid);
    }
    let mut value: i64 = 0;
    for &b in &bytes[..count] {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(b - b'0')))
            .ok_or(OutOfRange)?;
    }
    *s = &s[count..];
    Ok(value)
}

/// A date or time field of one or two digits, after optional whitespace.
fn field(s: &mut &str, range: std::ops::RangeInclusive<i64>) -> Result<i64, TimestampParseError> {
    *s = s.trim_start();
    let value = digits(s, FIELD_DIGITS)?;
    range.contains(&value).then_some(value).ok_or(OutOfRange)
}

/// Up to four digits, or a sign and any number of digits.
fn year(s: &mut &str) -> Result<i32, TimestampParseError> {
    *s = s.trim_start();
    let value = if let Some(rest) = s.strip_prefix('-') {
        *s = rest;
        -digits(s, usize::MAX)?
    } else if let Some(rest) = s.strip_prefix('+') {
        *s = rest;
        digits(s, usize::MAX)?
    } else {
        digits(s, UNSIGNED_YEAR_DIGITS)?
    };
    i32::try_from(value).map_err(|_| OutOfRange)
}

/// The digits after `.`: the first nine are nanoseconds, the rest dropped.
fn fraction(s: &mut &str) -> Result<u32, TimestampParseError> {
    let before = s.len();
    let value = digits(s, FRACTION_DIGITS)?;
    let consumed = before - s.len();
    let scale = 10_i64.pow(u32::try_from(FRACTION_DIGITS - consumed).map_err(|_| OutOfRange)?);
    *s = s.trim_start_matches(|c: char| c.is_ascii_digit());
    u32::try_from(value * scale).map_err(|_| OutOfRange)
}

/// `Z`, `z`, or a sign (`+`, `-`, U+2212), two hour digits, any colons or
/// whitespace, and two minute digits. Returns seconds east of UTC.
fn offset(s: &mut &str) -> Result<i64, TimestampParseError> {
    if let Some(b'Z' | b'z') = s.as_bytes().first() {
        *s = &s[1..];
        return Ok(0);
    }
    let negative = match s.chars().next() {
        Some('+') => false,
        Some('-') | Some(MINUS_SIGN) => true,
        Some(_) => return Err(Invalid),
        None => return Err(TooShort),
    };
    *s = &s[s.chars().next().map_or(0, char::len_utf8)..];
    let pair = |s: &str| -> Option<(u8, u8)> {
        let b = s.as_bytes();
        (b.len() >= 2).then(|| (b[0], b[1]))
    };
    let hours = match pair(s).ok_or(TooShort)? {
        (h1 @ b'0'..=b'9', h2 @ b'0'..=b'9') => i64::from((h1 - b'0') * 10 + (h2 - b'0')),
        _ => return Err(Invalid),
    };
    *s = &s[2..];
    *s = s.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let minutes = match pair(s).ok_or(TooShort)? {
        (m1 @ b'0'..=b'5', m2 @ b'0'..=b'9') => i64::from((m1 - b'0') * 10 + (m2 - b'0')),
        (b'6'..=b'9', b'0'..=b'9') => return Err(OutOfRange),
        _ => return Err(Invalid),
    };
    *s = &s[2..];
    let secs = hours * SECS_PER_HOUR + minutes * SECS_PER_MINUTE;
    Ok(if negative { -secs } else { secs })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn range_constants_match_the_calendar() {
        let min = Date::from_calendar_date(MIN_YEAR, Month::January, 1).unwrap();
        let max = Date::from_calendar_date(MAX_YEAR, Month::December, 31).unwrap();
        assert_eq!(min.midnight().assume_utc().unix_timestamp(), MIN_UNIX_SECS);
        assert_eq!(
            max.midnight().assume_utc().unix_timestamp() + SECS_PER_DAY - 1,
            MAX_UNIX_SECS
        );
        assert_eq!(format!("{:?}", Timestamp::MIN), "-262143-01-01T00:00:00Z");
        assert_eq!(
            format!("{:?}", Timestamp::MAX),
            "+262142-12-31T23:59:59.999999999Z"
        );
    }

    #[test]
    fn spellings_of_one_instant() {
        let at: Timestamp = "2023-11-14T22:13:20.5Z".parse().unwrap();
        assert_eq!(
            at,
            Timestamp::from_unix(1_700_000_000, 500_000_000).unwrap()
        );
        assert_eq!(format!("{at:?}"), "2023-11-14T22:13:20.500Z");
        assert_eq!(at.to_string(), "2023-11-14 22:13:20.500 UTC");
        assert_eq!(at.to_rfc3339(), "2023-11-14T22:13:20.500+00:00");
        assert_eq!(
            serde_json::to_string(&at).unwrap(),
            r#""2023-11-14T22:13:20.500Z""#
        );
        let leap: Timestamp = "-0001-12-31T23:59:60Z".parse().unwrap();
        assert_eq!(leap.to_string(), "-0001-12-31 23:59:60 UTC");
        assert_eq!(leap.subsec_nanos(), NANOS_PER_SEC);
    }

    #[test]
    fn from_unix_rejects_leap_nanos_and_out_of_range_seconds() {
        assert!(Timestamp::from_unix(0, NANOS_PER_SEC).is_none());
        assert!(Timestamp::from_unix(MIN_UNIX_SECS - 1, 0).is_none());
        assert!(Timestamp::from_unix(MAX_UNIX_SECS + 1, 0).is_none());
        assert_eq!(
            Timestamp::from_unix(MAX_UNIX_SECS, NANOS_PER_SEC - 1),
            Some(Timestamp::MAX)
        );
    }

    #[test]
    fn parse_errors_name_the_reason() {
        for (input, error) in [
            ("", TooShort),
            ("x", Invalid),
            ("2023-13-01T00:00:00Z", OutOfRange),
            ("2023-02-29T00:00:00Z", OutOfRange),
            ("2023-11-14T22:13:20", TooShort),
            ("2023-11-14T22:13:20Zx", TooLong),
            ("2023-11-14T22:13:20+09:60", OutOfRange),
            ("2023-11-14T22:13:20+24:00", OutOfRange),
            ("+262142-12-31T23:59:59-00:01", Impossible),
            ("+99999999999-01-01T00:00:00Z", OutOfRange),
            ("+99999999999999999999-01-01T00:00:00Z", OutOfRange),
        ] {
            assert_eq!(input.parse::<Timestamp>(), Err(error), "{input:?}");
        }
        assert_eq!(Invalid.to_string(), "input contains invalid characters");
    }

    #[test]
    fn arithmetic_saturates_to_none_at_the_range_ends() {
        let second = Duration::seconds(1);
        let nano = Duration::nanoseconds(1);
        assert_eq!(Timestamp::MAX.checked_add(nano), None);
        assert_eq!(Timestamp::MIN.checked_sub(nano), None);
        assert_eq!(Timestamp::MAX.checked_add(Duration::MAX), None);
        assert_eq!(Timestamp::MIN.checked_add(Duration::MIN), None);
        assert_eq!(
            Timestamp::MAX
                .checked_sub(second)
                .map(Timestamp::unix_timestamp),
            Some(MAX_UNIX_SECS - 1)
        );
        assert_eq!(Timestamp::MIN + second - second, Timestamp::MIN);
    }

    /// Leap-second arithmetic as `chrono`'s `NaiveTime::overflowing_add_signed`
    /// and `signed_duration_since` define it.
    #[test]
    fn leap_second_arithmetic_matches_chrono() {
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        let leap = at("2016-12-31T23:59:60.3Z");
        for (delta, expected) in [
            (Duration::seconds(10), "2017-01-01T00:00:09.300Z"),
            (Duration::seconds(-10), "2016-12-31T23:59:50.300Z"),
            (Duration::milliseconds(500), "2016-12-31T23:59:60.800Z"),
            (Duration::milliseconds(700), "2017-01-01T00:00:00Z"),
            (Duration::milliseconds(-300), "2016-12-31T23:59:60Z"),
            (Duration::milliseconds(-500), "2016-12-31T23:59:59.800Z"),
            (Duration::milliseconds(1_500), "2017-01-01T00:00:00.800Z"),
            (Duration::ZERO, "2016-12-31T23:59:60.300Z"),
        ] {
            assert_eq!(format!("{:?}", leap + delta), expected, "{delta}");
        }
        // Midnight is 0.3 s *before* 23:59:60.3 by difference, though it
        // orders after it: `chrono` overlaps the leap second with midnight.
        assert_eq!(
            at("2017-01-01T00:00:00Z") - leap,
            Duration::milliseconds(-300)
        );
        assert_eq!(
            leap - at("2016-12-31T23:59:59Z"),
            Duration::milliseconds(1_300)
        );
        assert_eq!(
            at("2017-01-01T00:00:01Z") - leap,
            Duration::milliseconds(700)
        );
        assert_eq!(leap - leap, Duration::ZERO);
    }
}
