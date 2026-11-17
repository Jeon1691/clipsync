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
        "2000",
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
            max_rooms: 2000,
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
        // Every room could have a device online, leaving none to evict.
        (
            RelayResourceLimits {
                max_rooms: defaults.max_connections + MAX_ACTIVE_PAIRING_SESSIONS,
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

#[test]
fn the_smallest_valid_room_cap_leaves_a_room_to_evict() {
    let defaults = RelayResourceLimits::default();
    RelayResourceLimits {
        max_rooms: defaults.max_connections + MAX_ACTIVE_PAIRING_SESSIONS + 1,
        ..defaults
    }
    .validate()
    .unwrap();
}

#[test]
fn derived_limits_keep_a_reserve_for_authenticated_sockets_and_group_ipv6_sites() {
    let defaults = RelayResourceLimits::default();
    let connections = defaults.connection_limits();
    assert_eq!(connections.max_total, MAX_CONNECTIONS);
    assert_eq!(connections.max_unauthenticated, 384);
    assert_eq!(connections.max_per_client, MAX_CLIENT_CONNECTIONS);
    assert_eq!(connections.max_per_site, 96);
    assert_eq!(defaults.create_budget().site_limit, Some(60));
    // The per-site allowance never exceeds the relay-wide one.
    let tight = RelayResourceLimits {
        client_create_limit: PAIRING_CREATE_RELAY_LIMIT,
        max_client_connections: MAX_CONNECTIONS,
        ..defaults
    };
    assert_eq!(
        tight.create_budget().site_limit,
        Some(PAIRING_CREATE_RELAY_LIMIT)
    );
    assert_eq!(tight.connection_limits().max_per_site, MAX_CONNECTIONS);
    // A one-slot relay still admits one unauthenticated socket.
    let single = RelayResourceLimits {
        max_connections: 1,
        max_client_connections: 1,
        ..defaults
    };
    assert_eq!(single.connection_limits().max_unauthenticated, 1);
    // Evictions at the relay-wide cap always have room to close.
    assert_eq!(single.connection_limits().max_eviction_overshoot, 1);
    assert_eq!(defaults.connection_limits().max_eviction_overshoot, 25);
}
