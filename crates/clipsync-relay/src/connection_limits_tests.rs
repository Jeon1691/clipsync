// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use crate::rate_limit::IPV6_SITE_CLIENT_SHARE;

const MAX_TOTAL: usize = 3;
const MAX_PER_CLIENT: usize = 2;
/// Caps where every slot may be unauthenticated, so nothing is evicted.
const LIMITS: ConnectionLimits = ConnectionLimits {
    max_total: MAX_TOTAL,
    max_unauthenticated: MAX_TOTAL,
    max_per_client: MAX_PER_CLIENT,
    max_per_site: MAX_TOTAL,
};

#[test]
fn the_connection_after_a_clients_cap_is_refused_until_one_closes() {
    let counts = Arc::new(ConnectionCounts::default());
    let client: IpAddr = "198.51.100.40".parse().unwrap();
    let held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|_| counts.acquire(client, LIMITS).unwrap())
        .collect();
    assert_eq!(
        counts.acquire(client, LIMITS).err(),
        Some(ConnectionRefused::ClientFull)
    );
    // A refusal takes no slot.
    assert_eq!(counts.total(), MAX_PER_CLIENT);
    drop(held);
    assert_eq!(counts.total(), 0);
    assert_eq!(counts.clients(), 0, "closed clients leave no entry");
    assert_eq!(counts.unauthenticated(), 0);
    counts.acquire(client, LIMITS).unwrap();
}

#[test]
fn the_connection_after_the_relay_cap_is_refused_for_every_client() {
    let counts = Arc::new(ConnectionCounts::default());
    let held: Vec<_> = (0..MAX_TOTAL)
        .map(|n| {
            let client = IpAddr::from([198, 51, 100, 50 + n as u8]);
            counts.acquire(client, LIMITS).unwrap()
        })
        .collect();
    let fresh: IpAddr = "203.0.113.9".parse().unwrap();
    assert_eq!(
        counts.acquire(fresh, LIMITS).err(),
        Some(ConnectionRefused::RelayFull)
    );
    assert_eq!(counts.total(), MAX_TOTAL);
    assert_eq!(counts.clients(), MAX_TOTAL);
    drop(held);
    counts.acquire(fresh, LIMITS).unwrap();
}

#[test]
fn ipv6_connections_share_a_cap_per_64_prefix() {
    let counts = Arc::new(ConnectionCounts::default());
    let _held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|n| {
            let ip = IpAddr::from([0x2001, 0xdb8, 0, 0x9, 0, 0, 0, n as u16 + 1]);
            counts.acquire(ip, LIMITS).unwrap()
        })
        .collect();
    let same_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0x9, 0xffff, 0, 0, 1]);
    assert_eq!(
        counts.acquire(same_prefix, LIMITS).err(),
        Some(ConnectionRefused::ClientFull)
    );
    let next_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0xa, 0, 0, 0, 1]);
    counts.acquire(next_prefix, LIMITS).unwrap();
}

/// One IPv6 site spreading over many /64s gets a few clients' worth of
/// connections, not one client's cap per /64.
#[test]
fn ipv6_connections_share_a_cap_per_48_site() {
    const CLIENTS: usize = 64;
    let limits = ConnectionLimits {
        max_total: CLIENTS * MAX_PER_CLIENT,
        max_unauthenticated: CLIENTS * MAX_PER_CLIENT,
        max_per_client: MAX_PER_CLIENT,
        max_per_site: crate::rate_limit::site_limit(MAX_PER_CLIENT, CLIENTS * MAX_PER_CLIENT),
    };
    let site_cap = MAX_PER_CLIENT * IPV6_SITE_CLIENT_SHARE;
    assert_eq!(limits.max_per_site, site_cap);
    let counts = Arc::new(ConnectionCounts::default());
    // Each connection from its own /64 inside 2001:db8:7::/48.
    let in_site = |subnet: u16| IpAddr::from([0x2001, 0xdb8, 0x7, subnet, 0, 0, 0, 1]);
    let _held: Vec<_> = (0..site_cap)
        .map(|n| counts.acquire(in_site(n as u16), limits).unwrap())
        .collect();
    assert_eq!(
        counts.acquire(in_site(u16::MAX), limits).err(),
        Some(ConnectionRefused::ClientFull)
    );
    assert_eq!(counts.total(), site_cap);
    let other_site = IpAddr::from([0x2001, 0xdb8, 0x8, 0, 0, 0, 0, 1]);
    counts.acquire(other_site, limits).unwrap();
    // IPv4 has no site: distinct addresses are distinct clients only.
    for n in 0..site_cap + 1 {
        counts
            .acquire(IpAddr::from([198, 51, 100, n as u8]), limits)
            .unwrap();
    }
}

