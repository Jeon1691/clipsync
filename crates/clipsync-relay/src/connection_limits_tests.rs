// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;

const MAX_TOTAL: usize = 3;
const MAX_PER_CLIENT: usize = 2;

#[test]
fn the_connection_after_a_clients_cap_is_refused_until_one_closes() {
    let counts = Arc::new(ConnectionCounts::default());
    let client: IpAddr = "198.51.100.40".parse().unwrap();
    let held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|_| counts.acquire(client, MAX_TOTAL, MAX_PER_CLIENT).unwrap())
        .collect();
    assert_eq!(
        counts.acquire(client, MAX_TOTAL, MAX_PER_CLIENT).err(),
        Some(ConnectionRefused::ClientFull)
    );
    // A refusal takes no slot.
    assert_eq!(counts.total(), MAX_PER_CLIENT);
    drop(held);
    assert_eq!(counts.total(), 0);
    assert_eq!(counts.clients(), 0, "closed clients leave no entry");
    counts.acquire(client, MAX_TOTAL, MAX_PER_CLIENT).unwrap();
}

#[test]
fn the_connection_after_the_relay_cap_is_refused_for_every_client() {
    let counts = Arc::new(ConnectionCounts::default());
    let held: Vec<_> = (0..MAX_TOTAL)
        .map(|n| {
            let client = IpAddr::from([198, 51, 100, 50 + n as u8]);
            counts.acquire(client, MAX_TOTAL, MAX_PER_CLIENT).unwrap()
        })
        .collect();
    let fresh: IpAddr = "203.0.113.9".parse().unwrap();
    assert_eq!(
        counts.acquire(fresh, MAX_TOTAL, MAX_PER_CLIENT).err(),
        Some(ConnectionRefused::RelayFull)
    );
    assert_eq!(counts.total(), MAX_TOTAL);
    assert_eq!(counts.clients(), MAX_TOTAL);
    drop(held);
    counts.acquire(fresh, MAX_TOTAL, MAX_PER_CLIENT).unwrap();
}

#[test]
fn ipv6_connections_share_a_cap_per_64_prefix() {
    let counts = Arc::new(ConnectionCounts::default());
    let _held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|n| {
            let ip = IpAddr::from([0x2001, 0xdb8, 0, 0x9, 0, 0, 0, n as u16 + 1]);
            counts.acquire(ip, MAX_TOTAL, MAX_PER_CLIENT).unwrap()
        })
        .collect();
    let same_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0x9, 0xffff, 0, 0, 1]);
    assert_eq!(
        counts.acquire(same_prefix, MAX_TOTAL, MAX_PER_CLIENT).err(),
        Some(ConnectionRefused::ClientFull)
    );
    let next_prefix = IpAddr::from([0x2001, 0xdb8, 0, 0xa, 0, 0, 0, 1]);
    counts
        .acquire(next_prefix, MAX_TOTAL, MAX_PER_CLIENT)
        .unwrap();
}

#[test]
fn a_client_at_its_cap_does_not_make_the_relay_look_full() {
    let counts = Arc::new(ConnectionCounts::default());
    let busy: IpAddr = "198.51.100.70".parse().unwrap();
    let _held: Vec<_> = (0..MAX_PER_CLIENT)
        .map(|_| counts.acquire(busy, MAX_TOTAL, MAX_PER_CLIENT).unwrap())
        .collect();
    for _ in 0..MAX_TOTAL {
        assert_eq!(
            counts.acquire(busy, MAX_TOTAL, MAX_PER_CLIENT).err(),
            Some(ConnectionRefused::ClientFull)
        );
    }
    assert_eq!(counts.total(), MAX_PER_CLIENT);
    counts
        .acquire("203.0.113.70".parse().unwrap(), MAX_TOTAL, MAX_PER_CLIENT)
        .unwrap();
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
                    if let Ok(slot) = counts.acquire(client, MAX_TOTAL, MAX_PER_CLIENT) {
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
