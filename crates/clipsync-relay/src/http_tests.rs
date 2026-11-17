// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use axum::extract::ConnectInfo;
use clipsync_protocol::{DeviceId, PairingSessionId, PROTOCOL_VERSION};
use std::net::IpAddr;
use tokio::net::TcpListener;

#[tokio::test]
async fn pairing_http_errors_preserve_protocol_expiry_busy_and_attempt_statuses() {
    let state = Arc::new(RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![31; 32],
        webhook_url: None,
        webhook_secret: None,
    }));
    let owner = DeviceId::new();
    let sid = PairingSessionId::new();
    let mut request = CreatePairingRequest {
        protocol_version: 0,
        pairing_session_id: sid.clone(),
        device_id: owner.clone(),
        device_name: "owner".into(),
        os: "test".into(),
        ttl_secs: None,
    };
    let owner_address: SocketAddr = "127.0.0.3:4000".parse().unwrap();
    let error = create_pairing(
        ConnectInfo(owner_address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let offer = create_pairing(
        ConnectInfo(owner_address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request),
    )
    .await
    .unwrap()
    .0;
    let address: SocketAddr = "127.0.0.2:4000".parse().unwrap();
    let mut request = JoinPairingRequest {
        protocol_version: 0,
        pairing_code: offer.pairing_code.clone(),
        device_id: owner,
        device_name: "joiner".into(),
        os: "test".into(),
    };
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(error.1 .0.code, "join_failed");
    request.device_id = DeviceId::new();
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        chrono::Utc::now() - chrono::Duration::seconds(1);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::GONE);
    assert_eq!(error.1 .0.code, "code_expired");
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(60);
    let joined = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(joined.0.room_id, offer.room_id);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(error.1 .0.code, "busy");
    for _ in 0..IP_FAILED_JOIN_LIMIT {
        state.record_ip_fail(address.ip());
    }
    let error = join_pairing(
        ConnectInfo(address),
        State(state),
        HeaderMap::new(),
        Json(request),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.1 .0.code, "too_many_attempts");
    assert!(serde_json::to_string(&error.1 .0)
        .unwrap()
        .contains("pairing attempt limit"));
}

#[tokio::test]
async fn server_reports_bind_failure_and_serves_health_and_metrics_on_a_local_listener() {
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = occupied.local_addr().unwrap();
    let config = |bind| RelayConfig {
        bind,
        public_url: format!("http://{bind}"),
        token_secret: vec![32; 32],
        webhook_url: None,
        webhook_secret: None,
    };
    let transport = || RelayTransportConfig {
        allow_http: true,
        ..Default::default()
    };
    let error = serve_with_transport(config(address), transport())
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::AddrInUse
    );
    drop(occupied);
    let task = tokio::spawn(serve_with_transport(config(address), transport()));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let health = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = client.get(format!("http://{address}/healthz")).send().await {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        health.json::<serde_json::Value>().await.unwrap()["service"],
        "clipsync-relay"
    );
    let metrics = client
        .get(format!("http://{address}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics
        .lines()
        .any(|line| line == "clipsync_ws_connections 0"));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

fn join_test_state() -> Arc<RelayState> {
    Arc::new(RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![33; 32],
        webhook_url: None,
        webhook_secret: None,
    }))
}

async fn create_offer(state: &Arc<RelayState>) -> CreatePairingResponse {
    create_pairing(
        ConnectInfo("127.0.0.3:4000".parse().unwrap()),
        State(state.clone()),
        HeaderMap::new(),
        Json(CreatePairingRequest {
            protocol_version: PROTOCOL_VERSION,
            pairing_session_id: PairingSessionId::new(),
            device_id: DeviceId::new(),
            device_name: "owner".into(),
            os: "test".into(),
            ttl_secs: None,
        }),
    )
    .await
    .unwrap()
    .0
}

