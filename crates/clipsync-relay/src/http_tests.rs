// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use axum::extract::ConnectInfo;
use clipsync_protocol::{DeviceId, PairingSessionId, PROTOCOL_VERSION};
use std::net::IpAddr;
use tokio::net::TcpListener;

/// Scrape credential used wherever a test enables `/metrics`.
const METRICS_TEST_TOKEN: &str = "relay-metrics-scrape-token-0123456789";

#[tokio::test]
async fn pairing_http_errors_preserve_protocol_expiry_busy_and_attempt_statuses() {
    let state = Arc::new(RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![31; 32],
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
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
        RequestJson(request.clone()),
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
        RequestJson(request),
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
        RequestJson(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(error.1 .0.code, "join_failed");
    request.device_id = DeviceId::new();
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        clipsync_protocol::Timestamp::now() - time::Duration::seconds(1);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::GONE);
    assert_eq!(error.1 .0.code, "code_expired");
    state.pairing_mut(sid.as_str()).unwrap().expires_at =
        clipsync_protocol::Timestamp::now() + time::Duration::seconds(60);
    let joined = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(joined.0.room_id, offer.room_id);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(request.clone()),
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
        RequestJson(request),
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
        metrics_token: Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap()),
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
        .bearer_auth(METRICS_TEST_TOKEN)
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

fn metrics_test_config(bind: SocketAddr) -> RelayConfig {
    RelayConfig {
        bind,
        public_url: format!("http://{bind}"),
        token_secret: vec![33; 32],
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    }
}

async fn spawn_router(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), task)
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

#[tokio::test]
async fn metrics_route_is_absent_unless_explicitly_configured() {
    let (url, task) =
        spawn_router(router(metrics_test_config("127.0.0.1:0".parse().unwrap()))).await;
    let client = loopback_client();
    let health = client.get(format!("{url}/healthz")).send().await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let metrics = client.get(format!("{url}/metrics")).send().await.unwrap();
    assert_eq!(metrics.status(), StatusCode::NOT_FOUND);
    task.abort();
}

#[tokio::test]
async fn enabled_metrics_reject_missing_malformed_and_wrong_bearer_tokens() {
    let mut config = metrics_test_config("127.0.0.1:0".parse().unwrap());
    config.metrics_token = Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap());
    let (url, task) = spawn_router(router(config)).await;
    let client = loopback_client();
    let wrong = format!("{METRICS_TEST_TOKEN}x");
    let attempts = [
        None,
        Some(format!("Bearer {wrong}")),
        Some(format!("Bearer {}", &METRICS_TEST_TOKEN[1..])),
        Some(format!("Basic {METRICS_TEST_TOKEN}")),
        Some(format!("Bearer  {METRICS_TEST_TOKEN}")),
        Some(format!("Bearer{METRICS_TEST_TOKEN}")),
        Some(METRICS_TEST_TOKEN.to_string()),
        Some("Bearer ".to_string()),
    ];
    for authorization in attempts {
        let mut request = client.get(format!("{url}/metrics"));
        if let Some(value) = &authorization {
            request = request.header(axum::http::header::AUTHORIZATION, value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{authorization:?} was accepted"
        );
        assert_eq!(
            response.headers()[axum::http::header::WWW_AUTHENTICATE],
            "Bearer realm=\"clipsync-relay-metrics\""
        );
        let body = response.text().await.unwrap();
        assert!(
            !body.contains("clipsync_"),
            "rejected scrape leaked metrics"
        );
    }
    task.abort();
}

#[tokio::test]
async fn enabled_metrics_serve_the_metrics_text_to_the_configured_bearer() {
    let mut config = metrics_test_config("127.0.0.1:0".parse().unwrap());
    config.metrics_token = Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap());
    let expected = RelayState::new(config.clone()).metrics_text();
    let (url, task) = spawn_router(router(config)).await;
    let client = loopback_client();
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let response = client
            .get(format!("{url}/metrics"))
            .header(
                axum::http::header::AUTHORIZATION,
                format!("{scheme} {METRICS_TEST_TOKEN}"),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), expected);
    }
    let health = client.get(format!("{url}/healthz")).send().await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    task.abort();
}

