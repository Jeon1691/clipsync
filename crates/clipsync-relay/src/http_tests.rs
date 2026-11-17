// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use axum::extract::ConnectInfo;
use clipsync_protocol::{DeviceId, PairingSessionId, PROTOCOL_VERSION};
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
    let error = create_pairing(State(state.clone()), Json(request.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let offer = create_pairing(State(state.clone()), Json(request))
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
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
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
        Json(request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(joined.0.room_id, offer.room_id);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        Json(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(error.1 .0.code, "busy");
    for _ in 0..30 {
        state.record_ip_fail(address.ip());
    }
    let error = join_pairing(ConnectInfo(address), State(state), Json(request))
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
