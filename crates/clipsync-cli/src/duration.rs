// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! `room create --ttl` duration parsing.
//!
//! This replaces `humantime::parse_duration` 2.4.0, which RUSTSEC-2026-0542
//! marks unmaintained. It accepts the same grammar and prints the same error
//! text, so existing scripts and documented values keep working:
//!
//! * a duration is one or more `<number><unit>` spans, e.g. `1m30s` or `1h 5min`;
//! * a number is decimal digits with an optional `.` fraction; whitespace is
//!   skipped inside a number and between a number and its unit;
//! * a unit is a run of ASCII letters or `µ` from [`Unit::from_name`];
//! * the bare string `0` is zero.
//!
//! Values are exact: a fraction that cannot be represented in whole
//! nanoseconds (or whole seconds for hours and longer) is rejected, as is
//! anything that overflows `u64` seconds.

use std::fmt;
use std::str::Chars;
use std::time::Duration;

/// Radix of the accepted decimal numbers.
const DECIMAL_RADIX: u64 = 10;
const NANOS_PER_MICRO: u64 = 1_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
const NANOS_PER_SEC: u64 = 1_000_000_000;
const SECS_PER_MIN: u64 = 60;
const NANOS_PER_MIN: u64 = SECS_PER_MIN * NANOS_PER_SEC;
const MINS_PER_HOUR: u64 = 60;
const SECS_PER_HOUR: u64 = MINS_PER_HOUR * SECS_PER_MIN;
const HOURS_PER_DAY: u64 = 24;
const SECS_PER_DAY: u64 = HOURS_PER_DAY * SECS_PER_HOUR;
const DAYS_PER_WEEK: u64 = 7;
const SECS_PER_WEEK: u64 = DAYS_PER_WEEK * SECS_PER_DAY;
/// humantime's month: 30.44 days (mean Gregorian month, rounded).
const SECS_PER_MONTH: u64 = 2_630_016;
/// Julian year: 365.25 days.
const SECS_PER_YEAR: u64 = 31_557_600;

/// Why a duration was rejected. `Display` matches humantime 2.4.0 verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DurationError {
    /// A character that is not a digit, letter, `µ`, `.` or whitespace, at this byte offset.
    InvalidCharacter(usize),
    /// A number was required at this byte offset.
    NumberExpected(usize),
    /// The unit is missing or not recognized; `value` is the span's integer part.
    UnknownUnit { unit: String, value: u64 },
    /// The value overflows or needs sub-nanosecond (or, for hours and longer,
    /// sub-second) precision.
    NumberOverflow,
    /// The input was empty or only whitespace.
    Empty,
}

impl std::error::Error for DurationError {}

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCharacter(offset) => write!(f, "invalid character at {offset}"),
            Self::NumberExpected(offset) => write!(f, "expected number at {offset}"),
            Self::UnknownUnit { unit, value } if unit.is_empty() => {
                write!(f, "time unit needed, for example {value}sec or {value}ms")
            }
            Self::UnknownUnit { unit, .. } => write!(
                f,
                "unknown time unit {unit:?}, supported units: ns, us/µs, ms, sec, min, \
                 hours, days, weeks, months, years (and few variations)"
            ),
            Self::NumberOverflow => f.write_str(
                "number is too large or cannot be represented without a lack of precision \
                 (values below 1ns are not supported)",
            ),
            Self::Empty => f.write_str("value was empty"),
        }
    }
}

/// Parse a human-friendly duration such as `2m`, `45s` or `1m 30s`.
pub(crate) fn parse_duration(input: &str) -> Result<Duration, DurationError> {
    if input == "0" {
        return Ok(Duration::ZERO);
    }
    Parser {
        src: input,
        chars: input.chars(),
    }
    .parse()
}

/// Amount added per unit of a span's integer or fractional part.
#[derive(Clone, Copy)]
enum Scale {
    Nanos(u64),
    Secs(u64),
}

