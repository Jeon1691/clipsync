// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[command(flatten)]
    limits: RelayResourceLimits,
}

fn parse(args: &[&str]) -> RelayResourceLimits {
    Args::try_parse_from(std::iter::once("clipsync-relay").chain(args.iter().copied()))
        .unwrap()
        .limits
}

#[test]
fn defaults_are_the_documented_limits_and_are_valid() {
    let defaults = RelayResourceLimits::default();
    assert_eq!(parse(&[]), defaults);
    assert_eq!(defaults.client_create_limit, 20);
    assert_eq!(defaults.client_create_window_secs, 15 * 60);
    assert_eq!(defaults.relay_create_limit, 100);
    assert_eq!(defaults.max_rooms, 10_000);
    assert_eq!(defaults.max_connections, 512);
    assert_eq!(defaults.max_client_connections, 32);
    defaults.validate().unwrap();
    assert_eq!(MAX_ACTIVE_PAIRING_SESSIONS, 1_000);
    assert_eq!(MAX_PAIRING_CREATE_RELAY_LIMIT, 500);
}

#[test]
fn every_limit_is_configurable_from_the_command_line() {
    let limits = parse(&[
        "--pairing-client-create-limit",
        "3",
        "--pairing-client-create-window-secs",
        "600",
        "--pairing-relay-create-limit",
        "7",
        "--max-rooms",
        "40",
        "--max-connections",
        "9",
        "--max-client-connections",
        "4",
    ]);
    assert_eq!(
        limits,
        RelayResourceLimits {
            client_create_limit: 3,
            client_create_window_secs: 600,
            relay_create_limit: 7,
            max_rooms: 40,
            max_connections: 9,
            max_client_connections: 4,
        }
    );
    limits.validate().unwrap();
}

#[test]
fn limits_that_cannot_work_are_refused_with_the_variable_name() {
    let defaults = RelayResourceLimits::default();
    for (limits, variable) in [
        (
            RelayResourceLimits {
                relay_create_limit: MAX_PAIRING_CREATE_RELAY_LIMIT + 1,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT",
        ),
        (
            RelayResourceLimits {
                relay_create_limit: 0,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_RELAY_CREATE_LIMIT",
        ),
        (
            RelayResourceLimits {
                client_create_limit: defaults.relay_create_limit + 1,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT",
        ),
        (
            RelayResourceLimits {
                client_create_limit: 0,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_LIMIT",
        ),
        (
            RelayResourceLimits {
                client_create_window_secs: PAIRING_TTL_SECS - 1,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS",
        ),
        (
            RelayResourceLimits {
                client_create_window_secs: MAX_PAIRING_CREATE_CLIENT_WINDOW_SECS + 1,
                ..defaults
            },
            "CLIPSYNC_RELAY_PAIRING_CLIENT_CREATE_WINDOW_SECS",
        ),
        (
            RelayResourceLimits {
                max_rooms: 0,
                ..defaults
            },
            "CLIPSYNC_RELAY_MAX_ROOMS",
        ),
        (
            RelayResourceLimits {
                max_connections: 0,
                max_client_connections: 0,
                ..defaults
            },
            "CLIPSYNC_RELAY_MAX_CONNECTIONS",
        ),
        (
            RelayResourceLimits {
                max_client_connections: defaults.max_connections + 1,
                ..defaults
            },
            "CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS",
        ),
        (
            RelayResourceLimits {
                max_client_connections: 0,
                ..defaults
            },
            "CLIPSYNC_RELAY_MAX_CLIENT_CONNECTIONS",
        ),
    ] {
        let error = limits.validate().unwrap_err().to_string();
        assert!(error.contains(variable), "{limits:?}: {error}");
    }
}
