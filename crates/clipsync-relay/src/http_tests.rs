// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use super::*;
use axum::extract::ConnectInfo;
use clipsync_protocol::{DeviceId, PairingSessionId, PROTOCOL_VERSION};
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
    let error = create_pairing(State(state.clone()), RequestJson(request.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let offer = create_pairing(State(state.clone()), RequestJson(request))
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
        RequestJson(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.1 .0.code, "protocol_version");
    request.protocol_version = PROTOCOL_VERSION;
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        RequestJson(request.clone()),
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
        RequestJson(request.clone()),
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
        RequestJson(request.clone()),
    )
    .await
    .unwrap();
    assert_eq!(joined.0.room_id, offer.room_id);
    let error = join_pairing(
        ConnectInfo(address),
        State(state.clone()),
        RequestJson(request.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(error.1 .0.code, "busy");
    for _ in 0..30 {
        state.record_ip_fail(address.ip());
    }
    let error = join_pairing(ConnectInfo(address), State(state), RequestJson(request))
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