#[derive(Clone, Copy)]
enum Unit {
    Nanosecond,
    Microsecond,
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "nanos" | "nsec" | "ns" => Self::Nanosecond,
            "usec" | "us" | "µs" => Self::Microsecond,
            "millis" | "msec" | "ms" => Self::Millisecond,
            "seconds" | "second" | "secs" | "sec" | "s" => Self::Second,
            "minutes" | "minute" | "min" | "mins" | "m" => Self::Minute,
            "hours" | "hour" | "hr" | "hrs" | "h" => Self::Hour,
            "days" | "day" | "d" => Self::Day,
            "weeks" | "week" | "wk" | "wks" | "w" => Self::Week,
            "months" | "month" | "M" => Self::Month,
            "years" | "year" | "yr" | "yrs" | "y" => Self::Year,
            _ => return None,
        })
    }

    /// Scale of the integer part.
    fn whole(self) -> Scale {
        match self {
            Self::Nanosecond => Scale::Nanos(1),
            Self::Microsecond => Scale::Nanos(NANOS_PER_MICRO),
            Self::Millisecond => Scale::Nanos(NANOS_PER_MILLI),
            Self::Second => Scale::Secs(1),
            Self::Minute => Scale::Secs(SECS_PER_MIN),
            Self::Hour => Scale::Secs(SECS_PER_HOUR),
            Self::Day => Scale::Secs(SECS_PER_DAY),
            Self::Week => Scale::Secs(SECS_PER_WEEK),
            Self::Month => Scale::Secs(SECS_PER_MONTH),
            Self::Year => Scale::Secs(SECS_PER_YEAR),
        }
    }

    /// Scale of the fractional part; `None` when no fraction is representable.
    /// Up to minutes a fraction resolves to whole nanoseconds, from hours on to
    /// whole seconds (humantime's precision rule).
    fn fraction(self) -> Option<Scale> {
        match self {
            Self::Nanosecond => None,
            Self::Microsecond => Some(Scale::Nanos(NANOS_PER_MICRO)),
            Self::Millisecond => Some(Scale::Nanos(NANOS_PER_MILLI)),
            Self::Second => Some(Scale::Nanos(NANOS_PER_SEC)),
            Self::Minute => Some(Scale::Nanos(NANOS_PER_MIN)),
            Self::Hour | Self::Day | Self::Week | Self::Month | Self::Year => Some(self.whole()),
        }
    }
}

/// Digits after a `.`: `numerator / denominator`, with `denominator` a power of ten.
#[derive(Clone, Copy)]
struct Fraction {
    numerator: u64,
    denominator: u64,
}

fn checked_mul(a: u64, b: u64) -> Result<u64, DurationError> {
    a.checked_mul(b).ok_or(DurationError::NumberOverflow)
}

fn checked_add(a: u64, b: u64) -> Result<u64, DurationError> {
    a.checked_add(b).ok_or(DurationError::NumberOverflow)
}

/// Exact division; a remainder would lose precision.
fn exact_div(a: u64, b: u64) -> Result<u64, DurationError> {
    if a.is_multiple_of(b) {
        Ok(a / b)
    } else {
        Err(DurationError::NumberOverflow)
    }
}

/// `value * 10 + digit`, failing on `u64` overflow.
fn push_digit(value: u64, digit: char) -> Result<u64, DurationError> {
    checked_add(checked_mul(value, DECIMAL_RADIX)?, digit_value(digit))
}

fn digit_value(digit: char) -> u64 {
    u64::from(digit) - u64::from('0')
}

fn is_unit_char(c: char) -> bool {
    c.is_ascii_alphabetic() || c == 'µ'
}

struct Parser<'a> {
    src: &'a str,
    chars: Chars<'a>,
}