#[test]
fn a_client_at_its_cap_does_not_make_the_relay_look_full() {
    let counts = Arc::new(ConnectionCounts::default());
    let busy: IpAddr = "198.51.100.70".parse().unwrap();
    let _held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|_| counts.acquire(busy, LIMITS).unwrap())
        .collect();
    for _ in 0..MAX_TOTAL {
        assert_eq!(
            counts.acquire(busy, LIMITS).err(),
            Some(ConnectionRefused::ClientFull)
        );
    }
    assert_eq!(counts.total(), MAX_PER_CLIENT);
    counts
        .acquire("203.0.113.70".parse().unwrap(), LIMITS)
        .unwrap();
}

/// Sockets that never authenticate cannot take every slot: past their share
/// the oldest one is evicted for each newcomer, while authenticated sockets
/// keep theirs and the relay-wide cap still holds.
#[test]
fn unauthenticated_sockets_past_their_share_evict_the_oldest_one() {
    const MAX_UNAUTHENTICATED: usize = 3;
    let limits = ConnectionLimits {
        max_total: 4,
        max_unauthenticated: MAX_UNAUTHENTICATED,
        max_per_client: 1,
        max_per_site: 1,
    };
    let counts = Arc::new(ConnectionCounts::default());
    let client = |n: u8| IpAddr::from([198, 51, 100, n]);
    let device = counts.acquire(client(0), limits).unwrap();
    assert!(device.authenticate());
    let flood: Vec<_> = (1..=MAX_UNAUTHENTICATED as u8)
        .map(|n| counts.acquire(client(n), limits).unwrap())
        .collect();
    assert_eq!(counts.total(), 4);

    // At the relay-wide cap nothing is evicted: there is no slot to hand over.
    assert_eq!(
        counts.acquire(client(9), limits).err(),
        Some(ConnectionRefused::RelayFull)
    );
    drop(device);
    let newcomer = counts.acquire(client(10), limits).unwrap();
    assert_eq!(counts.unauthenticated(), MAX_UNAUTHENTICATED);
    assert_eq!(counts.total(), 4);
    // The oldest flood socket was told to close; it cannot authenticate any more.
    let evicted = &flood[0];
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(1), evicted.evicted())
                .await
                .expect("the oldest unauthenticated socket was not evicted");
        });
    assert!(!evicted.authenticate());
    assert!(flood[1].authenticate(), "a newer socket was evicted");
    assert!(newcomer.authenticate());
    drop(flood);
    drop(newcomer);
    assert_eq!(counts.total(), 0);
    assert_eq!(counts.unauthenticated(), 0);
    assert_eq!(counts.clients(), 0);
}

#[test]
fn concurrent_acquires_never_exceed_either_cap() {
    const THREADS: usize = 16;
    const ATTEMPTS: usize = 64;
    const CLIENTS: u8 = 4;
    let counts = Arc::new(ConnectionCounts::default());
    let slots = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for thread in 0..THREADS {
            let (counts, slots) = (&counts, &slots);
            scope.spawn(move || {
                for attempt in 0..ATTEMPTS {
                    let client =
                        IpAddr::from([192, 0, 2, ((thread + attempt) % CLIENTS as usize) as u8]);
                    if let Ok(slot) = counts.acquire(client, LIMITS) {
                        slots.lock().unwrap().push((client, slot));
                    }
                }
            });
        }
    });
    let slots = slots.into_inner().unwrap();
    assert_eq!(slots.len(), MAX_TOTAL);
    assert_eq!(counts.total(), MAX_TOTAL);
    for (client, _) in &slots {
        let held = slots.iter().filter(|(other, _)| other == client).count();
        assert!(held <= MAX_PER_CLIENT, "{client} holds {held}");
    }
    drop(slots);
    assert_eq!(counts.total(), 0);
    assert_eq!(counts.clients(), 0);
}
