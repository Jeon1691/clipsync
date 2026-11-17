// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Temporary shim: characterization tests run against humantime first.

use std::time::Duration;

pub(crate) fn parse_duration(input: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(input)
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
}