impl Parser<'_> {
    /// Byte offset of the next unread character.
    fn offset(&self) -> usize {
        self.src.len() - self.chars.as_str().len()
    }

    fn parse(mut self) -> Result<Duration, DurationError> {
        let mut total = Duration::ZERO;
        let mut value = self.first_digit()?.ok_or(DurationError::Empty)?;
        'span: loop {
            // Rest of the number, up to and including the unit's first letter.
            let mut fraction = None;
            let mut offset = self.offset();
            while let Some(c) = self.chars.next() {
                match c {
                    '0'..='9' => value = push_digit(value, c)?,
                    c if c.is_whitespace() => {}
                    c if is_unit_char(c) => break,
                    '.' => {
                        fraction = Some(self.fraction(&mut offset)?);
                        break;
                    }
                    _ => return Err(DurationError::InvalidCharacter(offset)),
                }
                offset = self.offset();
            }
            // Rest of the unit; a digit starts the next span directly (`1m30s`).
            let unit_start = offset;
            let mut unit_end = self.offset();
            while let Some(c) = self.chars.next() {
                match c {
                    '0'..='9' => {
                        self.add_span(value, fraction, unit_start, unit_end, &mut total)?;
                        value = digit_value(c);
                        continue 'span;
                    }
                    c if c.is_whitespace() => break,
                    c if is_unit_char(c) => {}
                    _ => return Err(DurationError::InvalidCharacter(unit_end)),
                }
                unit_end = self.offset();
            }
            self.add_span(value, fraction, unit_start, unit_end, &mut total)?;
            match self.first_digit()? {
                Some(digit) => value = digit,
                None => return Ok(total),
            }
        }
    }

    /// Skip whitespace and read the first digit of the next span, if any.
    fn first_digit(&mut self) -> Result<Option<u64>, DurationError> {
        let offset = self.offset();
        for c in self.chars.by_ref() {
            match c {
                '0'..='9' => return Ok(Some(digit_value(c))),
                c if c.is_whitespace() => {}
                _ => return Err(DurationError::NumberExpected(offset)),
            }
        }
        Ok(None)
    }

    /// Read digits after a `.`, up to and including the unit's first letter.
    fn fraction(&mut self, offset: &mut usize) -> Result<Fraction, DurationError> {
        let mut numerator = 0;
        let mut denominator: u64 = 1;
        while let Some(c) = self.chars.next() {
            match c {
                '0'..='9' => {
                    denominator = checked_mul(denominator, DECIMAL_RADIX)?;
                    numerator = push_digit(numerator, c)?;
                }
                c if c.is_whitespace() => {}
                c if is_unit_char(c) => break,
                _ => return Err(DurationError::InvalidCharacter(*offset)),
            }
            *offset = self.offset();
        }
        if denominator == 1 {
            // No digit after the separator, e.g. `1.` or `1.s`.
            return Err(DurationError::InvalidCharacter(*offset));
        }
        Ok(Fraction {
            numerator,
            denominator,
        })
    }

    fn add_span(
        &self,
        value: u64,
        fraction: Option<Fraction>,
        unit_start: usize,
        unit_end: usize,
        total: &mut Duration,
    ) -> Result<(), DurationError> {
        let name = &self.src[unit_start..unit_end];
        let unit = Unit::from_name(name).ok_or_else(|| DurationError::UnknownUnit {
            unit: name.to_owned(),
            value,
        })?;
        *total = match unit.whole() {
            Scale::Nanos(factor) => add(*total, 0, checked_mul(value, factor)?)?,
            Scale::Secs(factor) => add(*total, checked_mul(value, factor)?, 0)?,
        };
        if let Some(Fraction {
            numerator,
            denominator,
        }) = fraction
        {
            let scaled = |factor| exact_div(checked_mul(numerator, factor)?, denominator);
            *total = match unit.fraction().ok_or(DurationError::NumberOverflow)? {
                Scale::Nanos(factor) => add(*total, 0, scaled(factor)?)?,
                Scale::Secs(factor) => add(*total, scaled(factor)?, 0)?,
            };
        }
        Ok(())
    }
}