fn join_request(code: &str) -> JoinPairingRequest {
    JoinPairingRequest {
        protocol_version: PROTOCOL_VERSION,
        pairing_code: code.into(),
        device_id: DeviceId::new(),
        device_name: "joiner".into(),
        os: "test".into(),
    }
}

/// Regression for unbounded pairing-code guessing: a single client sweeping
/// wrong codes must be throttled, and must not then be able to join with the
/// correct code.
#[tokio::test]
async fn sweeping_wrong_codes_from_one_ip_is_throttled_and_blocks_the_correct_code() {
    const SWEEP: u32 = 20_000;
    let state = join_test_state();
    let offer = create_offer(&state).await;
    let attacker: SocketAddr = "198.51.100.7:4000".parse().unwrap();
    let (mut not_found, mut throttled) = (0u32, 0u32);
    for n in 0..SWEEP {
        let code = format!("{n:06}");
        if code == offer.pairing_code {
            continue;
        }
        let error = join_pairing(
            ConnectInfo(attacker),
            State(state.clone()),
            HeaderMap::new(),
            Json(join_request(&code)),
        )
        .await
        .unwrap_err();
        match error.0 {
            StatusCode::NOT_FOUND => not_found += 1,
            StatusCode::TOO_MANY_REQUESTS => throttled += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(throttled > 0, "wrong-code sweep was never throttled");
    assert!(not_found < 100, "{not_found} guesses were evaluated");
    let error = join_pairing(
        ConnectInfo(attacker),
        State(state.clone()),
        HeaderMap::new(),
        Json(join_request(&offer.pairing_code)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error.1 .0.code, "too_many_attempts");
}

fn forwarded_for(client: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", client.parse().unwrap());
    headers
}

async fn join_status(
    state: &Arc<RelayState>,
    peer: SocketAddr,
    headers: HeaderMap,
    code: &str,
) -> StatusCode {
    match join_pairing(
        ConnectInfo(peer),
        State(state.clone()),
        headers,
        Json(join_request(code)),
    )
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(error) => error.0,
    }
}

/// Wrong codes that never collide with `offer`.
fn wrong_codes(offer: &CreatePairingResponse) -> impl Iterator<Item = String> + '_ {
    (0..1_000_000u32)
        .map(|n| format!("{n:06}"))
        .filter(move |code| *code != offer.pairing_code)
}

#[tokio::test]
async fn forwarded_for_is_honored_only_from_configured_trusted_proxies() {
    let proxy: SocketAddr = "127.0.0.1:4000".parse().unwrap();
    // Without configuration the header is ignored: a direct client cannot
    // rotate spoofed addresses, and clients behind a proxy share its budget.
    // Each run stops at the per-client limit, below the relay-wide budget, so
    // a header that was wrongly honored would show up as extra 404s.
    let state = join_test_state();
    let offer = create_offer(&state).await;
    let mut not_found = 0;
    for (n, code) in wrong_codes(&offer).take(50).enumerate() {
        let spoofed = forwarded_for(&format!("192.0.2.{n}"));
        match join_status(&state, proxy, spoofed, &code).await {
            StatusCode::NOT_FOUND => not_found += 1,
            StatusCode::TOO_MANY_REQUESTS => break,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(not_found, IP_FAILED_JOIN_LIMIT);
    assert_eq!(
        join_status(
            &state,
            proxy,
            forwarded_for("203.0.113.5"),
            &offer.pairing_code
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // With the proxy trusted, each forwarded client has its own budget and a
    // client-forged entry to the left of the proxy's is ignored.
    let state = Arc::new(
        RelayState::with_join_policy(
            RelayConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                public_url: "http://127.0.0.1".into(),
                token_secret: vec![34; 32],
                webhook_url: None,
                webhook_secret: None,
            },
            vec!["127.0.0.0/8".parse().unwrap()],
            PairingJoinLimits::default(),
        )
        .unwrap(),
    );
    let offer = create_offer(&state).await;
    let mut not_found = 0;
    for (n, code) in wrong_codes(&offer).take(50).enumerate() {
        let forged = forwarded_for(&format!("192.0.2.{n}, 198.51.100.9"));
        match join_status(&state, proxy, forged, &code).await {
            StatusCode::NOT_FOUND => not_found += 1,
            StatusCode::TOO_MANY_REQUESTS => break,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(
        not_found, IP_FAILED_JOIN_LIMIT,
        "forged leftmost entries evaded the client limit"
    );
    assert_eq!(
        join_status(
            &state,
            proxy,
            forwarded_for("203.0.113.5"),
            &offer.pairing_code
        )
        .await,
        StatusCode::OK
    );
    // An untrusted peer cannot use the header even when proxies are configured.
    let direct: SocketAddr = "198.51.100.9:4000".parse().unwrap();
    assert_eq!(
        join_status(&state, direct, forwarded_for("203.0.113.6"), "000000").await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn exhausted_relay_wide_budget_answers_unknown_and_correct_codes_identically() {
    let state = join_test_state();
    let offer = create_offer(&state).await;
    let mut refusals = 0;
    for (n, code) in wrong_codes(&offer).take(1_000).enumerate() {
        let client: SocketAddr = format!("10.{}.{}.1:4000", n / 256, n % 256)
            .parse()
            .unwrap();
        if join_status(&state, client, HeaderMap::new(), &code).await
            == StatusCode::TOO_MANY_REQUESTS
        {
            refusals += 1;
        }
    }
    assert!(refusals > 0, "distributed guesses were never throttled");
    let fresh: SocketAddr = "192.0.2.250:4000".parse().unwrap();
    let unknown = join_pairing(
        ConnectInfo(fresh),
        State(state.clone()),
        HeaderMap::new(),
        Json(join_request("999999")),
    )
    .await
    .unwrap_err();
    let correct = join_pairing(
        ConnectInfo(fresh),
        State(state.clone()),
        HeaderMap::new(),
        Json(join_request(&offer.pairing_code)),
    )
    .await
    .unwrap_err();
    assert_eq!(unknown.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        serde_json::to_string(&unknown.1 .0).unwrap(),
        serde_json::to_string(&correct.1 .0).unwrap()
    );
}

/// The production Compose manifest, so the test follows its real settings.
const PROXMOX_COMPOSE: &str = include_str!("../../../deploy/proxmox/docker-compose.yml");

/// The value of the first `key: value` line in `manifest`, unquoted.
fn manifest_value<'a>(manifest: &'a str, key: &str) -> &'a str {
    manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix(':'))
        .unwrap_or_else(|| panic!("{key} missing from the deploy manifest"))
        .trim()
        .trim_matches('"')
}

/// Regression for one shared pairing budget in the production deployment:
/// cloudflared reaches the loopback-published port, so the relay sees every
/// visitor as the Compose network gateway. The manifest must trust exactly that
/// gateway, so the visitor address Cloudflare appends to `X-Forwarded-For`
/// identifies the client and one guesser cannot lock out everyone else.
#[tokio::test]
async fn proxmox_cloudflare_tunnel_chain_gives_each_visitor_its_own_join_budget() {
    let gateway: IpAddr = manifest_value(PROXMOX_COMPOSE, "gateway").parse().unwrap();
    let subnet: ipnet::IpNet = manifest_value(PROXMOX_COMPOSE, "- subnet").parse().unwrap();
    assert!(
        subnet.contains(&gateway),
        "gateway outside the pinned subnet"
    );
    let trusted: Vec<ipnet::IpNet> =
        manifest_value(PROXMOX_COMPOSE, "CLIPSYNC_RELAY_TRUSTED_PROXIES")
            .split(',')
            .map(|entry| client_ip::parse_trusted_proxy(entry).unwrap())
            .collect();
    assert!(
        trusted.iter().any(|net| net.contains(&gateway)),
        "the deploy manifest does not trust its own network gateway"
    );
    let state = Arc::new(
        RelayState::with_join_policy(
            RelayConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                public_url: "http://127.0.0.1".into(),
                token_secret: rand::random::<[u8; 32]>().to_vec(),
                webhook_url: None,
                webhook_secret: None,
            },
            trusted,
            PairingJoinLimits::default(),
        )
        .unwrap(),
    );
    let offer = create_offer(&state).await;
    let tunnel = SocketAddr::new(gateway, 41_000);
    // Cloudflare keeps whatever the visitor sent and appends the address it
    // received the request from; cloudflared forwards the header unchanged.
    let mut not_found = 0;
    for (n, code) in wrong_codes(&offer).take(50).enumerate() {
        let edge = forwarded_for(&format!("192.0.2.{n}, 198.51.100.20"));
        match join_status(&state, tunnel, edge, &code).await {
            StatusCode::NOT_FOUND => not_found += 1,
            StatusCode::TOO_MANY_REQUESTS => break,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(not_found, IP_FAILED_JOIN_LIMIT, "guesser was not limited");
    assert_eq!(
        join_status(
            &state,
            tunnel,
            forwarded_for("203.0.113.20"),
            &offer.pairing_code
        )
        .await,
        StatusCode::OK,
        "a different visitor was locked out by the guesser"
    );
}

#[tokio::test]
async fn invalid_join_limits_stop_the_relay_from_starting() {
    let config = RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: rand::random::<[u8; 32]>().to_vec(),
        webhook_url: None,
        webhook_secret: None,
    };
    let transport = RelayTransportConfig {
        allow_http: true,
        join_limits: PairingJoinLimits {
            relay_fail_limit: join_limits::MAX_GLOBAL_FAILED_JOIN_LIMIT + 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = serve_with_transport(config.clone(), transport.clone())
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("CLIPSYNC_RELAY_PAIRING_RELAY_FAIL_LIMIT"));
    assert!(router_with_join_policy(config, Vec::new(), transport.join_limits).is_err());
}

#[tokio::test]
async fn invalid_resource_limits_stop_the_relay_from_starting() {
    let config = RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: rand::random::<[u8; 32]>().to_vec(),
        webhook_url: None,
        webhook_secret: None,
    };
    let transport = RelayTransportConfig {
        allow_http: true,
        resource_limits: RelayResourceLimits {
            max_connections: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = serve_with_transport(config.clone(), transport.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("CLIPSYNC_RELAY_MAX_CONNECTIONS"));
    assert!(router_with_limits(
        config,
        Vec::new(),
        PairingJoinLimits::default(),
        transport.resource_limits
    )
    .is_err());
}

fn resource_limited_state(proxy: IpAddr, limits: RelayResourceLimits) -> Arc<RelayState> {
    Arc::new(
        RelayState::with_limits(
            RelayConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                public_url: "http://127.0.0.1".into(),
                token_secret: rand::random::<[u8; 32]>().to_vec(),
                webhook_url: None,
                webhook_secret: None,
            },
            vec![proxy.into()],
            PairingJoinLimits::default(),
            limits,
        )
        .unwrap(),
    )
}

async fn create_status(
    state: &Arc<RelayState>,
    peer: SocketAddr,
    headers: HeaderMap,
) -> Result<CreatePairingResponse, (StatusCode, String)> {
    create_pairing(
        ConnectInfo(peer),
        State(state.clone()),
        headers,
        Json(CreatePairingRequest {
            protocol_version: PROTOCOL_VERSION,
            pairing_session_id: PairingSessionId::new(),
            device_id: DeviceId::new(),
            device_name: "owner".into(),
            os: "test".into(),
            ttl_secs: None,
        }),
    )
    .await
    .map(|created| created.0)
    .map_err(|(status, body)| (status, body.0.code))
}

/// Pairing creation is limited per forwarded client (with the same client
/// resolution as joins), and a full relay answers 503, not a pairing error.
#[tokio::test]
async fn pairing_create_answers_429_per_client_and_503_when_the_relay_is_full() {
    const CLIENT_LIMIT: u32 = 2;
    const MAX_ROOMS: usize = 3;
    let proxy: SocketAddr = "127.0.0.1:4000".parse().unwrap();
    let state = resource_limited_state(
        proxy.ip(),
        RelayResourceLimits {
            client_create_limit: CLIENT_LIMIT,
            max_rooms: MAX_ROOMS,
            ..Default::default()
        },
    );
    for _ in 0..CLIENT_LIMIT {
        create_status(&state, proxy, forwarded_for("192.0.2.1"))
            .await
            .unwrap();
    }
    assert_eq!(
        create_status(&state, proxy, forwarded_for("192.0.2.1"))
            .await
            .unwrap_err(),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_pairings".into())
    );
    create_status(&state, proxy, forwarded_for("192.0.2.2"))
        .await
        .unwrap();
    assert_eq!(
        create_status(&state, proxy, forwarded_for("192.0.2.3"))
            .await
            .unwrap_err(),
        (StatusCode::SERVICE_UNAVAILABLE, "relay_full".into())
    );
    let metrics = state.metrics_text();
    for line in [
        "clipsync_pairing_create_throttled 1",
        "clipsync_pairing_create_full 1",
        "clipsync_pairing_sessions 3",
    ] {
        assert!(metrics.lines().any(|l| l == line), "{line} in {metrics}");
    }
}

/// Regression for unlimited WebSocket connections: the connection after a
/// client's cap, and after the relay-wide cap, is refused before the upgrade,
/// and a closed socket frees its slot.
#[tokio::test]
async fn websocket_connections_past_the_client_and_relay_caps_are_refused() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Error as WsError;
    const MAX_CONNECTIONS: usize = 3;
    const MAX_CLIENT_CONNECTIONS: usize = 2;

    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router_with_limits(
            RelayConfig {
                bind: address,
                public_url: format!("http://{address}"),
                token_secret: rand::random::<[u8; 32]>().to_vec(),
                webhook_url: None,
                webhook_secret: None,
            },
            vec![address.ip().into()],
            PairingJoinLimits::default(),
            RelayResourceLimits {
                max_connections: MAX_CONNECTIONS,
                max_client_connections: MAX_CLIENT_CONNECTIONS,
                ..Default::default()
            },
        )
        .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let connect = |client: &'static str| async move {
            let mut request = format!("ws://{address}/v1/ws")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("x-forwarded-for", client.parse().unwrap());
            match tokio_tungstenite::connect_async(request).await {
                Ok((socket, _)) => Ok(socket),
                Err(WsError::Http(response)) => Err(response.status()),
                Err(other) => panic!("unexpected WebSocket error {other}"),
            }
        };

        let mut first = Vec::new();
        for _ in 0..MAX_CLIENT_CONNECTIONS {
            first.push(connect("192.0.2.1").await.unwrap());
        }
        assert_eq!(
            connect("192.0.2.1").await.err(),
            Some(StatusCode::TOO_MANY_REQUESTS)
        );
        let _second = connect("192.0.2.2").await.unwrap();
        assert_eq!(
            connect("192.0.2.3").await.err(),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );

        first.pop().unwrap().close(None).await.unwrap();
        let reopened = loop {
            match connect("192.0.2.1").await {
                Ok(socket) => break socket,
                Err(StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(other) => panic!("unexpected status {other}"),
            }
        };
        drop(reopened);
        let metrics = reqwest::get(format!("http://{address}/metrics"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let refused = metrics
            .lines()
            .find_map(|line| line.strip_prefix("clipsync_ws_refused "))
            .unwrap();
        assert!(refused.parse::<u64>().unwrap() >= 2, "{metrics}");
        server.abort();
    })
    .await
    .unwrap();
}