#[test]
fn metrics_tokens_require_header_safe_minimum_length_and_redact_debug_output() {
    let short = &METRICS_TEST_TOKEN[..MIN_METRICS_TOKEN_BYTES - 1];
    assert!(MetricsToken::new(short)
        .unwrap_err()
        .to_string()
        .contains("at least 32 bytes"));
    let exact = &METRICS_TEST_TOKEN[..MIN_METRICS_TOKEN_BYTES];
    assert!(MetricsToken::new(exact).unwrap().matches(exact.as_bytes()));
    for invalid in [
        format!("{METRICS_TEST_TOKEN} suffix"),
        format!("{METRICS_TEST_TOKEN}\n"),
        format!("{METRICS_TEST_TOKEN}\u{e9}"),
    ] {
        assert!(MetricsToken::new(&invalid)
            .unwrap_err()
            .to_string()
            .contains("visible ASCII"));
    }
    let token = MetricsToken::new(METRICS_TEST_TOKEN).unwrap();
    assert!(token.matches(METRICS_TEST_TOKEN.as_bytes()));
    assert!(!token.matches(&METRICS_TEST_TOKEN.as_bytes()[1..]));
    let debug = format!("{token:?}");
    assert!(!debug.contains(METRICS_TEST_TOKEN));
    assert_eq!(debug, "MetricsToken(<redacted>)");
}

fn join_test_state() -> Arc<RelayState> {
    Arc::new(RelayState::new(RelayConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        public_url: "http://127.0.0.1".into(),
        token_secret: vec![33; 32],
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    }))
}