/// `total + secs + nanos`, failing instead of panicking on overflow.
fn add(total: Duration, secs: u64, nanos: u64) -> Result<Duration, DurationError> {
    let nanos = checked_add(u64::from(total.subsec_nanos()), nanos)?;
    let secs = checked_add(checked_add(total.as_secs(), secs)?, nanos / NANOS_PER_SEC)?;
    // Below one second, so this sum cannot carry into (and overflow) `secs`.
    Ok(Duration::from_secs(secs) + Duration::from_nanos(nanos % NANOS_PER_SEC))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::parse_duration;

    /// Text humantime 2.4.0 printed for an unknown unit; scripts may match it.
    const UNKNOWN_UNIT_HINT: &str = "supported units: ns, us/µs, ms, sec, min, hours, days, \
                                     weeks, months, years (and few variations)";

    fn secs(input: &str) -> u64 {
        parse_duration(input)
            .unwrap_or_else(|e| panic!("{input:?} rejected: {e}"))
            .as_secs()
    }

    fn rejection(input: &str) -> String {
        match parse_duration(input) {
            Ok(value) => panic!("{input:?} accepted as {value:?}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn ttl_duration_accepts_documented_examples() {
        // README (`--ttl 10m`), `--help` (`2m`) and the background-create test (`45s`).
        assert_eq!(secs("10m"), 600);
        assert_eq!(secs("2m"), 120);
        assert_eq!(secs("45s"), 45);
    }

    #[test]
    fn ttl_duration_accepts_every_unit_alias() {
        let cases: &[(&[&str], Duration)] = &[
            (&["nanos", "nsec", "ns"], Duration::from_nanos(7)),
            (&["usec", "us", "µs"], Duration::from_micros(7)),
            (&["millis", "msec", "ms"], Duration::from_millis(7)),
            (
                &["seconds", "second", "secs", "sec", "s"],
                Duration::from_secs(7),
            ),
            (
                &["minutes", "minute", "min", "mins", "m"],
                Duration::from_secs(7 * 60),
            ),
            (
                &["hours", "hour", "hr", "hrs", "h"],
                Duration::from_secs(7 * 3_600),
            ),
            (&["days", "day", "d"], Duration::from_secs(7 * 86_400)),
            (
                &["weeks", "week", "wk", "wks", "w"],
                Duration::from_secs(7 * 604_800),
            ),
            (
                &["months", "month", "M"],
                Duration::from_secs(7 * 2_630_016),
            ),
            (
                &["years", "year", "yr", "yrs", "y"],
                Duration::from_secs(7 * 31_557_600),
            ),
        ];
        for (aliases, expected) in cases {
            for alias in *aliases {
                assert_eq!(
                    parse_duration(&format!("7{alias}")).unwrap(),
                    *expected,
                    "{alias}"
                );
                assert_eq!(
                    parse_duration(&format!("7 {alias}")).unwrap(),
                    *expected,
                    "{alias}"
                );
            }
        }
    }

    #[test]
    fn ttl_duration_accepts_compound_fractional_and_spaced_forms() {
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert_eq!(parse_duration("0s").unwrap(), Duration::ZERO);
        assert_eq!(secs("1m30s"), 90);
        assert_eq!(secs("1m 30s"), 90);
        assert_eq!(secs("1 m 30 s"), 90);
        assert_eq!(secs("  45s\t"), 45);
        assert_eq!(secs("1hour 12min 5s"), 4_325);
        assert_eq!(secs("2h 37min"), 9_420);
        assert_eq!(secs("1.5m"), 90);
        assert_eq!(secs("0.5h"), 1_800);
        assert_eq!(secs("1.0s"), 1);
        assert_eq!(secs("90s 90s"), 180);
        assert_eq!(
            parse_duration("1.5s").unwrap(),
            Duration::from_millis(1_500)
        );
        assert_eq!(
            parse_duration("1.25ms").unwrap(),
            Duration::from_micros(1_250)
        );
        assert_eq!(parse_duration("0.001s").unwrap(), Duration::from_millis(1));
        assert_eq!(
            parse_duration("500ms 500ms").unwrap(),
            Duration::from_secs(1)
        );
        assert_eq!(
            parse_duration("2000000000ns").unwrap(),
            Duration::from_secs(2)
        );
        // humantime skipped whitespace inside a number; keep accepting it.
        assert_eq!(secs("1 0s"), 10);
        assert_eq!(
            parse_duration("1.2 5s").unwrap(),
            Duration::from_millis(1_250)
        );
    }

    #[test]
    fn ttl_duration_rejects_with_unchanged_messages() {
        assert_eq!(rejection(""), "value was empty");
        assert_eq!(rejection(" \t "), "value was empty");
        assert_eq!(rejection("not-a-duration"), "expected number at 0");
        assert_eq!(rejection("-5s"), "expected number at 0");
        assert_eq!(rejection("s"), "expected number at 0");
        assert_eq!(rejection("5s x"), "expected number at 3");
        assert_eq!(
            rejection("45"),
            "time unit needed, for example 45sec or 45ms"
        );
        assert_eq!(
            rejection("1.5"),
            "time unit needed, for example 1sec or 1ms"
        );
        assert_eq!(
            rejection("5x"),
            format!("unknown time unit \"x\", {UNKNOWN_UNIT_HINT}")
        );
        assert_eq!(
            rejection("5 sex"),
            format!("unknown time unit \"sex\", {UNKNOWN_UNIT_HINT}")
        );
        assert_eq!(
            rejection("5S"),
            format!("unknown time unit \"S\", {UNKNOWN_UNIT_HINT}")
        );
        assert_eq!(rejection("1."), "invalid character at 1");
        assert_eq!(rejection("1.s"), "invalid character at 1");
        assert_eq!(rejection("5s!"), "invalid character at 2");
        assert_eq!(rejection("1s,2s"), "invalid character at 2");
        assert_eq!(rejection("1m-30s"), "invalid character at 2");
        assert_eq!(rejection("5_000s"), "invalid character at 1");
        assert_eq!(rejection("1.2.3s"), "invalid character at 3");
    }

    #[test]
    fn ttl_duration_rejects_overflow_and_sub_nanosecond_precision() {
        const OVERFLOW: &str = "number is too large or cannot be represented without a lack \
                                of precision (values below 1ns are not supported)";
        // u64::MAX + 1 seconds.
        assert_eq!(rejection("18446744073709551616s"), OVERFLOW);
        assert_eq!(rejection("18446744073709551615m"), OVERFLOW);
        assert_eq!(rejection("18446744073709551615s 1s"), OVERFLOW);
        assert_eq!(rejection("1.5ns"), OVERFLOW);
        assert_eq!(rejection("0.0000000001s"), OVERFLOW);
        // Hour and longer fractions must be a whole number of seconds.
        assert_eq!(rejection("0.0001h"), OVERFLOW);
        // Twenty fractional digits exceed the u64 denominator.
        assert_eq!(rejection("0.00000000000000000001s"), OVERFLOW);
        assert_eq!(
            parse_duration("18446744073709551615s").unwrap(),
            Duration::from_secs(u64::MAX)
        );
    }

    #[test]
    fn ttl_duration_nanosecond_carry_overflow_is_an_error_not_a_panic() {
        // humantime 2.4.0 panicked in `Duration::new` when exactly one second of
        // nanoseconds carried into `u64::MAX` seconds, aborting `room create`.
        for input in [
            "18446744073709551615s 1000000000ns",
            "18446744073709551615s1000ms",
            "18446744073709551615s 0.5s 0.5s",
        ] {
            assert!(
                matches!(
                    parse_duration(input),
                    Err(super::DurationError::NumberOverflow)
                ),
                "{input:?}"
            );
        }
    }
}
