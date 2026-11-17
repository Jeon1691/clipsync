// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[command(flatten)]
    limits: PairingJoinLimits,
}

fn parse(args: &[&str]) -> PairingJoinLimits {
    Args::try_parse_from(std::iter::once("clipsync-relay").chain(args.iter().copied()))
        .unwrap()
        .limits
}

#[test]
fn defaults_are_the_documented_budgets_and_are_valid() {
    let defaults = PairingJoinLimits::default();
    assert_eq!(parse(&[]), defaults);
    assert_eq!(defaults.client_fail_limit, 10);
    assert_eq!(defaults.client_fail_window_secs, 15 * 60);
    assert_eq!(defaults.relay_fail_limit, 20);
    defaults.validate().unwrap();
}

#[test]
fn relay_wide_limit_cannot_exceed_the_guessing_bound() {
    assert_eq!(MAX_GLOBAL_FAILED_JOIN_LIMIT, 50);
    let at_bound = parse(&["--pairing-relay-fail-limit", "50"]);
    at_bound.validate().unwrap();
    let over = parse(&["--pairing-relay-fail-limit", "51"]);
    let error = over.validate().unwrap_err().to_string();
    assert!(
        error.contains("CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT"),
        "{error}"
    );
    assert!(error.contains("1e-4"), "{error}");
    let zero = PairingJoinLimits {
        relay_fail_limit: 0,
        client_fail_limit: 0,
        ..Default::default()
    };
    assert!(zero.validate().is_err());
}

#[test]
fn client_budget_must_be_positive_and_within_the_relay_budget() {
    for client in [0, 21] {
        let limits = PairingJoinLimits {
            client_fail_limit: client,
            ..Default::default()
        };
        let error = limits.validate().unwrap_err().to_string();
        assert!(
            error.contains("CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_LIMIT"),
            "{error}"
        );
    }
    PairingJoinLimits {
        client_fail_limit: 20,
        ..Default::default()
    }
    .validate()
    .unwrap();
}

#[test]
fn client_window_must_cover_a_pairing_ttl_and_at_most_a_day() {
    for secs in [PAIRING_TTL_SECS - 1, MAX_IP_FAILED_JOIN_WINDOW_SECS + 1] {
        let limits = PairingJoinLimits {
            client_fail_window_secs: secs,
            ..Default::default()
        };
        let error = limits.validate().unwrap_err().to_string();
        assert!(
            error.contains("CLIPSYNC_RELAY_PAIRING_CLIENT_FAIL_WINDOW_SECS"),
            "{error}"
        );
    }
    for secs in [PAIRING_TTL_SECS, MAX_IP_FAILED_JOIN_WINDOW_SECS] {
        let limits = parse(&["--pairing-client-fail-window-secs", &secs.to_string()]);
        limits.validate().unwrap();
        assert_eq!(limits.client_fail_window(), Duration::from_secs(secs));
    }
}

#[test]
fn malformed_values_are_rejected_by_the_parser() {
    for args in [
        ["--pairing-client-fail-limit", "-1"],
        ["--pairing-relay-fail-limit", "many"],
        ["--pairing-client-fail-window-secs", "1.5"],
    ] {
        assert!(Args::try_parse_from(std::iter::once("clipsync-relay").chain(args)).is_err());
    }
}