async fn create_offer(state: &Arc<RelayState>) -> CreatePairingResponse {
    create_pairing(
        ConnectInfo("127.0.0.3:4000".parse().unwrap()),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(CreatePairingRequest {
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
            RequestJson(join_request(&code)),
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
        RequestJson(join_request(&offer.pairing_code)),
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
        RequestJson(join_request(code)),
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
                metrics_token: None,
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
        RequestJson(join_request("999999")),
    )
    .await
    .unwrap_err();
    let correct = join_pairing(
        ConnectInfo(fresh),
        State(state.clone()),
        HeaderMap::new(),
        RequestJson(join_request(&offer.pairing_code)),
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
                metrics_token: None,
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
        metrics_token: None,
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
        metrics_token: None,
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

async fn create_status(
    state: &Arc<RelayState>,
    peer: SocketAddr,
    headers: HeaderMap,
) -> Result<CreatePairingResponse, (StatusCode, String)> {
    create_pairing(
        ConnectInfo(peer),
        State(state.clone()),
        headers,
        RequestJson(CreatePairingRequest {
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
    // Far below what startup accepts, so open sessions alone fill it.
    const MAX_ROOMS: usize = 3;
    let proxy: SocketAddr = "127.0.0.1:4000".parse().unwrap();
    let state = Arc::new(RelayState::unvalidated(
        RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_url: "http://127.0.0.1".into(),
            token_secret: rand::random::<[u8; 32]>().to_vec(),
            webhook_url: None,
            webhook_secret: None,
            metrics_token: None,
        },
        vec![proxy.ip().into()],
        RelayResourceLimits {
            client_create_limit: CLIENT_LIMIT,
            max_rooms: MAX_ROOMS,
            ..Default::default()
        },
    ));
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

/// Serve the relay with `limits` and every peer trusted as a proxy, so tests
/// pick each connection's client address with `X-Forwarded-For`.
async fn serve_limited(limits: RelayResourceLimits) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let (address, server, _) = serve_limited_state(limits).await;
    (address, server)
}

/// [`serve_limited`], also returning the relay's state.
async fn serve_limited_state(
    limits: RelayResourceLimits,
) -> (SocketAddr, tokio::task::JoinHandle<()>, Arc<RelayState>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(
        RelayState::with_limits(
            RelayConfig {
                bind: address,
                public_url: format!("http://{address}"),
                token_secret: rand::random::<[u8; 32]>().to_vec(),
                webhook_url: None,
                webhook_secret: None,
                metrics_token: Some(MetricsToken::new(METRICS_TEST_TOKEN).unwrap()),
            },
            vec![address.ip().into()],
            PairingJoinLimits::default(),
            limits,
        )
        .unwrap(),
    );
    let app = router_with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (address, server, state)
}

/// A device-token `hello` for the owner of a freshly paired room.
fn member_hello(state: &RelayState) -> String {
    use clipsync_protocol::{ClientRole, ControlMessage};
    let owner = DeviceId::new();
    let sid = PairingSessionId::new();
    let client: IpAddr = "127.0.0.1".parse().unwrap();
    let offer = state
        .create_pairing(
            CreatePairingRequest {
                protocol_version: PROTOCOL_VERSION,
                pairing_session_id: sid.clone(),
                device_id: owner.clone(),
                device_name: "owner".into(),
                os: "test".into(),
                ttl_secs: None,
            },
            client,
        )
        .unwrap();
    state
        .join_pairing(
            JoinPairingRequest {
                protocol_version: PROTOCOL_VERSION,
                pairing_code: offer.pairing_code,
                device_id: DeviceId::new(),
                device_name: "member".into(),
                os: "test".into(),
            },
            client,
        )
        .unwrap();
    assert!(state.complete_pairing(sid.as_str()).is_none());
    let (owner_token, _, room_id, _, _) = state.complete_pairing(sid.as_str()).unwrap();
    ControlMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: owner,
        device_name: "owner".into(),
        os: "test".into(),
        role: ClientRole::Member,
        room_id: Some(room_id),
        token: owner_token,
        pairing_session_id: None,
    }
    .encode()
    .unwrap()
}

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a WebSocket as forwarded client `client`, or return the refusal status.
async fn connect_as(address: SocketAddr, client: &str) -> Result<WsClient, StatusCode> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Error as WsError;
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
}

/// Regression for unlimited WebSocket connections: the connection after a
/// client's cap, and after the relay-wide cap, is refused before the upgrade,
/// and a closed socket frees its slot.
#[tokio::test]
async fn websocket_connections_past_the_client_and_relay_caps_are_refused() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    const MAX_CONNECTIONS: usize = 3;
    const MAX_CLIENT_CONNECTIONS: usize = 2;

    tokio::time::timeout(Duration::from_secs(10), async {
        let (address, server) = serve_limited(RelayResourceLimits {
            max_connections: MAX_CONNECTIONS,
            max_client_connections: MAX_CLIENT_CONNECTIONS,
            ..Default::default()
        })
        .await;
        let mut first = Vec::new();
        for _ in 0..MAX_CLIENT_CONNECTIONS {
            first.push(connect_as(address, "192.0.2.1").await.unwrap());
        }
        assert_eq!(
            connect_as(address, "192.0.2.1").await.err(),
            Some(StatusCode::TOO_MANY_REQUESTS)
        );
        first.pop().unwrap().close(None).await.unwrap();
        let reopened = loop {
            match connect_as(address, "192.0.2.1").await {
                Ok(socket) => break socket,
                Err(StatusCode::TOO_MANY_REQUESTS) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(other) => panic!("unexpected status {other}"),
            }
        };
        drop(reopened);
        server.abort();

        // A one-slot relay held by a room member: the second connection, from
        // anyone, gets 503 (a pre-hello socket would be evicted instead).
        let (address, server, state) = serve_limited_state(RelayResourceLimits {
            max_connections: 1,
            max_client_connections: 1,
            ..Default::default()
        })
        .await;
        let mut held = connect_as(address, "192.0.2.1").await.unwrap();
        held.send(ClientMessage::Text(member_hello(&state).into()))
            .await
            .unwrap();
        match held.next().await {
            Some(Ok(ClientMessage::Text(text))) => assert!(
                matches!(
                    clipsync_protocol::ControlMessage::decode(&text).unwrap(),
                    clipsync_protocol::ControlMessage::HelloOk { .. }
                ),
                "{text}"
            ),
            other => panic!("expected hello_ok, got {other:?}"),
        }
        assert_eq!(
            connect_as(address, "192.0.2.2").await.err(),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        let metrics = reqwest::Client::new()
            .get(format!("http://{address}/metrics"))
            .bearer_auth(METRICS_TEST_TOKEN)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let refused = metrics
            .lines()
            .find_map(|line| line.strip_prefix("clipsync_ws_refused "))
            .unwrap();
        assert!(refused.parse::<u64>().unwrap() >= 1, "{metrics}");
        server.abort();
    })
    .await
    .unwrap();
}

/// Regression for pre-hello sockets holding every connection slot: once
/// unauthenticated sockets fill their share, a newcomer is admitted and the
/// oldest of them is closed, instead of the newcomer being refused.
#[tokio::test]
async fn a_newcomer_evicts_the_oldest_unauthenticated_socket_instead_of_being_refused() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    const MAX_CONNECTIONS: usize = 4;
    // 75% of four slots.
    const MAX_UNAUTHENTICATED: usize = 3;

    tokio::time::timeout(Duration::from_secs(10), async {
        let (address, server) = serve_limited(RelayResourceLimits {
            max_connections: MAX_CONNECTIONS,
            max_client_connections: 1,
            ..Default::default()
        })
        .await;
        let mut flood = Vec::new();
        for n in 0..MAX_UNAUTHENTICATED {
            flood.push(connect_as(address, &format!("192.0.2.{n}")).await.unwrap());
        }
        let _newcomer = connect_as(address, "203.0.113.1")
            .await
            .expect("a newcomer must not be refused while pre-hello sockets hold the slots");
        // The oldest flood socket is closed by the relay.
        loop {
            match flood[0].next().await {
                None | Some(Err(_)) | Some(Ok(ClientMessage::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
        server.abort();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn pairing_endpoints_reject_unsafe_wire_identifiers_with_bad_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(RelayConfig {
        bind: address,
        public_url: format!("http://{address}"),
        token_secret: vec![33; 32],
        webhook_url: None,
        webhook_secret: None,
        metrics_token: None,
    })
    .into_make_service_with_connect_info::<SocketAddr>();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let post = |path: &'static str, body: serde_json::Value| {
        let client = client.clone();
        async move {
            client
                .post(format!("http://{address}{path}"))
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let create = |sid: &str, device: &str| {
        serde_json::json!({
            "pairing_session_id": sid,
            "device_id": device,
            "device_name": "owner",
            "os": "test",
            "protocol_version": PROTOCOL_VERSION,
            "ttl_secs": null,
        })
    };
    let good_sid = PairingSessionId::new().to_string();
    let good_device = DeviceId::new().to_string();
    for (sid, device) in [
        (good_sid.as_str(), "../../outside"),
        (good_sid.as_str(), "dev\u{1b}[2Jice"),
        (good_sid.as_str(), ""),
        ("..\\session", good_device.as_str()),
    ] {
        let response = post("/v1/pairing/create", create(sid, device)).await;
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let body: ErrorBody = response.json().await.unwrap();
        assert_eq!(body.code, "invalid_request");
        assert!(!body.message.contains("outside") && !body.message.contains('\u{1b}'));
    }

    let offer: CreatePairingResponse = post(
        "/v1/pairing/create",
        create(good_sid.as_str(), good_device.as_str()),
    )
    .await
    .json()
    .await
    .unwrap();
    let join = |device: &str| {
        serde_json::json!({
            "pairing_code": offer.pairing_code,
            "device_id": device,
            "device_name": "joiner",
            "os": "test",
            "protocol_version": PROTOCOL_VERSION,
        })
    };
    // A missing content type keeps axum's own status rather than becoming 400.
    let untyped = client
        .post(format!("http://{address}/v1/pairing/create"))
        .body(create(good_sid.as_str(), good_device.as_str()).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        untyped.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let response = post("/v1/pairing/join", join("joiner/../../outside")).await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<ErrorBody>().await.unwrap().code,
        "invalid_request"
    );
    // The rejected join consumed neither the session nor an attempt.
    let joined = post("/v1/pairing/join", join(&DeviceId::new().to_string())).await;
    assert_eq!(joined.status(), reqwest::StatusCode::OK);
    let joined: JoinPairingResponse = joined.json().await.unwrap();
    assert_eq!(joined.room_id, offer.room_id);
    server.abort();
}

/// Regression for the failed-join budget around undecodable bodies. A join
/// whose body fails to decode (here an unsafe device ID beside the live code)
/// is answered with one fixed `400` before any code is looked up, so it tests
/// no code and is not charged. Guessing therefore still needs decodable joins,
/// which are charged, and once the budget is spent the correct code is refused
/// as well, whichever body carries it.
#[tokio::test]
async fn undecodable_joins_test_no_code_and_leave_the_join_budget_to_decodable_guesses() {
    let (url, task) =
        spawn_router(router(metrics_test_config("127.0.0.1:0".parse().unwrap()))).await;
    let client = loopback_client();
    let post = |path: &'static str, body: serde_json::Value| {
        let request = client.post(format!("{url}{path}")).json(&body);
        async move { request.send().await.unwrap() }
    };
    let offer: CreatePairingResponse = post(
        "/v1/pairing/create",
        serde_json::json!({
            "pairing_session_id": PairingSessionId::new(),
            "device_id": DeviceId::new(),
            "device_name": "owner",
            "os": "test",
            "protocol_version": PROTOCOL_VERSION,
            "ttl_secs": null,
        }),
    )
    .await
    .json()
    .await
    .unwrap();
    let join = |code: &str, device: &str| {
        serde_json::json!({
            "pairing_code": code,
            "device_id": device,
            "device_name": "joiner",
            "os": "test",
            "protocol_version": PROTOCOL_VERSION,
        })
    };
    let undecodable = || join(&offer.pairing_code, "../outside");
    let expect_invalid_request = |response: reqwest::Response| async move {
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let body: ErrorBody = response.json().await.unwrap();
        assert_eq!(body.code, "invalid_request");
        assert_eq!(body.message, "malformed request body");
    };

    // More undecodable joins than the client budget allows failures.
    for _ in 0..=IP_FAILED_JOIN_LIMIT {
        expect_invalid_request(post("/v1/pairing/join", undecodable()).await).await;
    }
    // The whole budget is still there for decodable guesses, which it bounds.
    let mut guesses = wrong_codes(&offer);
    for code in guesses.by_ref().take(IP_FAILED_JOIN_LIMIT as usize) {
        let device = DeviceId::new().to_string();
        let response = post("/v1/pairing/join", join(&code, &device)).await;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    let device = DeviceId::new().to_string();
    let next = guesses.next().unwrap();
    let response = post("/v1/pairing/join", join(&next, &device)).await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    // Spent budget: an undecodable body still learns nothing about the code,
    // and a decodable one with the correct code is refused.
    expect_invalid_request(post("/v1/pairing/join", undecodable()).await).await;
    let response = post("/v1/pairing/join", join(&offer.pairing_code, &device)).await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response.json::<ErrorBody>().await.unwrap().code,
        "too_many_attempts"
    );
    task.abort();
}

/// RFC 3339 exactly as the relay emits it: `YYYY-MM-DDTHH:MM:SS`, then either
/// nothing or `.` and 3, 6 or 9 digits whose last group of three is not all
/// zeros (the shortest exact spelling), then `Z`.
fn is_canonical_utc(value: &str) -> bool {
    const SECONDS_LEN: usize = "YYYY-MM-DDTHH:MM:SS".len();
    const FRACTION_GROUP: usize = 3;
    let Some(body) = value.strip_suffix('Z') else {
        return false;
    };
    if body.len() < SECONDS_LEN {
        return false;
    }
    let (seconds, fraction) = body.split_at(SECONDS_LEN);
    let digit_at = |i: usize| seconds.as_bytes()[i].is_ascii_digit();
    let separators_ok = seconds.as_bytes()[4] == b'-'
        && seconds.as_bytes()[7] == b'-'
        && seconds.as_bytes()[10] == b'T'
        && seconds.as_bytes()[13] == b':'
        && seconds.as_bytes()[16] == b':';
    let digits_ok = (0..SECONDS_LEN)
        .filter(|i| ![4, 7, 10, 13, 16].contains(i))
        .all(digit_at);
    let fraction_ok = match fraction.strip_prefix('.') {
        None => fraction.is_empty(),
        Some(digits) => {
            [3, 6, 9].contains(&digits.len())
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !digits[digits.len() - FRACTION_GROUP..]
                    .bytes()
                    .all(|b| b == b'0')
        }
    };
    separators_ok && digits_ok && fraction_ok
}

#[test]
fn canonical_utc_shape_check_matches_the_protocol_vectors() {
    for good in [
        "2023-11-14T22:13:20Z",
        "2023-11-14T22:13:20.100Z",
        "2023-11-14T22:13:20.123456Z",
        "2023-11-14T22:13:20.000000001Z",
    ] {
        assert!(is_canonical_utc(good), "{good}");
    }
    for bad in [
        "2023-11-14T22:13:20+00:00",
        "2023-11-14T22:13:20.000Z",
        "2023-11-14T22:13:20.123000Z",
        "2023-11-14T22:13:20.1Z",
        "2023-11-14 22:13:20Z",
        "2023-11-14T22:13:20z",
    ] {
        assert!(!is_canonical_utc(bad), "{bad}");
    }
}

/// The pairing responses carry `expires_at` in the canonical spelling, and
/// the joiner sees exactly the creator's string.
#[tokio::test]
async fn pairing_responses_emit_one_canonical_expiry_string() {
    let state = join_test_state();
    let (url, server) = spawn_router(router_with_state(state)).await;
    let client = loopback_client();
    let create: serde_json::Value = client
        .post(format!("{url}/v1/pairing/create"))
        .json(&CreatePairingRequest {
            protocol_version: PROTOCOL_VERSION,
            pairing_session_id: PairingSessionId::new(),
            device_id: DeviceId::new(),
            device_name: "owner".into(),
            os: "test".into(),
            ttl_secs: None,
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let created = create["expires_at"].as_str().unwrap();
    assert!(is_canonical_utc(created), "{created}");
    let join: serde_json::Value = client
        .post(format!("{url}/v1/pairing/join"))
        .json(&join_request(create["pairing_code"].as_str().unwrap()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(join["expires_at"].as_str(), Some(created), "{join}");
    server.abort();
}
